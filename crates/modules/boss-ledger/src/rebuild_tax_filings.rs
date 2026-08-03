//! Rebuild `tax_filings` from the `ledger.tax.filing.created` +
//! `ledger.tax.remitted` events in `audit_log`.
//!
//! `tax_filings` is a pure projection of the log: creation carries the
//! whole row shape, remittance carries the status flip. This projector
//! replays both in log order.
//!
//! Why this exists: before it, `tax_filings` was written directly by
//! the create/remit handlers and owned by no rebuilder — the same
//! stale-across-epochs class `rebuild_payroll` closed for
//! `payroll_runs`. A `boss-rebuild-all` run (and therefore every demo
//! epoch reset, which trims `audit_log` then rebuilds) left the
//! directly-written rows untouched, so prior-lap filings survived
//! already marked `paid`. `remit_tax_filing` short-circuits on an
//! already-paid filing, so the new lap's remittance silently never
//! posted: no `finance.tax.remitted` fact, no draining journal entry,
//! and 2150/2300/2310/2320 accrued forever while 1000 Cash stayed
//! overstated by the un-remitted total.
//!
//! Rooted in `audit_log` rather than `financial_facts` (the payroll
//! projector's source) because a filing for a non-accruing kind —
//! sales, payroll_941/940, excise, whose liability was already credited
//! by its own source facts — posts no journal entry and therefore has
//! no fact to rebuild from. Only income tax accrues against 6500. The
//! `ledger.tax.filing.created` event carries the row shape for every
//! kind instead.
//!
//! TRUNCATE-then-replay, one transaction, advisory-locked.

use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::{PgPool, Row};

use crate::error::LedgerError;

/// Advisory-lock key for the tax-filings rebuild, derived from the
/// projection name — the same serialization `rebuild_facts` and
/// `rebuild_payroll` take.
const REBUILD_TAX_FILINGS_LOCK_KEY: i64 = boss_core::rebuild::lock_key("ledger-tax-filings");

#[derive(Debug, Clone)]
pub struct RebuildTaxFilingsReport {
    pub events_scanned: u64,
    pub filings_written: u64,
    pub remittances_applied: u64,
    /// Remittances whose filing isn't in the log. Legitimate on a
    /// trimmed log (the create fell before the baseline) — counted, not
    /// fatal.
    pub remittances_orphaned: u64,
    /// Payloads that didn't deserialize. Skipped with a warning, the
    /// same leniency `rebuild_facts` gives a missing field.
    pub events_skipped_malformed: u64,
}

/// The reconstructable shape of a filing, read straight out of the
/// `ledger.tax.filing.created` payload. `status` is always `accrued` on
/// creation (the live `upsert` hardcodes it), so it isn't carried.
#[derive(Debug, Deserialize)]
struct FilingCreatedPayload {
    filing_id: String,
    kind: String,
    jurisdiction: String,
    period_start: NaiveDate,
    period_end: NaiveDate,
    due_on: NaiveDate,
    amount_cents: i64,
    liability_account: String,
    provider: String,
}

/// The status flip carried by `ledger.tax.remitted`. The payload holds
/// more (amounts, accounts — the fact needs them to post the drain);
/// the projection only needs to know which filing was paid, and when.
#[derive(Debug, Deserialize)]
struct FilingRemittedPayload {
    filing_id: String,
    filed_on: NaiveDate,
}

/// Rebuild tax filings from the log. Opens a transaction, takes the
/// advisory lock, replays, commits.
pub async fn rebuild_tax_filings(pool: &PgPool) -> Result<RebuildTaxFilingsReport, LedgerError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_TAX_FILINGS_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let report = rebuild_tax_filings_in_tx(&mut tx).await?;

    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(report)
}

/// Caller-controlled-transaction variant — symmetric with
/// `rebuild_facts_in_tx` / `rebuild_payroll_in_tx`.
pub async fn rebuild_tax_filings_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<RebuildTaxFilingsReport, LedgerError> {
    // Pure-projection wipe: no filing may live that doesn't trace back
    // to the log. This is the line that kills the prior-lap rows.
    sqlx::query("TRUNCATE tax_filings")
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // Log order, by audit_log id: a remittance must land after the
    // creation it flips. Ordering by `timestamp` instead would be
    // wrong — sim-time can repeat within a lap, and a same-timestamp
    // create/remit pair would replay in an arbitrary order.
    let rows = sqlx::query(
        "SELECT kind, payload FROM audit_log \
         WHERE kind IN ('ledger.tax.filing.created', 'ledger.tax.remitted') \
         ORDER BY id",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut report = RebuildTaxFilingsReport {
        events_scanned: 0,
        filings_written: 0,
        remittances_applied: 0,
        remittances_orphaned: 0,
        events_skipped_malformed: 0,
    };

    for row in &rows {
        report.events_scanned += 1;
        let kind: String = row.get("kind");
        let payload: serde_json::Value = row.get("payload");

        match kind.as_str() {
            "ledger.tax.filing.created" => {
                let filing: FilingCreatedPayload = match serde_json::from_value(payload) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "ledger.tax.filing.created payload unreadable; skipping"
                        );
                        report.events_skipped_malformed += 1;
                        continue;
                    }
                };
                // ON CONFLICT DO NOTHING on both keys: a re-emitted
                // creation (the live path is idempotent on the id, and
                // on the (kind, jurisdiction, period) unique index)
                // must not duplicate or clobber. First write wins,
                // matching the live upsert's short-circuit.
                let done = sqlx::query(
                    "INSERT INTO tax_filings \
                        (id, kind, jurisdiction, period_start, period_end, due_on, \
                         filed_on, amount_cents, liability_account, status, provider) \
                     VALUES ($1, $2, $3, $4, $5, $6, NULL, $7, $8, 'accrued', $9) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(&filing.filing_id)
                .bind(&filing.kind)
                .bind(&filing.jurisdiction)
                .bind(filing.period_start)
                .bind(filing.period_end)
                .bind(filing.due_on)
                .bind(filing.amount_cents)
                .bind(&filing.liability_account)
                .bind(&filing.provider)
                .execute(&mut **tx)
                .await
                .map_err(|e| LedgerError::Storage(e.to_string()))?;
                report.filings_written += done.rows_affected();
            }
            _ => {
                let remit: FilingRemittedPayload = match serde_json::from_value(payload) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "ledger.tax.remitted payload unreadable; skipping"
                        );
                        report.events_skipped_malformed += 1;
                        continue;
                    }
                };
                let done = sqlx::query(
                    "UPDATE tax_filings \
                        SET status = 'paid', filed_on = $2, updated_at = NOW() \
                      WHERE id = $1",
                )
                .bind(&remit.filing_id)
                .bind(remit.filed_on)
                .execute(&mut **tx)
                .await
                .map_err(|e| LedgerError::Storage(e.to_string()))?;
                if done.rows_affected() == 0 {
                    // The create fell before a trim baseline. The remit
                    // fact still rebuilds (rebuild_facts reads the same
                    // event) so the GL drain is not lost — only the
                    // read-model row is unreconstructable.
                    tracing::warn!(
                        filing_id = %remit.filing_id,
                        "ledger.tax.remitted has no filing in the log; skipping status flip"
                    );
                    report.remittances_orphaned += 1;
                } else {
                    report.remittances_applied += done.rows_affected();
                }
            }
        }
    }

    Ok(report)
}
