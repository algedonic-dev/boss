//! Rebuild `payroll_runs` + `payroll_run_lines` from the
//! `finance.payroll.run` facts in `financial_facts`.
//!
//! Both tables are pure projections of the payroll fact: the header
//! totals AND the per-employee `lines` array ride in the fact payload
//! (recorded inline by the synthesize/create handlers, reconstructed
//! from `audit_log` by `rebuild_facts` via the
//! `ledger.payroll.run → finance.payroll.run` projection rule). This
//! projector turns those facts back into the two read-model tables.
//!
//! Why this exists: before it, `payroll_runs` was written directly by
//! the synthesize handler and owned by no rebuilder. A `boss-rebuild-all`
//! run (and therefore every demo epoch reset, which trims `audit_log`
//! then rebuilds) left the directly-written rows untouched, so
//! prior-cycle rows survived and the calendar-dated synthesize
//! idempotency key collided across cycles — payroll silently stopped
//! posting to the GL. Making payroll a projection puts it under the
//! same audit-log-rooted rebuild guarantee as `financial_facts` and the
//! journal: the no-leak machinery (replay-check diffs the
//! `finance.payroll.run` fact payload — lines included — live vs.
//! replayed) now covers it end to end.
//!
//! TRUNCATE-then-replay, one transaction, advisory-locked. Runs after
//! `ledger-facts` in `boss-rebuild-all` because it consumes
//! `financial_facts`.

use serde::Deserialize;
use sqlx::{PgPool, Row};

use crate::error::LedgerError;
use crate::payroll::PayrollRunLine;

/// Advisory-lock key for the payroll-rebuild, derived from the
/// projection name — serializes concurrent payroll-rebuilds the same
/// way `rebuild_facts` serializes concurrent fact-rebuilds.
const REBUILD_PAYROLL_LOCK_KEY: i64 = boss_core::rebuild::lock_key("ledger-payroll");

#[derive(Debug, Clone)]
pub struct RebuildPayrollReport {
    pub facts_scanned: u64,
    pub runs_written: u64,
    pub lines_written: u64,
}

/// The reconstructable shape of a payroll run, read straight out of the
/// `finance.payroll.run` fact payload. Mirrors the columns the
/// synthesize/create handlers stamp; `status` is always `'posted'`
/// (the live write hardcodes it, so it isn't carried in the payload).
#[derive(Debug, Deserialize)]
struct PayrollFactPayload {
    run_id: String,
    run_date: chrono::NaiveDate,
    period_start: chrono::NaiveDate,
    period_end: chrono::NaiveDate,
    gross_cents: i64,
    employer_tax_cents: i64,
    withheld_cents: i64,
    net_cents: i64,
    employee_count: i32,
    provider: String,
    /// Per-employee detail. `default` keeps a pre-lines fact (emitted
    /// before the payload carried `lines`) from failing the whole
    /// rebuild — it reconstructs the header and leaves the detail
    /// empty, the same skip-don't-fail leniency `rebuild_facts` gives a
    /// missing field.
    #[serde(default)]
    lines: Vec<PayrollRunLine>,
}

/// Rebuild payroll from the facts. Opens a transaction, takes the
/// advisory lock, replays, commits.
pub async fn rebuild_payroll(pool: &PgPool) -> Result<RebuildPayrollReport, LedgerError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_PAYROLL_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let report = rebuild_payroll_in_tx(&mut tx).await?;

    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(report)
}

/// Caller-controlled-transaction variant — symmetric with
/// `rebuild_facts_in_tx`. TRUNCATEs both payroll tables (lines first to
/// satisfy the FK without CASCADE) and re-derives every row from the
/// non-superseded `finance.payroll.run` facts.
pub async fn rebuild_payroll_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<RebuildPayrollReport, LedgerError> {
    // Pure-projection wipe: no payroll row may live that doesn't trace
    // back to a fact. Both tables in one TRUNCATE so the
    // `payroll_run_lines → payroll_runs` FK is satisfied without
    // CASCADE.
    sqlx::query("TRUNCATE payroll_run_lines, payroll_runs")
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // Deterministic order. The natural key `(kind, source_table,
    // source_id)` makes each run a distinct fact, so order doesn't
    // change the result set — but a stable order keeps logs and any
    // future diffing cheap.
    let fact_rows = sqlx::query(
        "SELECT payload FROM financial_facts \
         WHERE kind = 'finance.payroll.run' AND supersede_reason IS NULL \
         ORDER BY happened_on, source_id",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut facts_scanned: u64 = 0;
    let mut runs_written: u64 = 0;
    let mut lines_written: u64 = 0;

    for row in &fact_rows {
        facts_scanned += 1;
        let payload: serde_json::Value = row.get("payload");
        let run: PayrollFactPayload = serde_json::from_value(payload)
            .map_err(|e| LedgerError::Storage(format!("finance.payroll.run payload: {e}")))?;

        // Faithful reconstruction from the payload totals (no
        // recomputation — the fact IS the record of what happened).
        // status is always 'posted', matching the live `create_run`.
        sqlx::query(
            "INSERT INTO payroll_runs \
                (id, run_date, period_start, period_end, gross_cents, employer_tax_cents, \
                 withheld_cents, net_cents, employee_count, provider, status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'posted')",
        )
        .bind(&run.run_id)
        .bind(run.run_date)
        .bind(run.period_start)
        .bind(run.period_end)
        .bind(run.gross_cents)
        .bind(run.employer_tax_cents)
        .bind(run.withheld_cents)
        .bind(run.net_cents)
        .bind(run.employee_count)
        .bind(&run.provider)
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
        runs_written += 1;

        for line in &run.lines {
            sqlx::query(
                "INSERT INTO payroll_run_lines \
                    (run_id, employee_id, gross_cents, withheld_cents, net_cents, \
                     department, role) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(&run.run_id)
            .bind(&line.employee_id)
            .bind(line.gross_cents)
            .bind(line.withheld_cents)
            .bind(line.net_cents)
            .bind(&line.department)
            .bind(&line.role)
            .execute(&mut **tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
            lines_written += 1;
        }
    }

    Ok(RebuildPayrollReport {
        facts_scanned,
        runs_written,
        lines_written,
    })
}
