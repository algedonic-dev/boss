//! Rebuild the GL projection from `financial_facts`.
//!
//! Per docs/architecture-decisions.md §Finance & ledger: the
//! ledger is a projection of immutable
//! facts. When rules change or during initial backfill, `rebuild` drops
//! entries in open periods and re-projects them through the active ruleset.
//! Locked periods are never touched — their `rule_version_id` stays pinned.
//!
//! v1c ships online rebuild only (advisory-lock protected so concurrent
//! domain writes block briefly). The `--offline` variant from the design
//! Q4 comes in when the gateway has a read-only mode to go with it; in
//! the meantime, rebuild scales fine because it runs in one transaction.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::LedgerError;
use crate::postgres::post_fact_in_tx;
use crate::types::FactRef;

/// Advisory-lock key for the ledger rebuild, derived from the projection
/// name. Shared with `replay_check` (through this `pub(crate)` const) so
/// the verifier serializes against concurrent rebuilds the same way two
/// rebuilds serialize against each other.
pub(crate) const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("ledger");

/// Summary of a completed rebuild.
#[derive(Debug, Clone, PartialEq)]
pub struct RebuildReport {
    pub facts_processed: u64,
    pub entries_dropped: u64,
    pub entries_created: u64,
    pub periods_rebuilt: u64,
    pub total_debits: i64,
    pub total_credits: i64,
}

impl RebuildReport {
    pub fn is_balanced(&self) -> bool {
        self.total_debits == self.total_credits
    }
}

/// Rebuild the GL projection for every open period.
///
/// Wrapped in a single transaction holding an advisory lock for the
/// duration — concurrent domain writes that want to post a fact will
/// queue until we release. For today's data volumes that's fine; when
/// the fact log grows large enough to make this too long a pause, we'll
/// move to per-period transactions with a separate lock per period.
pub async fn rebuild(pool: &PgPool) -> Result<RebuildReport, LedgerError> {
    let mut tx = pool
        .begin()
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

    let entries_dropped: i64 = sqlx::query_scalar(
        "WITH deleted AS ( \
            DELETE FROM gl_journal_entries \
            WHERE period_id = ANY($1) \
            RETURNING 1 \
         ) SELECT COUNT(*) FROM deleted",
    )
    .bind(&open_period_ids)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // Re-project every fact whose happened_on falls in an open period, or
    // whose period doesn't exist yet. `ensure_period_for` inside the
    // projection auto-creates missing periods.
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

    let mut facts_processed: u64 = 0;
    let mut entries_created: u64 = 0;
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
        facts_processed += 1;
        entries_created += 1;
    }

    let (total_debits, total_credits): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(debit_cents), 0)::bigint, COALESCE(SUM(credit_cents), 0)::bigint \
         FROM gl_journal_lines",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(RebuildReport {
        facts_processed,
        entries_dropped: entries_dropped as u64,
        entries_created,
        periods_rebuilt: open_period_ids.len() as u64,
        total_debits,
        total_credits,
    })
}
