//! Postgres adapter for `AssetsRepository`.
//!
//! Stores events in the `asset_events` table as append-only rows.
//! The `kind` column carries the discriminator for SQL-level queries;
//! `payload` carries the full JSONB of the event variant (including the
//! kind tag) for lossless round-trip serialization back to
//! `AssetEventKind`.
//!
//! `append` also maintains the `assets` projection table in the same
//! transaction by re-projecting the serial's full event history. This
//! is the cost of having a queryable current-state view; the alternative
//! (recomputing on every read) doesn't scale to the per-account and
//! per-sku queries the rest of the asset needs.

use async_trait::async_trait;
use sqlx::PgPool;

use std::collections::HashSet;
use std::time::Instant;

use crate::port::{
    AssetsError, AssetsRepository, BatchAppendStats, model_named,
    refuse_an_actor_that_reads_back_as_another, refuse_negative_page, refuse_nul_in_event,
};
use crate::project::{TicketOp, apply_event, project};
use crate::types::{
    AssetCurrentState, AssetEvent, AssetEventId, AssetEventKind, AssetId, AssetLifecyclePhase,
    AssetsSummary, PhaseRollup, SkuRollup,
};

pub struct PgAssets {
    pool: PgPool,
}

impl PgAssets {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Total event count = inserted + duplicates; used purely for the
/// phase-timing tracing line below. Pulled into a helper so the log
/// statement doesn't have to duplicate the arithmetic.
fn events_len_for_log(inserted: u64, duplicates: u64) -> u64 {
    inserted + duplicates
}

/// Extract the serde tag value from a `AssetEventKind` for the `kind` column.
fn kind_tag(kind: &AssetEventKind) -> &'static str {
    match kind {
        AssetEventKind::Registered { .. } => "Registered",
        AssetEventKind::Identified { .. } => "Identified",
        AssetEventKind::Received { .. } => "Received",
        AssetEventKind::PutAway { .. } => "PutAway",
        AssetEventKind::PartReplaced { .. } => "PartReplaced",
        AssetEventKind::Sold { .. } => "Sold",
        AssetEventKind::Shipped { .. } => "Shipped",
        AssetEventKind::Installed { .. } => "Installed",
        AssetEventKind::WarrantyStarted { .. } => "WarrantyStarted",
        AssetEventKind::WarrantyExpired => "WarrantyExpired",
        AssetEventKind::OwnershipTransferred { .. } => "OwnershipTransferred",
        AssetEventKind::ServiceJobOpened { .. } => "ServiceJobOpened",
        AssetEventKind::ServiceJobClosed { .. } => "ServiceJobClosed",
        AssetEventKind::WarrantyClaimed { .. } => "WarrantyClaimed",
        AssetEventKind::Decommissioned { .. } => "Decommissioned",
    }
}

#[async_trait]
impl AssetsRepository for PgAssets {
    async fn append(&self, event: AssetEvent) -> Result<(), AssetsError> {
        refuse_nul_in_event(&event)?;
        refuse_an_actor_that_reads_back_as_another(&event)?;
        let kind = kind_tag(&event.kind);
        let payload =
            serde_json::to_value(&event.kind).map_err(|e| AssetsError::Storage(e.to_string()))?;
        let serial = event.asset_id.clone();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO asset_events (id, asset_id, ts, actor_id, kind, payload)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(&event.id.0)
        .bind(&event.asset_id.0)
        .bind(event.ts)
        .bind(event.actor_id.to_string())
        .bind(kind)
        .bind(&payload)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                AssetsError::DuplicateEvent(event.id.0.clone())
            } else {
                AssetsError::Storage(e.to_string())
            }
        })?;
        // Judged after the insert, so a redelivery is a duplicate
        // whatever it names; the refusal drops the transaction.
        refuse_unknown_models(&mut tx, model_named(&event.kind).into_iter().collect()).await?;

        // Read existing projection state. This is one indexed lookup,
        // not a full event-log scan.
        let existing = fetch_system_state(&mut tx, &serial).await?;

        if sorts_before_the_projection(
            &mut tx,
            existing.as_ref(),
            &event,
            std::slice::from_ref(&event.id.0),
        )
        .await?
        {
            // Out-of-order arrival: full reprojection from the serial's
            // complete event log (which already includes the row we
            // just inserted). full_reproject_system does the upsert
            // and the open-tickets rebuild itself.
            full_reproject_system(&mut tx, &serial).await?;
        } else {
            // Fast path: read just the open ticket ids and apply the
            // new event incrementally. Order matters: upsert the
            // asset row first, then mutate asset_open_tickets
            // (which has an FK to assets(asset_id)).
            let open_ids = fetch_open_ticket_ids(&mut tx, &serial).await?;
            let (state, ticket_op) = apply_event(&serial, existing.as_ref(), &open_ids, &event);
            upsert_system(&mut tx, &state).await?;
            apply_ticket_op_to_table(&mut tx, &serial, &ticket_op).await?;
        }

        tx.commit()
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;

        Ok(())
    }

    #[tracing::instrument(skip(self, events), fields(n = events.len()))]
    async fn batch_append(&self, events: Vec<AssetEvent>) -> Result<BatchAppendStats, AssetsError> {
        if events.is_empty() {
            return Ok(BatchAppendStats::default());
        }

        // Per-phase timing: instrumentation for the replay-throughput-decay
        // investigation (TODO "Residual replay throughput decay"). We log
        // the phase breakdown on every batch; over a long replay, grep
        // `journalctl -u boss-assets-api | grep batch_phase` and watch
        // whether phase_1_insert_ms grows faster than phase_2_project_ms
        // as the asset_events table grows. The hypothesis is btree index
        // amplification on phase 1; if phase 2 grows faster, the culprit
        // is instead full-reproject frequency or projection-table bloat.
        let t_batch_start = Instant::now();
        // All or nothing (backlog be459ab9): every refusal is judged
        // before the transaction commits, the NUL before any SQL.
        for event in &events {
            refuse_nul_in_event(event)?;
            refuse_an_actor_that_reads_back_as_another(event)?;
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;

        // First, bulk insert into asset_events with ON CONFLICT DO
        // NOTHING. The RETURNING id gives us the ids that actually
        // got inserted (i.e., not duplicates), which is what the
        // projection updates need to apply.
        //
        // Multi-row INSERT collapses N round-trips into one. The
        // bound parameters per row stay below the postgres 65535-bind
        // limit even at large batch sizes (6 binds * 10000 rows is
        // fine).
        let mut sql = String::from(
            "INSERT INTO asset_events (id, asset_id, ts, actor_id, kind, payload) VALUES ",
        );
        let mut bind_idx = 1usize;
        for i in 0..events.len() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${}, ${})",
                bind_idx,
                bind_idx + 1,
                bind_idx + 2,
                bind_idx + 3,
                bind_idx + 4,
                bind_idx + 5
            ));
            bind_idx += 6;
        }
        sql.push_str(" ON CONFLICT (id) DO NOTHING RETURNING id");

        let mut q = sqlx::query_as::<_, (String,)>(&sql);
        for event in &events {
            let payload = serde_json::to_value(&event.kind)
                .map_err(|e| AssetsError::Storage(e.to_string()))?;
            q = q
                .bind(&event.id.0)
                .bind(&event.asset_id.0)
                .bind(event.ts)
                .bind(event.actor_id.to_string())
                .bind(kind_tag(&event.kind))
                .bind(payload);
        }
        let inserted_ids: std::collections::HashSet<String> = q
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?
            .into_iter()
            .map(|(id,)| id)
            .collect();

        let phase_1_insert_ms = t_batch_start.elapsed().as_millis() as u64;
        let events_len = events.len() as u64;
        let t_phase_2 = Instant::now();

        let inserted_count = inserted_ids.len() as u64;
        let duplicate_count = events_len - inserted_count;

        // Then keep only the events that actually got inserted,
        // group them by serial, and update each serial's projection
        // by walking its inserted events in chronological order.
        // Within a serial we apply incrementally if all of the new
        // events are >= the existing last_event_at; otherwise we
        // fall back to a full reprojection of that serial.
        //
        // Only the FIRST copy of an id is the one stored: a batch may
        // carry one id twice, and `ON CONFLICT DO NOTHING` keeps the
        // first while RETURNING names the id once. Until backlog
        // be459ab9 both copies were applied to the projection.
        let mut unapplied = inserted_ids.clone();
        let stored: Vec<AssetEvent> = events
            .into_iter()
            .filter(|e| unapplied.remove(&e.id.0))
            .collect();
        refuse_unknown_models(
            &mut tx,
            stored.iter().filter_map(|e| model_named(&e.kind)).collect(),
        )
        .await?;
        let mut by_serial: std::collections::BTreeMap<String, Vec<AssetEvent>> =
            std::collections::BTreeMap::new();
        for e in stored {
            by_serial.entry(e.asset_id.0.clone()).or_default().push(e);
        }

        let serials_touched = by_serial.len() as u64;
        let mut full_reproject_count: u64 = 0;

        for (serial_str, mut new_events) in by_serial {
            new_events.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.0.cmp(&b.id.0)));
            let serial = AssetId::new(serial_str);

            let existing = fetch_system_state(&mut tx, &serial).await?;
            let new_ids: Vec<String> = new_events.iter().map(|e| e.id.0.clone()).collect();
            let needs_full_reproject = match new_events.first() {
                Some(earliest) => {
                    sorts_before_the_projection(&mut tx, existing.as_ref(), earliest, &new_ids)
                        .await?
                }
                None => false,
            };

            if needs_full_reproject {
                full_reproject_count += 1;
                full_reproject_system(&mut tx, &serial).await?;
                continue;
            }

            // Fast path: walk new events in order, applying each.
            let mut state = existing;
            let mut open_ids = fetch_open_ticket_ids(&mut tx, &serial).await?;
            for event in &new_events {
                let (next, ticket_op) = apply_event(&serial, state.as_ref(), &open_ids, event);
                // Mirror the ticket op into the in-memory open_ids set
                // so the next event in this batch sees the latest set
                // without re-querying the table.
                match &ticket_op {
                    TicketOp::Open { ticket_id, .. } => {
                        open_ids.insert(ticket_id.clone());
                    }
                    TicketOp::Close { ticket_id } => {
                        open_ids.remove(ticket_id);
                    }
                    TicketOp::ClearAll | TicketOp::Noop => {}
                }
                upsert_system(&mut tx, &next).await?;
                apply_ticket_op_to_table(&mut tx, &serial, &ticket_op).await?;
                state = Some(next);
            }
        }

        let phase_2_project_ms = t_phase_2.elapsed().as_millis() as u64;
        let t_commit = Instant::now();

        tx.commit()
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;

        let phase_3_commit_ms = t_commit.elapsed().as_millis() as u64;
        let total_ms = t_batch_start.elapsed().as_millis() as u64;

        tracing::info!(
            target: "batch_phase",
            events = events_len_for_log(inserted_count, duplicate_count),
            inserted = inserted_count,
            duplicates = duplicate_count,
            serials_touched = serials_touched,
            full_reprojects = full_reproject_count,
            phase_1_insert_ms,
            phase_2_project_ms,
            phase_3_commit_ms,
            total_ms,
            "batch_append phase timings"
        );

        Ok(BatchAppendStats {
            inserted: inserted_count,
            duplicates: duplicate_count,
        })
    }

    async fn events_for(&self, serial: &AssetId) -> Result<Vec<AssetEvent>, AssetsError> {
        if holds_nul(&serial.0) {
            return Ok(Vec::new());
        }
        // `id COLLATE "C"`: one day's events in BYTE order, the order
        // `project` folds them in; the locale's order until backlog
        // be459ab9.
        let rows: Vec<EventRow> = sqlx::query_as(
            r#"
            SELECT id, asset_id, ts, actor_id, payload
            FROM asset_events
            WHERE asset_id = $1
            ORDER BY ts ASC, id COLLATE "C" ASC
            "#,
        )
        .bind(&serial.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AssetsError::Storage(e.to_string()))?;

        // The rebuild's decode: a retired kind is skipped and logged, a
        // known kind that will not decode is still an error (8d5ac7c5).
        rows.into_iter()
            .filter_map(|r| r.into_replayable_event().transpose())
            .collect::<Result<Vec<_>, _>>()
    }

    async fn current_state(
        &self,
        serial: &AssetId,
    ) -> Result<Option<AssetCurrentState>, AssetsError> {
        let events = self.events_for(serial).await?;
        Ok(project(serial, &events))
    }

    async fn all_asset_ids(&self) -> Result<Vec<AssetId>, AssetsError> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT DISTINCT asset_id FROM asset_events ORDER BY asset_id")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| AssetsError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|(s,)| AssetId::new(s)).collect())
    }

    async fn list_asset_ids(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<AssetId>, i64), AssetsError> {
        refuse_negative_page(limit, offset)?;
        let (total,): (i64,) = sqlx::query_as("SELECT count(DISTINCT asset_id) FROM asset_events")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;

        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT asset_id COLLATE \"C\" FROM asset_events \
             ORDER BY 1 LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AssetsError::Storage(e.to_string()))?;

        Ok((
            rows.into_iter().map(|(s,)| AssetId::new(s)).collect(),
            total,
        ))
    }

    async fn list_assets(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<AssetCurrentState>, i64), AssetsError> {
        // Account filter is optional; when set, both queries gain a
        // account filter = the typed pair (holder_kind='account');
        // the brewery's location-held equipment never matches an
        // account scope, which is the honest answer.
        refuse_negative_page(limit, offset)?;
        if account_id.is_some_and(holds_nul) {
            return Ok((Vec::new(), 0));
        }
        let (total,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM assets WHERE ($1::text IS NULL OR (holder_kind = 'account' AND holder_id = $1))",
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| AssetsError::Storage(e.to_string()))?;

        let rows: Vec<SystemRow> = sqlx::query_as(
            "SELECT asset_id, sku, phase, holder_kind, holder_id, warranty_through, \
             open_ticket_count, first_seen, last_event_at, oem_serial \
             FROM assets \
             WHERE ($1::text IS NULL OR (holder_kind = 'account' AND holder_id = $1)) \
             ORDER BY last_event_at DESC, asset_id COLLATE \"C\" LIMIT $2 OFFSET $3",
        )
        .bind(account_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AssetsError::Storage(e.to_string()))?;

        Ok((
            rows.into_iter().map(|r| r.into_current_state()).collect(),
            total,
        ))
    }

    async fn open_ticket_count_for_account(&self, account_id: &str) -> Result<u64, AssetsError> {
        if holds_nul(account_id) {
            return Ok(0);
        }
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM asset_open_tickets t \
             JOIN assets d ON d.asset_id = t.asset_id \
             WHERE d.holder_kind = 'account' AND d.holder_id = $1 \
               AND d.phase <> 'decommissioned'",
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| AssetsError::Storage(e.to_string()))?;
        Ok(count.max(0) as u64)
    }

    async fn active_asset_count_for_sku(&self, sku: &str) -> Result<u64, AssetsError> {
        if holds_nul(sku) {
            return Ok(0);
        }
        // "Active" means any phase except decommissioned. Assets just
        // received, in stock, or installed all
        // count — if a model has active devices, deleting the model
        // would orphan them.
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM assets WHERE sku = $1 AND phase <> 'decommissioned'",
        )
        .bind(sku)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| AssetsError::Storage(e.to_string()))?;
        Ok(count.max(0) as u64)
    }

    async fn assets_summary(&self, today: chrono::NaiveDate) -> Result<AssetsSummary, AssetsError> {
        // Phase distribution. The CASE expression pins the output order
        // to the pipeline sequence so the kanban renders left-to-right
        // without re-sorting on the client.
        let phase_rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT phase, COUNT(*)::bigint FROM assets GROUP BY phase")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| AssetsError::Storage(e.to_string()))?;
        let phase_map: std::collections::HashMap<String, i64> = phase_rows.into_iter().collect();
        let phase_counts: Vec<PhaseRollup> = AssetLifecyclePhase::ORDER
            .iter()
            .map(|p| PhaseRollup {
                phase: p.to_string(),
                count: phase_map.get(*p).copied().unwrap_or(0),
            })
            .collect();
        let total_systems: i64 = phase_counts.iter().map(|p| p.count).sum();
        let in_field_count: i64 = phase_counts
            .iter()
            .filter(|p| p.phase != "decommissioned")
            .map(|p| p.count)
            .sum();

        // Only count tickets on devices that are still in operation.
        // Retired devices keep their historical open_ticket_count for
        // the projection contract but we don't want those surfacing
        // as "work to do today" on the Assets list header.
        let open_tickets_total: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(open_ticket_count), 0)::bigint \
             FROM assets WHERE phase <> 'decommissioned'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| AssetsError::Storage(e.to_string()))?;

        // Top SKUs by active device count. The Assets list header shows
        // a "model mix" line so a handful is enough — return all 20 and
        // let the client truncate if it wants. An unidentified asset
        // has no model (its NULL sku failed this whole summary until
        // backlog be459ab9); a tie reads in byte order of sku.
        let sku_rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT sku, COUNT(*)::bigint \
             FROM assets \
             WHERE phase <> 'decommissioned' AND sku IS NOT NULL \
             GROUP BY sku \
             ORDER BY COUNT(*) DESC, sku COLLATE \"C\"",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AssetsError::Storage(e.to_string()))?;
        let sku_counts: Vec<SkuRollup> = sku_rows
            .into_iter()
            .map(|(sku, count)| SkuRollup { sku, count })
            .collect();

        let warranty_expiring_30d: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM assets \
             WHERE warranty_through IS NOT NULL \
               AND warranty_through >= $1::date \
               AND warranty_through < $1::date + INTERVAL '30 days'",
        )
        .bind(today)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| AssetsError::Storage(e.to_string()))?;

        Ok(AssetsSummary {
            phase_counts,
            total_systems,
            in_field_count,
            open_tickets_total,
            sku_counts,
            warranty_expiring_30d,
        })
    }
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct EventRow {
    id: String,
    asset_id: String,
    ts: chrono::NaiveDate,
    actor_id: Option<String>,
    payload: serde_json::Value,
}

impl EventRow {
    /// Decode for a REPLAY (a full reprojection of a serial) or a READ
    /// (`events_for`, and `current_state` through it): a row
    /// whose `kind` names no variant this build knows — a retired kind,
    /// such as the refurb pipeline's TriageCompleted / RefurbStarted /
    /// RefurbCompleted / QaPassed (backlog a8991c86) — is `Ok(None)`, to
    /// be skipped and counted by the caller, and logged here with its id.
    /// A row of a KNOWN kind whose payload will not decode is still an
    /// error: that is corruption, not history this build outgrew.
    ///
    /// Until backlog df6aedb4 one such row failed the whole `assets`
    /// step of `boss-rebuild-all`, and with it every caller that gates
    /// on rebuild-all. `asset_events` is a no-fact store, so the
    /// migration that retired those kinds could not measure it. The read
    /// path decoded strictly until backlog 8d5ac7c5, so the same serial
    /// rebuilt cleanly and then answered its every read with an error;
    /// the two now agree, and there is one decode.
    ///
    /// "Unknown" is serde's own verdict, read off the one error it gives
    /// for an unmatched tag (`unknown variant `<tag>``) and matched
    /// against THIS row's tag, so a nested enum field that fails inside
    /// a known kind is not mistaken for a retired kind.
    fn into_replayable_event(mut self) -> Result<Option<AssetEvent>, AssetsError> {
        let tag = self
            .payload
            .get("kind")
            .and_then(|k| k.as_str())
            .map(str::to_owned);
        match serde_json::from_value::<AssetEventKind>(std::mem::take(&mut self.payload)) {
            Ok(kind) => Ok(Some(self.with_kind(kind))),
            Err(e)
                if tag.as_deref().is_some_and(|tag| {
                    e.to_string()
                        .starts_with(&format!("unknown variant `{tag}`"))
                }) =>
            {
                tracing::warn!(
                    event_id = %self.id,
                    asset_id = %self.asset_id,
                    kind = tag.as_deref().unwrap_or_default(),
                    "asset_events row of a kind this build does not know: skipped"
                );
                Ok(None)
            }
            Err(e) => Err(AssetsError::Storage(format!("bad event payload: {e}"))),
        }
    }

    /// The envelope around an already-decoded kind.
    fn with_kind(self, kind: AssetEventKind) -> AssetEvent {
        AssetEvent {
            id: AssetEventId::new(self.id),
            asset_id: AssetId::new(self.asset_id),
            ts: self.ts,
            // A non-NULL string is parsed by ActorId's FromStr
            // (`automation:<slug>` → Automation, a bare `asset` →
            // automation:platform, everything else = a bare employee id
            // = Human). A NULL actor_id decodes to the named `platform`
            // automation — an unattributed row is never anonymous.
            // Both fallbacks are defensive; a clean v1.1.0 regen
            // produces neither shape.
            actor_id: self
                .actor_id
                .as_deref()
                .map(|s| {
                    s.parse().unwrap_or_else(|_| {
                        boss_core::actor::ActorId::Automation("platform".into())
                    })
                })
                .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into())),
            kind,
        }
    }
}

#[derive(sqlx::FromRow)]
struct SystemRow {
    asset_id: String,
    sku: Option<String>,
    phase: String,
    holder_kind: Option<String>,
    holder_id: Option<String>,
    warranty_through: Option<chrono::NaiveDate>,
    open_ticket_count: i32,
    first_seen: chrono::NaiveDate,
    last_event_at: chrono::NaiveDate,
    oem_serial: Option<String>,
}

impl SystemRow {
    fn into_current_state(self) -> AssetCurrentState {
        AssetCurrentState {
            asset_id: AssetId::new(self.asset_id),
            sku: self.sku,
            // Phase is a free-text Class code; the column stores the
            // kebab string directly, so the newtype wraps it as-is.
            phase: AssetLifecyclePhase::new(self.phase),
            holder_kind: self.holder_kind,
            holder_id: self.holder_id,
            warranty_through: self.warranty_through,
            open_ticket_count: self.open_ticket_count as u32,
            first_seen: self.first_seen,
            last_event_at: self.last_event_at,
            oem_serial: self.oem_serial,
        }
    }
}

/// Read the projection row for a serial. None if no row exists yet.
async fn fetch_system_state(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    serial: &AssetId,
) -> Result<Option<AssetCurrentState>, AssetsError> {
    let row: Option<SystemRow> = sqlx::query_as(
        "SELECT asset_id, sku, phase, holder_kind, holder_id, warranty_through, \
                open_ticket_count, first_seen, last_event_at, oem_serial \
         FROM assets WHERE asset_id = $1",
    )
    .bind(&serial.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| AssetsError::Storage(e.to_string()))?;
    Ok(row.map(|r| r.into_current_state()))
}

/// Read just the open ticket ids for a serial. The set, not full
/// rows, is what `apply_event` needs to decide whether a Close is a
/// no-op.
async fn fetch_open_ticket_ids(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    serial: &AssetId,
) -> Result<HashSet<String>, AssetsError> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT ticket_id FROM asset_open_tickets WHERE asset_id = $1")
            .bind(&serial.0)
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Apply a single `TicketOp` against `asset_open_tickets`. Each op is
/// idempotent at the table layer (an INSERT…ON CONFLICT for Open, a
/// DELETE…WHERE for Close, a clear-all DELETE for Decommission).
async fn apply_ticket_op_to_table(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    serial: &AssetId,
    op: &TicketOp,
) -> Result<(), AssetsError> {
    match op {
        TicketOp::Noop => Ok(()),
        TicketOp::Open {
            ticket_id,
            summary,
            opened_on,
        } => {
            sqlx::query(
                "INSERT INTO asset_open_tickets \
                    (ticket_id, asset_id, summary, opened_on) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (asset_id, ticket_id) DO NOTHING",
            )
            .bind(ticket_id)
            .bind(&serial.0)
            .bind(summary)
            .bind(opened_on)
            .execute(&mut **tx)
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;
            Ok(())
        }
        TicketOp::Close { ticket_id } => {
            // By BOTH keys: a ticket id is the caller's and may be open on
            // another asset too, whose row this close must not take
            // (backlog b8099caf).
            sqlx::query("DELETE FROM asset_open_tickets WHERE asset_id = $1 AND ticket_id = $2")
                .bind(&serial.0)
                .bind(ticket_id)
                .execute(&mut **tx)
                .await
                .map_err(|e| AssetsError::Storage(e.to_string()))?;
            Ok(())
        }
        TicketOp::ClearAll => {
            sqlx::query("DELETE FROM asset_open_tickets WHERE asset_id = $1")
                .bind(&serial.0)
                .execute(&mut **tx)
                .await
                .map_err(|e| AssetsError::Storage(e.to_string()))?;
            Ok(())
        }
    }
}

/// Slow-path full reprojection: read every event for the serial, run
/// `project()`, upsert the projection row, and rebuild the
/// `asset_open_tickets` rows for this serial — all within the
/// caller's transaction. Returns the new state for downstream use.
///
/// Used by the append out-of-order fallback and by the bulk
/// `rebuild_projection` recovery path.
async fn full_reproject_system(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    serial: &AssetId,
) -> Result<Reprojected, AssetsError> {
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT id, asset_id, ts, actor_id, payload \
         FROM asset_events WHERE asset_id = $1 \
         ORDER BY ts ASC, id COLLATE \"C\" ASC",
    )
    .bind(&serial.0)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| AssetsError::Storage(e.to_string()))?;

    let mut events: Vec<AssetEvent> = Vec::with_capacity(rows.len());
    let mut events_skipped = 0u64;
    for row in rows {
        match row.into_replayable_event()? {
            Some(event) => events.push(event),
            None => events_skipped += 1,
        }
    }
    let events_processed = events.len() as u64;
    let Some(state) = project(serial, &events) else {
        return Ok(Reprojected {
            state: None,
            events_processed,
            events_skipped,
        });
    };

    // Order matters because asset_open_tickets FKs to assets(asset_id):
    // upsert the projection row first, THEN rebuild the open tickets.
    upsert_system(tx, &state).await?;

    sqlx::query("DELETE FROM asset_open_tickets WHERE asset_id = $1")
        .bind(&serial.0)
        .execute(&mut **tx)
        .await
        .map_err(|e| AssetsError::Storage(e.to_string()))?;

    {
        // Walk sorted events, track each open ticket's summary +
        // opened_on, and insert one row per ticket id still open at
        // the moment we either run out of events or hit the first
        // Decommissioned. Decommissioned devices DO retain their
        // historical open-ticket rows, matching the proptest
        // contract that the count is "opened without ever closed".
        let mut sorted: Vec<&AssetEvent> = events.iter().collect();
        sorted.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.0.cmp(&b.id.0)));
        let mut open: std::collections::HashMap<String, (String, chrono::NaiveDate)> =
            std::collections::HashMap::new();
        for e in sorted {
            match &e.kind {
                AssetEventKind::ServiceJobOpened { job_id, summary } => {
                    open.insert(job_id.clone(), (summary.clone(), e.ts));
                }
                AssetEventKind::ServiceJobClosed { job_id, .. } => {
                    open.remove(job_id);
                }
                AssetEventKind::Decommissioned { .. } => {
                    // Stop walking but preserve the still-open set —
                    // they're historical "never closed before
                    // retirement" rows, kept for parity with the
                    // projection_properties contract.
                    break;
                }
                _ => {}
            }
        }
        for (ticket_id, (summary, opened_on)) in open {
            sqlx::query(
                "INSERT INTO asset_open_tickets \
                    (ticket_id, asset_id, summary, opened_on) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (asset_id, ticket_id) DO NOTHING",
            )
            .bind(&ticket_id)
            .bind(&serial.0)
            .bind(&summary)
            .bind(opened_on)
            .execute(&mut **tx)
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;
        }
    }

    Ok(Reprojected {
        state: Some(state),
        events_processed,
        events_skipped,
    })
}

/// One serial's full reprojection: the state it landed at (None when no
/// event this build can read exists for it) and the rows it folded or
/// skipped.
struct Reprojected {
    state: Option<AssetCurrentState>,
    events_processed: u64,
    events_skipped: u64,
}

/// UPSERT a device row from a projected current-state. Used by both
/// the per-append projection write and the bulk rebuild path.
async fn upsert_system(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    state: &AssetCurrentState,
) -> Result<(), AssetsError> {
    // Identity write-through (subject-model R1, Q1): the asset's
    // identity row rides the same transaction as its current-state
    // upsert (identity-first — a Registered event with only an id
    // still lands an identity).
    boss_subject_kinds::subjects::record_subject_in_tx(tx, "asset", &state.asset_id.0, None)
        .await
        .map_err(AssetsError::Storage)?;
    // `sku` is nullable: an identity-first asset is `Registered`
    // before it is identified, so `None` is a valid projection state
    // (it binds SQL NULL). When `Some`, `assets.sku` FKs to
    // `asset_models`, so a non-null sku must name a real catalog
    // model — Postgres enforces that.
    sqlx::query(
        r#"
        INSERT INTO assets
            (asset_id, sku, phase, holder_kind, holder_id, warranty_through,
             open_ticket_count, first_seen, last_event_at, oem_serial, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW())
        ON CONFLICT (asset_id) DO UPDATE SET
            sku = EXCLUDED.sku,
            phase = EXCLUDED.phase,
            holder_kind = EXCLUDED.holder_kind,
            holder_id = EXCLUDED.holder_id,
            warranty_through = EXCLUDED.warranty_through,
            open_ticket_count = EXCLUDED.open_ticket_count,
            first_seen = EXCLUDED.first_seen,
            last_event_at = EXCLUDED.last_event_at,
            oem_serial = EXCLUDED.oem_serial,
            updated_at = NOW()
        "#,
    )
    .bind(&state.asset_id.0)
    .bind(&state.sku)
    .bind(state.phase.as_str())
    .bind(&state.holder_kind)
    .bind(&state.holder_id)
    .bind(state.warranty_through)
    .bind(state.open_ticket_count as i32)
    .bind(state.first_seen)
    .bind(state.last_event_at)
    .bind(&state.oem_serial)
    .execute(&mut **tx)
    .await
    .map_err(|e| AssetsError::Storage(e.to_string()))?;
    Ok(())
}

/// Advisory-lock key for the `assets` rebuild, derived from the
/// projection name like every other rebuilder's.
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("assets");

impl PgAssets {
    /// One-shot rebuild of the `assets` projection table (and
    /// `asset_open_tickets`) from the `asset_events` log. Walks distinct
    /// serials, projects each, and upserts the result. Reports the rows
    /// written and the events folded or skipped.
    ///
    /// Run as the `assets` step of `boss-rebuild-all` (it was the `boss
    /// assets rebuild-projection` CLI verb until backlog 05cd6572), to
    /// recover a stale or empty projection from the log.
    /// Idempotent: running it on a healthy DB is a no-op-equivalent
    /// because the upserts produce identical state.
    pub async fn rebuild_projection(&self) -> Result<AssetsRebuildReport, AssetsError> {
        // One transaction under the per-service advisory lock, as every
        // other rebuilder in `boss-rebuild-all` holds one — so two
        // rebuilds of this projection never interleave. Until backlog
        // df6aedb4 this committed serial by serial and took no lock,
        // while rebuild-all's header said every step was locked.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(REBUILD_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;

        let serials: Vec<(String,)> =
            sqlx::query_as("SELECT DISTINCT asset_id FROM asset_events ORDER BY asset_id")
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| AssetsError::Storage(e.to_string()))?;

        let mut report = AssetsRebuildReport::default();
        for (serial_str,) in serials {
            let serial = AssetId::new(serial_str);
            // full_reproject_system upserts both `assets` and
            // `asset_open_tickets` for this serial in the same
            // transaction, so a rebuild keeps both consistent.
            let reprojected = full_reproject_system(&mut tx, &serial).await?;
            report.events_processed += reprojected.events_processed;
            report.events_skipped += reprojected.events_skipped;
            if reprojected.state.is_some() {
                report.assets_written += 1;
            }
        }
        tx.commit()
            .await
            .map_err(|e| AssetsError::Storage(e.to_string()))?;
        Ok(report)
    }
}

/// What the `assets` step of `boss-rebuild-all` did. The field names
/// are the ones rebuild-all's tally scrapes off every report's Debug
/// (`events_processed`, `events_skipped`), so a skipped row is counted
/// in the run's closing line rather than only here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetsRebuildReport {
    /// Serials whose `assets` row (and open tickets) were re-projected.
    pub assets_written: u64,
    /// `asset_events` rows decoded and folded into a projection.
    pub events_processed: u64,
    /// Rows whose `kind` this build no longer knows — a retired kind —
    /// left out of the projection and counted here.
    pub events_skipped: u64,
}

/// A key holding a NUL byte names nothing TEXT can hold, so a read keyed
/// by one is the miss it is — Postgres refused the bind with its
/// encoding error, a 500, until backlog be459ab9.
fn holds_nul(key: &str) -> bool {
    key.contains('\0')
}

/// Refuse the first of `skus` (in the order given) that names no
/// catalog model, `UnknownModel`, before the projection's foreign key
/// would answer it as `Storage` (backlog be459ab9).
async fn refuse_unknown_models(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    skus: Vec<&str>,
) -> Result<(), AssetsError> {
    if skus.is_empty() {
        return Ok(());
    }
    let unknown: Option<(String,)> = sqlx::query_as(
        "SELECT t.s FROM unnest($1::text[]) WITH ORDINALITY AS t(s, n) \
         WHERE NOT EXISTS (SELECT 1 FROM asset_models m WHERE m.sku = t.s) \
         ORDER BY t.n LIMIT 1",
    )
    .bind(&skus)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| AssetsError::Storage(e.to_string()))?;
    match unknown {
        Some((sku,)) => Err(AssetsError::UnknownModel(sku)),
        None => Ok(()),
    }
}

/// Whether `event` (the earliest of `new_ids`, all already inserted)
/// sorts before the projection's last folded event, so the fast path
/// would apply it out of the log's order and a full reprojection is
/// owed. A DAY earlier always does. On the projection's own last day it
/// does when a stored event of that day — not one of `new_ids` — sorts
/// after it in byte order of id, the order `project` folds a day in.
/// Until backlog be459ab9 only the day was compared, so a same-day
/// event arriving late was folded after one it precedes, and the
/// projection table answered a different phase than the log.
async fn sorts_before_the_projection(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    existing: Option<&AssetCurrentState>,
    event: &AssetEvent,
    new_ids: &[String],
) -> Result<bool, AssetsError> {
    let Some(state) = existing else {
        return Ok(false);
    };
    if event.ts != state.last_event_at {
        return Ok(event.ts < state.last_event_at);
    }
    let (later,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM asset_events \
         WHERE asset_id = $1 AND ts = $2 AND id COLLATE \"C\" > $3 \
           AND NOT (id = ANY($4)))",
    )
    .bind(&event.asset_id.0)
    .bind(event.ts)
    .bind(&event.id.0)
    .bind(new_ids)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| AssetsError::Storage(e.to_string()))?;
    Ok(later)
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(db_err) => db_err.code().as_deref() == Some("23505"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_tag_covers_all_variants() {
        // Compile-time exhaustiveness is already guaranteed by the match.
        // This test just confirms the tags look right for a couple of variants.
        assert_eq!(
            kind_tag(&AssetEventKind::Received {
                sku: Some("Boss-TEST-2024".into()),
                source: crate::types::IntakeSource::new("oem-new"),
                oem_serial: None,
            }),
            "Received"
        );
        assert_eq!(
            kind_tag(&AssetEventKind::WarrantyExpired),
            "WarrantyExpired"
        );
    }

    #[test]
    fn event_row_round_trips_through_json_payload() {
        use crate::types::IntakeSource;

        let kind = AssetEventKind::Received {
            sku: Some("Boss-TEST-2024".into()),
            source: IntakeSource::new("buyback"),
            oem_serial: None,
        };
        let payload = serde_json::to_value(&kind).unwrap();
        let row = EventRow {
            id: "evt-1".into(),
            asset_id: "SN-1".into(),
            ts: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            // EventRow mirrors the nullable DB column; a NULL actor_id
            // decodes to the named `platform` automation.
            actor_id: None,
            payload,
        };
        let event = row.into_replayable_event().unwrap().unwrap();
        assert_eq!(event.kind, kind);
    }
}
