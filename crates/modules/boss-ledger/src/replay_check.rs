//! 1:1 reconstruction integrity check for the ledger projection.
//!
//! The principle: `gl_journal_entries` (and `gl_journal_lines`) must be
//! byte-for-byte regenerable by replaying every `financial_facts` row
//! through the active posting-rule registry. Any divergence is either
//! (a) non-determinism in a rule (a bug), or (b) a live-side mutation
//! that bypassed the rule pipeline (also a bug).
//!
//! This module never mutates live state. The strategy:
//!
//! 1. Open a transaction holding the same advisory lock `rebuild` uses
//!    so we don't race concurrent writers.
//! 2. Snapshot the live `gl_journal_entries`/`gl_journal_lines` for
//!    *open* periods. (Locked periods are immutable by design — their
//!    `rule_version_id` is pinned, so they are out of scope.)
//! 3. Inside the same transaction, DELETE entries in open periods and
//!    re-project every fact through `post_fact_in_tx`.
//! 4. Read the rebuilt rows back out.
//! 5. Diff the snapshot against the rebuilt set on the natural key
//!    `(fact_id, rule_version_id)`.
//! 6. **ROLLBACK.** Live state is untouched.
//!
//! Wire as a daily systemd timer alongside `boss-audit-integrity-check`,
//! and as a CI step on every PR that touches `gl_rule_versions`.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::LedgerError;
use crate::postgres::post_fact_in_tx;
use crate::rebuild::REBUILD_LOCK_KEY;
use crate::rebuild_facts::{RebuildFactsReport, rebuild_facts_in_tx};
use crate::types::FactRef;

/// Natural key for a financial fact: the `(kind, source_table, source_id)`
/// tuple from the unique index. NULL `source_table` / `source_id` are
/// treated as the empty string for keying — manual entries write
/// non-NULL `source_table='manual_entries'` so this only matters for
/// any pre-projection-era rows that survive in the live tree.
pub type FactKey = (String, String, String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactShape {
    pub kind: String,
    pub happened_on: chrono::NaiveDate,
    pub source_table: Option<String>,
    pub source_id: Option<String>,
    pub created_by: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactDivergence {
    OnlyInLive {
        key: FactKey,
        live: FactShape,
    },
    OnlyInReplay {
        key: FactKey,
        replay: FactShape,
    },
    Mismatch {
        key: FactKey,
        live: FactShape,
        replay: FactShape,
    },
}

/// End-to-end audit-log-rooted replay-check. Composes
/// `rebuild_facts_in_tx` (audit_log → financial_facts) with
/// `rebuild`'s in-tx replay (financial_facts → gl_journal_entries),
/// snapshots both layers before/after, diffs each, and rollbacks.
///
/// Two divergence sets are returned. Use them to localize drift:
/// fact-level divergences mean an upstream `*.created` event is
/// missing or its payload disagrees with what the live writer
/// emitted; entry-level divergences mean a posting rule produced
/// different lines this run vs. last.
#[derive(Debug, Clone)]
pub struct DeepReplayCheckReport {
    pub events_scanned: u64,
    pub facts_in_live: u64,
    pub facts_in_replay: u64,
    pub fact_divergences: Vec<FactDivergence>,
    pub open_periods: u64,
    pub live_entries: u64,
    pub replay_entries: u64,
    pub entry_divergences: Vec<Divergence>,
    pub rebuild_report: RebuildFactsReport,
}

impl DeepReplayCheckReport {
    pub fn is_ok(&self) -> bool {
        self.fact_divergences.is_empty() && self.entry_divergences.is_empty()
    }
}

pub async fn replay_check_from_audit_log(
    pool: &PgPool,
) -> Result<DeepReplayCheckReport, LedgerError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // One consistent snapshot for every read in this tx. With the
    // shadow tables below, live writers keep writing DURING the check
    // — under READ COMMITTED a fact committed between the live
    // snapshot and the audit_log scan would appear in the replay but
    // not in "live", a false OnlyInReplay divergence. (The old
    // exclusive TRUNCATE lock froze the world instead; snapshot
    // isolation is the honest replacement.) Must precede any query.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let open_period_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM gl_periods WHERE status = 'open' ORDER BY starts_on")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let live_facts = load_facts_for_periods(&mut tx, &open_period_ids).await?;
    let live_entries = load_entries_for_periods(&mut tx, &open_period_ids).await?;

    // Shadow the mutable tables — every unqualified reference below
    // (including `rebuild_facts_in_tx`'s opening `TRUNCATE
    // financial_facts CASCADE`) now resolves to the session-private
    // clones, so the live tables are never locked and never written.
    // The old pending-trigger-events TRUNCATE hazard is gone with the
    // trigger (LIKE doesn't copy it); the CASCADE degrades to a plain
    // truncate of the already-empty clone, which is exactly right.
    create_replay_shadows(&mut tx, true).await?;

    let rebuild_report = rebuild_facts_in_tx(&mut tx).await?;

    // Now project facts → entries, scoped to open periods (mirrors
    // `rebuild`'s behavior). Only facts within open-period dates
    // re-post; locked-period facts stay untouched.
    let fact_rows = sqlx::query(
        "SELECT f.id, f.kind, f.happened_on, f.payload \
         FROM financial_facts f \
         LEFT JOIN gl_periods p \
            ON p.kind = 'month' \
           AND f.happened_on BETWEEN p.starts_on AND p.ends_on \
         WHERE (p.id IS NULL OR p.status = 'open') \
           AND f.supersede_reason IS NULL \
         ORDER BY f.happened_on, f.recorded_at",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    for row in &fact_rows {
        let id: Uuid = row.get("id");
        let kind: String = row.get("kind");
        let happened_on: chrono::NaiveDate = row.get("happened_on");
        let payload: serde_json::Value = row.get("payload");
        let fact = FactRef {
            id,
            kind: &kind,
            happened_on,
            payload: &payload,
        };
        post_fact_in_tx(&mut tx, &fact).await?;
    }

    let post_open_period_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM gl_periods WHERE status = 'open' ORDER BY starts_on")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
    let replay_facts = load_facts_for_periods(&mut tx, &post_open_period_ids).await?;
    let replay_entries = load_entries_for_periods(&mut tx, &post_open_period_ids).await?;

    tx.rollback()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let fact_divergences = diff_facts(&live_facts, &replay_facts);
    let entry_divergences = diff_entries(&live_entries, &replay_entries);

    Ok(DeepReplayCheckReport {
        events_scanned: rebuild_report.events_scanned,
        facts_in_live: live_facts.len() as u64,
        facts_in_replay: replay_facts.len() as u64,
        fact_divergences,
        open_periods: open_period_ids.len() as u64,
        live_entries: live_entries.len() as u64,
        replay_entries: replay_entries.len() as u64,
        entry_divergences,
        rebuild_report,
    })
}

fn diff_facts(
    live: &std::collections::BTreeMap<FactKey, FactShape>,
    replay: &std::collections::BTreeMap<FactKey, FactShape>,
) -> Vec<FactDivergence> {
    let mut out = Vec::new();
    let mut live_remaining: std::collections::BTreeSet<FactKey> = live.keys().cloned().collect();
    for (key, r) in replay {
        match live.get(key) {
            Some(l) if l != r => out.push(FactDivergence::Mismatch {
                key: key.clone(),
                live: l.clone(),
                replay: r.clone(),
            }),
            Some(_) => {}
            None => out.push(FactDivergence::OnlyInReplay {
                key: key.clone(),
                replay: r.clone(),
            }),
        }
        live_remaining.remove(key);
    }
    for key in &live_remaining {
        if let Some(l) = live.get(key) {
            out.push(FactDivergence::OnlyInLive {
                key: key.clone(),
                live: l.clone(),
            });
        }
    }
    out
}

fn diff_entries(
    live: &std::collections::BTreeMap<EntryKey, EntryShape>,
    replay: &std::collections::BTreeMap<EntryKey, EntryShape>,
) -> Vec<Divergence> {
    let mut out = Vec::new();
    let mut live_remaining: std::collections::BTreeSet<EntryKey> = live.keys().copied().collect();
    for (key, r) in replay {
        match live.get(key) {
            Some(l) if l != r => out.push(Divergence::Mismatch {
                key: *key,
                live: l.clone(),
                replay: r.clone(),
            }),
            Some(_) => {}
            None => out.push(Divergence::OnlyInReplay {
                key: *key,
                replay: r.clone(),
            }),
        }
        live_remaining.remove(key);
    }
    for key in &live_remaining {
        if let Some(l) = live.get(key) {
            out.push(Divergence::OnlyInLive {
                key: *key,
                live: l.clone(),
            });
        }
    }
    out
}

async fn load_facts_for_periods(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    period_ids: &[Uuid],
) -> Result<std::collections::BTreeMap<FactKey, FactShape>, LedgerError> {
    let rows = sqlx::query(
        "SELECT f.kind, f.happened_on, f.source_table, f.source_id, f.created_by, f.payload \
         FROM financial_facts f \
         JOIN gl_periods p \
            ON p.kind = 'month' \
           AND f.happened_on BETWEEN p.starts_on AND p.ends_on \
         WHERE p.id = ANY($1) \
           AND f.supersede_reason IS NULL",
    )
    .bind(period_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut out = std::collections::BTreeMap::new();
    for row in &rows {
        let kind: String = row.get("kind");
        let happened_on: chrono::NaiveDate = row.get("happened_on");
        let source_table: Option<String> = row.get("source_table");
        let source_id: Option<String> = row.get("source_id");
        let created_by: String = row.get("created_by");
        let payload: serde_json::Value = row.get("payload");
        let key = (
            kind.clone(),
            source_table.clone().unwrap_or_default(),
            source_id.clone().unwrap_or_default(),
        );
        out.insert(
            key,
            FactShape {
                kind,
                happened_on,
                source_table,
                source_id,
                created_by,
                payload,
            },
        );
    }
    Ok(out)
}

/// One journal-entry row collapsed to its replay-comparable shape.
///
/// `posted_on`, `period_id`, and `memo` are the projected entry fields
/// minus `id` and `created_at` (those are non-deterministic). Lines are
/// sorted by `sort_order` so two equivalent entries compare equal even
/// if Postgres returned them in a different physical order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryShape {
    pub posted_on: chrono::NaiveDate,
    pub period_id: Uuid,
    pub memo: Option<String>,
    pub lines: Vec<LineShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineShape {
    pub account_code: String,
    pub debit_cents: i64,
    pub credit_cents: i64,
    pub currency: String,
    pub memo: Option<String>,
}

/// Natural key for an entry: the same `(fact_id, rule_version_id)`
/// pair the unique index uses.
pub type EntryKey = (Uuid, Uuid);

/// One concrete divergence between live and replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Divergence {
    /// Live has an entry that replay did not produce.
    OnlyInLive { key: EntryKey, live: EntryShape },
    /// Replay produced an entry that live does not have.
    OnlyInReplay { key: EntryKey, replay: EntryShape },
    /// Both sides have an entry but they differ.
    Mismatch {
        key: EntryKey,
        live: EntryShape,
        replay: EntryShape,
    },
}

#[derive(Debug, Clone)]
pub struct ReplayCheckReport {
    pub facts_replayed: u64,
    pub open_periods: u64,
    pub live_entries: u64,
    pub replay_entries: u64,
    pub divergences: Vec<Divergence>,
}

impl ReplayCheckReport {
    pub fn is_ok(&self) -> bool {
        self.divergences.is_empty()
    }
}

/// Shadow the mutable ledger tables with session-private TEMP clones
/// for the rest of the transaction. Postgres resolves unqualified
/// names through `pg_temp` first, so the ENTIRE existing replay path
/// (`rebuild_facts_in_tx`, `post_fact_in_tx`, the supersede replay)
/// writes into the shadows untouched, while reads of reference tables
/// (audit_log, gl_accounts, gl_periods, gl_fact_projection_rules)
/// fall through to the live schema. `LIKE … INCLUDING ALL` copies the
/// PKs, unique keys (the ON CONFLICT targets), defaults and CHECKs —
/// deliberately NOT the FKs or the deferred balance trigger (LIKE
/// never copies either): the replay needs no FK enforcement, and
/// balance is guaranteed by the ruleset's balanced-draft construction
/// plus the diff itself.
///
/// This is what makes the check LOCK-FREE for live writers: before
/// 2026-07-10 the deep path's `TRUNCATE financial_facts` took an
/// ACCESS EXCLUSIVE lock on the live table for the check's full
/// runtime (~2 min at year scale — every fact write stalled behind it
/// nightly, and a concurrently-running regen hard-failed on a 30s
/// client timeout), and the entry path's open-period DELETE held row
/// locks that blocked concurrent journal posts. The temp clones are
/// dropped with the ROLLBACK.
async fn create_replay_shadows(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    shadow_financial_facts: bool,
) -> Result<(), LedgerError> {
    let mut tables = vec!["gl_journal_entries", "gl_journal_lines", "gl_account_daily"];
    if shadow_financial_facts {
        tables.insert(0, "financial_facts");
    }
    for t in tables {
        sqlx::query(&format!(
            "CREATE TEMP TABLE {t} (LIKE public.{t} INCLUDING ALL) ON COMMIT DROP"
        ))
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(format!("shadowing {t}: {e}")))?;
    }
    Ok(())
}

/// Run the verifier. Read-only — opens a transaction, replays into it,
/// compares, then ROLLBACK.
pub async fn replay_check(pool: &PgPool) -> Result<ReplayCheckReport, LedgerError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // Same snapshot-isolation reasoning as the deep check: shadows let
    // writers proceed, so the tx must see one consistent moment.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let open_period_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM gl_periods WHERE status = 'open' ORDER BY starts_on")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let live = load_entries_for_periods(&mut tx, &open_period_ids).await?;

    // Shadow the journal tables (facts stay LIVE — the entry-level
    // check replays entries FROM live facts). The clones start empty,
    // which replaces the old open-period DELETE — the row locks it
    // held on live entries blocked concurrent journal posts for the
    // check's duration.
    create_replay_shadows(&mut tx, false).await?;

    let fact_rows = sqlx::query(
        "SELECT f.id, f.kind, f.happened_on, f.payload \
         FROM financial_facts f \
         LEFT JOIN gl_periods p \
            ON p.kind = 'month' \
           AND f.happened_on BETWEEN p.starts_on AND p.ends_on \
         WHERE (p.id IS NULL OR p.status = 'open') \
           AND f.supersede_reason IS NULL \
         ORDER BY f.happened_on, f.recorded_at",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut facts_replayed: u64 = 0;
    for row in &fact_rows {
        let id: Uuid = row.get("id");
        let kind: String = row.get("kind");
        let happened_on: chrono::NaiveDate = row.get("happened_on");
        let payload: serde_json::Value = row.get("payload");
        let fact = FactRef {
            id,
            kind: &kind,
            happened_on,
            payload: &payload,
        };
        post_fact_in_tx(&mut tx, &fact).await?;
        facts_replayed += 1;
    }

    // Re-read open-period set in case `ensure_period_for` inside the
    // replay auto-created a previously-missing month. Those new rows
    // count as "open" for diff scope.
    let post_open_period_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM gl_periods WHERE status = 'open' ORDER BY starts_on")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
    let replay = load_entries_for_periods(&mut tx, &post_open_period_ids).await?;

    // Always rollback. The verifier is read-only by contract.
    tx.rollback()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut divergences = Vec::new();
    let mut live_keys: std::collections::BTreeSet<EntryKey> = live.keys().copied().collect();
    let replay_keys: std::collections::BTreeSet<EntryKey> = replay.keys().copied().collect();

    for key in &replay_keys {
        match (live.get(key), replay.get(key)) {
            (Some(l), Some(r)) if l != r => divergences.push(Divergence::Mismatch {
                key: *key,
                live: l.clone(),
                replay: r.clone(),
            }),
            (Some(_), Some(_)) => {}
            (None, Some(r)) => divergences.push(Divergence::OnlyInReplay {
                key: *key,
                replay: r.clone(),
            }),
            _ => unreachable!(),
        }
        live_keys.remove(key);
    }
    for key in &live_keys {
        if let Some(l) = live.get(key) {
            divergences.push(Divergence::OnlyInLive {
                key: *key,
                live: l.clone(),
            });
        }
    }

    Ok(ReplayCheckReport {
        facts_replayed,
        open_periods: open_period_ids.len() as u64,
        live_entries: live.len() as u64,
        replay_entries: replay.len() as u64,
        divergences,
    })
}

/// Load every entry whose period is in `period_ids` and collapse it to
/// an `EntryShape` keyed on `(fact_id, rule_version_id)`. Lines are
/// pulled in one query to keep the round-trip count constant.
async fn load_entries_for_periods(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    period_ids: &[Uuid],
) -> Result<std::collections::BTreeMap<EntryKey, EntryShape>, LedgerError> {
    let entry_rows = sqlx::query(
        "SELECT id, fact_id, rule_version_id, posted_on, period_id, memo \
         FROM gl_journal_entries \
         WHERE period_id = ANY($1)",
    )
    .bind(period_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut shapes: std::collections::BTreeMap<EntryKey, EntryShape> =
        std::collections::BTreeMap::new();
    let mut entry_ids: Vec<Uuid> = Vec::with_capacity(entry_rows.len());
    let mut id_to_key: std::collections::HashMap<Uuid, EntryKey> =
        std::collections::HashMap::with_capacity(entry_rows.len());
    for row in &entry_rows {
        let id: Uuid = row.get("id");
        let fact_id: Uuid = row.get("fact_id");
        let rule_version_id: Uuid = row.get("rule_version_id");
        let posted_on: chrono::NaiveDate = row.get("posted_on");
        let period_id: Uuid = row.get("period_id");
        let memo: Option<String> = row.get("memo");
        let key = (fact_id, rule_version_id);
        shapes.insert(
            key,
            EntryShape {
                posted_on,
                period_id,
                memo,
                lines: Vec::new(),
            },
        );
        entry_ids.push(id);
        id_to_key.insert(id, key);
    }

    if entry_ids.is_empty() {
        return Ok(shapes);
    }

    let line_rows = sqlx::query(
        "SELECT l.journal_entry_id, a.code AS account_code, l.debit_cents, l.credit_cents, \
                l.currency, l.memo, l.sort_order \
         FROM gl_journal_lines l \
         JOIN gl_accounts a ON a.id = l.account_id \
         WHERE l.journal_entry_id = ANY($1) \
         ORDER BY l.journal_entry_id, l.sort_order, a.code",
    )
    .bind(&entry_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    for row in &line_rows {
        let entry_id: Uuid = row.get("journal_entry_id");
        let Some(key) = id_to_key.get(&entry_id) else {
            continue;
        };
        let line = LineShape {
            account_code: row.get("account_code"),
            debit_cents: row.get("debit_cents"),
            credit_cents: row.get("credit_cents"),
            currency: row.get("currency"),
            memo: row.get("memo"),
        };
        if let Some(shape) = shapes.get_mut(key) {
            shape.lines.push(line);
        }
    }

    Ok(shapes)
}
