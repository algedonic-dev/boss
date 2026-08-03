//! Rebuild `bank_settlements` from the `ledger.payment.received` +
//! `ledger.payment.settled` events in `audit_log`.
//!
//! `bank_settlements` is a pure projection of the log: the receive
//! carries the whole row shape, the settle carries the status flip.
//! This projector replays both in log order.
//!
//! Why this exists: before it, `bank_settlements` was live state
//! written by the create/settle handlers and owned by no rebuilder —
//! the same stale-across-epochs class `rebuild_payroll` closed for
//! `payroll_runs` and `rebuild_tax_filings` for `tax_filings`. To
//! survive the commerce rebuilder's `TRUNCATE invoices`, that rebuilder
//! detached every row (`UPDATE bank_settlements SET invoice_id = NULL`),
//! replayed invoices, then re-attached by the deterministic
//! `inv-step-{step_id}` key. After a demo epoch trim the prior lap's
//! invoices are gone forever, so those rows never re-attached: they
//! accumulated with a NULL `invoice_id` and nothing ever deleted them.
//!
//! That was not merely bloat. `row_to_settlement` decodes `invoice_id`
//! into a non-`Option<String>`, so once the sim date passed the point
//! where the previous lap's orphaned pending rows started maturing,
//! every sweep panicked on `UnexpectedNullError` → 500 and AR
//! collections stopped settling for the rest of the lap.
//!
//! Rooted in `audit_log` rather than `financial_facts` — symmetric with
//! `rebuild_tax_filings` and with the other module rebuilders — because
//! the audit events are the canonical record of the payment lifecycle
//! and carry every column the row needs. (`rebuild_payroll` reads facts
//! only because the per-employee lines live nowhere else.)
//!
//! TRUNCATE-then-replay, one transaction, advisory-locked.

use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::{PgPool, Row};

use crate::error::LedgerError;

/// Advisory-lock key for the bank-settlements rebuild, derived from the
/// projection name.
const REBUILD_BANK_SETTLEMENTS_LOCK_KEY: i64 =
    boss_core::rebuild::lock_key("ledger-bank-settlements");

#[derive(Debug, Clone)]
pub struct RebuildBankSettlementsReport {
    pub events_scanned: u64,
    pub settlements_written: u64,
    pub settlements_marked: u64,
    /// Settles whose receive isn't in the log. Legitimate on a trimmed
    /// log (the receive fell before the baseline) — counted, not fatal.
    pub settles_orphaned: u64,
    /// Payloads that didn't deserialize. Skipped with a warning.
    pub events_skipped_malformed: u64,
}

/// The reconstructable shape of a settlement, read straight out of the
/// `ledger.payment.received` payload. `status` is always `pending` on
/// receive (the live `create_pending` hardcodes it), so it isn't
/// carried.
///
/// `expected_settle_on` rides the payload rather than being re-derived
/// from `received_on + payment_method.default_settle_days()`: a caller
/// may pass a `settle_in_days` override, and a re-derivation would then
/// silently disagree with what actually happened — a determinism break
/// (live != rebuilt) of exactly the kind the correctness protocol
/// forbids.
#[derive(Debug, Deserialize)]
struct PaymentReceivedPayload {
    settlement_id: String,
    invoice_id: String,
    received_on: NaiveDate,
    expected_settle_on: NaiveDate,
    amount_cents: i64,
    bank_provider: String,
    payment_method: String,
}

/// The status flip carried by `ledger.payment.settled`.
#[derive(Debug, Deserialize)]
struct PaymentSettledPayload {
    settlement_id: String,
    settled_on: NaiveDate,
}

/// Rebuild bank settlements from the log. Opens a transaction, takes
/// the advisory lock, replays, commits.
pub async fn rebuild_bank_settlements(
    pool: &PgPool,
) -> Result<RebuildBankSettlementsReport, LedgerError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_BANK_SETTLEMENTS_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let report = rebuild_bank_settlements_in_tx(&mut tx).await?;

    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(report)
}

/// Caller-controlled-transaction variant — symmetric with
/// `rebuild_facts_in_tx` / `rebuild_payroll_in_tx` /
/// `rebuild_tax_filings_in_tx`.
pub async fn rebuild_bank_settlements_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<RebuildBankSettlementsReport, LedgerError> {
    // Pure-projection wipe: no settlement may live that doesn't trace
    // back to the log. This is the line that kills the orphans.
    sqlx::query("TRUNCATE bank_settlements")
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // Log order, by audit_log id: a settle must land after the receive
    // it flips. Ordering by `timestamp` instead would be wrong — a
    // wire settles same-day, so receive and settle can share a
    // timestamp and would replay in an arbitrary order.
    let rows = sqlx::query(
        "SELECT kind, payload FROM audit_log \
         WHERE kind IN ('ledger.payment.received', 'ledger.payment.settled') \
         ORDER BY id",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut report = RebuildBankSettlementsReport {
        events_scanned: 0,
        settlements_written: 0,
        settlements_marked: 0,
        settles_orphaned: 0,
        events_skipped_malformed: 0,
    };

    for row in &rows {
        report.events_scanned += 1;
        let kind: String = row.get("kind");
        let payload: serde_json::Value = row.get("payload");

        match kind.as_str() {
            "ledger.payment.received" => {
                let s: PaymentReceivedPayload = match serde_json::from_value(payload) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "ledger.payment.received payload unreadable; skipping"
                        );
                        report.events_skipped_malformed += 1;
                        continue;
                    }
                };
                // ON CONFLICT DO NOTHING: the live create is idempotent
                // on id, so a re-emitted receive must not clobber a row
                // a later settle may already have flipped. First write
                // wins, matching the live short-circuit.
                let done = sqlx::query(
                    "INSERT INTO bank_settlements \
                        (id, invoice_id, received_on, expected_settle_on, settled_on, \
                         amount_cents, bank_provider, payment_method, status) \
                     VALUES ($1, $2, $3, $4, NULL, $5, $6, $7, 'pending') \
                     ON CONFLICT (id) DO NOTHING",
                )
                .bind(&s.settlement_id)
                .bind(&s.invoice_id)
                .bind(s.received_on)
                .bind(s.expected_settle_on)
                .bind(s.amount_cents)
                .bind(&s.bank_provider)
                .bind(&s.payment_method)
                .execute(&mut **tx)
                .await
                .map_err(|e| LedgerError::Storage(e.to_string()))?;
                report.settlements_written += done.rows_affected();
            }
            _ => {
                let s: PaymentSettledPayload = match serde_json::from_value(payload) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "ledger.payment.settled payload unreadable; skipping"
                        );
                        report.events_skipped_malformed += 1;
                        continue;
                    }
                };
                let done = sqlx::query(
                    "UPDATE bank_settlements \
                        SET status = 'settled', settled_on = $2, updated_at = NOW() \
                      WHERE id = $1",
                )
                .bind(&s.settlement_id)
                .bind(s.settled_on)
                .execute(&mut **tx)
                .await
                .map_err(|e| LedgerError::Storage(e.to_string()))?;
                if done.rows_affected() == 0 {
                    // The receive fell before a trim baseline. The
                    // settled fact still rebuilds (rebuild_facts reads
                    // the same event) so the GL is not affected — only
                    // the read-model row is unreconstructable.
                    tracing::warn!(
                        settlement_id = %s.settlement_id,
                        "ledger.payment.settled has no receive in the log; skipping status flip"
                    );
                    report.settles_orphaned += 1;
                } else {
                    report.settlements_marked += done.rows_affected();
                }
            }
        }
    }

    Ok(report)
}
