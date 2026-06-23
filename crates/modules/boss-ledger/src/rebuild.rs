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

    // Rebuild the gl_account_daily convenience projection from the final
    // journal state. A full TRUNCATE + re-aggregate (one GROUP BY over
    // the lines plus a cash-attribution pass) — cheap, and correct
    // regardless of which periods were re-projected. The live path
    // increments this table per entry; rebuild re-derives it
    // authoritatively so the two always agree (replay-check territory).
    rebuild_account_daily(&mut tx).await?;

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

/// Rebuild the `gl_account_daily` rollup from the current journal state.
///
/// Full TRUNCATE + re-aggregate: cheap (one GROUP BY over the lines, then
/// a cash-attribution pass) and always correct regardless of which periods
/// were re-projected, so it runs at the end of every rebuild. The
/// cash-attribution pass mirrors the per-entry split the live path applies
/// in `post_fact_in_tx`, using `trunc()` (truncate toward zero) so it
/// matches the live path's i128 integer division to the cent.
async fn rebuild_account_daily(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), LedgerError> {
    sqlx::query("TRUNCATE gl_account_daily")
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // Debit/credit totals per account-day (cash_flow filled in below).
    sqlx::query(
        "INSERT INTO gl_account_daily \
            (account_id, posted_on, debit_cents, credit_cents, cash_flow_cents) \
         SELECT l.account_id, e.posted_on, \
                SUM(l.debit_cents)::bigint, SUM(l.credit_cents)::bigint, 0 \
         FROM gl_journal_lines l \
         JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         GROUP BY l.account_id, e.posted_on",
    )
    .execute(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // Cash attributed to each non-pool offset account per day: for every
    // entry that moves the cash pool, split its net pool change across
    // offsets in proportion to credit-net share. trunc()::bigint
    // truncates toward zero, matching the live path's i128 division.
    sqlx::query(
        "WITH pool_jes AS ( \
             SELECT e.id, e.posted_on, \
                    SUM(l.debit_cents) - SUM(l.credit_cents) AS net_cash \
             FROM gl_journal_entries e \
             JOIN gl_journal_lines l ON l.journal_entry_id = e.id \
             JOIN gl_accounts a ON a.id = l.account_id AND a.code = ANY($1) \
             GROUP BY e.id, e.posted_on \
             HAVING SUM(l.debit_cents) - SUM(l.credit_cents) <> 0 \
         ), \
         offset_per_je AS ( \
             SELECT je.id AS je_id, je.posted_on, je.net_cash, l.account_id, \
                    SUM(l.credit_cents - l.debit_cents) AS offset_cr_net \
             FROM pool_jes je \
             JOIN gl_journal_lines l ON l.journal_entry_id = je.id \
             JOIN gl_accounts a ON a.id = l.account_id AND a.code <> ALL($1) \
             GROUP BY je.id, je.posted_on, je.net_cash, l.account_id \
         ), \
         offset_totals AS ( \
             SELECT je_id, SUM(offset_cr_net) AS offset_total_cr \
             FROM offset_per_je GROUP BY je_id \
         ), \
         attributed AS ( \
             SELECT o.account_id, o.posted_on, \
                    SUM(CASE WHEN ot.offset_total_cr <> 0 \
                             THEN trunc(o.net_cash::numeric * o.offset_cr_net::numeric \
                                        / ot.offset_total_cr::numeric)::bigint \
                             ELSE 0 END)::bigint AS cash_cents \
             FROM offset_per_je o \
             JOIN offset_totals ot ON ot.je_id = o.je_id \
             GROUP BY o.account_id, o.posted_on \
         ) \
         UPDATE gl_account_daily d \
            SET cash_flow_cents = attributed.cash_cents \
         FROM attributed \
         WHERE attributed.account_id = d.account_id \
           AND attributed.posted_on = d.posted_on",
    )
    .bind(&crate::postgres::CASH_POOL[..])
    .execute(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(())
}
