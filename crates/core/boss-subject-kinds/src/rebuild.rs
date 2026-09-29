//! `rebuild_subject_kinds` — replay `subject_kind.updated` from
//! `audit_log` onto the `subject_kinds` rows the migrations seed.
//!
//! WHY (the adversarial review of car abc2e9d5, 2026-09-28, MED-2:
//! closure). The metadata door's facts were read by nothing, so a
//! log-rooted rebuild onto a fresh database — migrations, then the log
//! — came back without every edit the door had made, including the
//! `module` values it exists to set. The rows themselves are born in
//! migrations (declaring a kind has no evented door yet, ca7bf46c), so
//! this is not a truncate-and-reproject: it folds the log onto the
//! seeded rows, through the same `apply_change` the write ran.
//!
//! It never writes a row the log disagrees with. Per kind, the pure
//! [`replay_log`] answers: already at the log's head (a live table, or a
//! second run — nothing written), at the log's start (a fresh database —
//! the head is written), or at neither (drift — the whole rebuild fails
//! naming the kind and the audit row, and rolls back).

use std::collections::BTreeMap;

use serde_json::Value;
use sqlx::PgPool;

use crate::port::{MetadataChange, SUBJECT_KIND_UPDATED, replay_log};

/// Held for the whole transaction so two rebuilds of the registry never
/// interleave — the lock every `boss-rebuild-all` step takes. This one
/// took none until backlog 8d5ac7c5.
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("subject-kinds");

/// What one rebuild did. `events_processed` is the name boss-rebuild's
/// tally reads.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SubjectKindsRebuildReport {
    pub events_processed: u64,
    pub kinds_written: u64,
    pub kinds_already_current: u64,
}

pub async fn rebuild_subject_kinds(pool: &PgPool) -> Result<SubjectKindsRebuildReport, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("taking the subject-kinds rebuild lock: {e}"))?;
    let rows: Vec<(i64, Value)> =
        sqlx::query_as("SELECT id, payload FROM audit_log WHERE kind = $1 ORDER BY id")
            .bind(SUBJECT_KIND_UPDATED)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("reading {SUBJECT_KIND_UPDATED}: {e}"))?;

    let mut report = SubjectKindsRebuildReport::default();
    let mut by_kind: BTreeMap<String, Vec<(i64, MetadataChange)>> = BTreeMap::new();
    for (id, payload) in rows {
        let kind = payload
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("audit_log {id}: {SUBJECT_KIND_UPDATED} names no kind"))?
            .to_string();
        let change: MetadataChange = serde_json::from_value(payload)
            .map_err(|e| format!("audit_log {id}: not a metadata change: {e}"))?;
        by_kind.entry(kind).or_default().push((id, change));
        report.events_processed += 1;
    }

    for (kind, facts) in &by_kind {
        let held: Option<Value> =
            sqlx::query_scalar("SELECT metadata FROM subject_kinds WHERE kind = $1 FOR UPDATE")
                .bind(kind)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| format!("reading subject kind {kind}: {e}"))?;
        let Some(held) = held else {
            return Err(format!(
                "drift: the log edits subject kind `{kind}` (audit_log {}), which the registry \
                 does not carry",
                facts[0].0
            ));
        };
        let log: Vec<MetadataChange> = facts.iter().map(|(_, c)| c.clone()).collect();
        let head = replay_log(&held, &log).map_err(|e| {
            let ids: Vec<String> = facts.iter().map(|(id, _)| id.to_string()).collect();
            format!("subject kind `{kind}` (audit_log {}): {e}", ids.join(", "))
        })?;
        match head {
            None => report.kinds_already_current += 1,
            Some(metadata) => {
                sqlx::query("UPDATE subject_kinds SET metadata = $2 WHERE kind = $1")
                    .bind(kind)
                    .bind(&metadata)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| format!("writing subject kind {kind}: {e}"))?;
                report.kinds_written += 1;
            }
        }
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(report)
}
