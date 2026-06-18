//! Bank settlement projection — tracks the float between "account paid
//! an invoice" and "funds cleared into the operating account."
//!
//! One row per inbound account payment. Lifecycle:
//!
//! 1. `create_pending` — called by the invoice-paid write path. Posts
//!    `finance.payment.received` alongside (the caller does that),
//!    records the expected settle date, flips the settlement row to
//!    `pending`.
//! 2. `list_due_pending` — called by the bank-clearing sim generator on
//!    each tick. Returns every pending row whose `expected_settle_on`
//!    is at or before today.
//! 3. `mark_settled` — called by the generator once it's posted
//!    `finance.payment.settled` for a row. Flips the row to `settled`
//!    and stamps `settled_on`.
//!
//! A small share of payments (NSF, wire recall) flip to `returned`
//! instead of `settled`; that path is reserved for the generator's
//! anomaly mode but not exercised in v1.

use chrono::NaiveDate;
use serde::Serialize;
use sqlx::{PgPool, Row};

use crate::error::LedgerError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentMethod {
    Ach,
    Wire,
    Check,
    Card,
}

impl PaymentMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            PaymentMethod::Ach => "ach",
            PaymentMethod::Wire => "wire",
            PaymentMethod::Check => "check",
            PaymentMethod::Card => "card",
        }
    }

    pub fn parse(s: &str) -> Result<Self, LedgerError> {
        match s {
            "ach" => Ok(PaymentMethod::Ach),
            "wire" => Ok(PaymentMethod::Wire),
            "check" => Ok(PaymentMethod::Check),
            "card" => Ok(PaymentMethod::Card),
            other => Err(LedgerError::Storage(format!(
                "unknown payment_method `{other}`"
            ))),
        }
    }

    /// Business-day float by method. Picked to match industry norms — wires
    /// same-day, ACH next business day, card 2d, check slowest. Used only
    /// as a default when the caller doesn't pass a specific `settle_in_days`.
    pub fn default_settle_days(&self) -> i64 {
        match self {
            PaymentMethod::Wire => 0,
            PaymentMethod::Ach => 1,
            PaymentMethod::Card => 2,
            PaymentMethod::Check => 4,
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct BankSettlement {
    pub id: String,
    pub invoice_id: String,
    pub received_on: NaiveDate,
    pub expected_settle_on: NaiveDate,
    pub settled_on: Option<NaiveDate>,
    pub amount_cents: i64,
    pub bank_provider: String,
    pub payment_method: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct NewBankSettlement<'a> {
    pub id: &'a str,
    pub invoice_id: &'a str,
    pub received_on: NaiveDate,
    pub amount_cents: i64,
    pub bank_provider: &'a str,
    pub payment_method: PaymentMethod,
    /// Override for the default method-based settle window. `None` falls
    /// back to `payment_method.default_settle_days()`.
    pub settle_in_days: Option<i64>,
}

/// Insert a fresh pending settlement. Idempotent on `id` — a repeat
/// insert with the same id is a no-op (returns the existing row),
/// which lets sim replays re-hit the same payments without duplicates.
pub async fn create_pending(
    pool: &PgPool,
    new: NewBankSettlement<'_>,
) -> Result<BankSettlement, LedgerError> {
    if new.amount_cents <= 0 {
        return Err(LedgerError::Storage(
            "amount_cents must be positive".to_string(),
        ));
    }
    let days = new
        .settle_in_days
        .unwrap_or_else(|| new.payment_method.default_settle_days());
    let expected = new.received_on + chrono::Duration::days(days);

    let row = sqlx::query(
        "INSERT INTO bank_settlements \
            (id, invoice_id, received_on, expected_settle_on, amount_cents, \
             bank_provider, payment_method, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending') \
         ON CONFLICT (id) DO UPDATE SET updated_at = bank_settlements.updated_at \
         RETURNING id, invoice_id, received_on, expected_settle_on, settled_on, \
                   amount_cents, bank_provider, payment_method, status",
    )
    .bind(new.id)
    .bind(new.invoice_id)
    .bind(new.received_on)
    .bind(expected)
    .bind(new.amount_cents)
    .bind(new.bank_provider)
    .bind(new.payment_method.as_str())
    .fetch_one(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(row_to_settlement(&row))
}

/// All pending settlements whose expected-settle date has arrived
/// (or is in the past — catch-up for missed ticks). Ordered oldest
/// first so the generator's journal entries post in chronological
/// order when it catches up across multiple days.
pub async fn list_due_pending(
    pool: &PgPool,
    as_of: NaiveDate,
) -> Result<Vec<BankSettlement>, LedgerError> {
    let rows = sqlx::query(
        "SELECT id, invoice_id, received_on, expected_settle_on, settled_on, \
                amount_cents, bank_provider, payment_method, status \
         FROM bank_settlements \
         WHERE status = 'pending' AND expected_settle_on <= $1 \
         ORDER BY expected_settle_on ASC, id ASC",
    )
    .bind(as_of)
    .fetch_all(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(rows.iter().map(row_to_settlement).collect())
}

/// Look up a settlement by id; `None` if it doesn't exist.
pub async fn get(pool: &PgPool, id: &str) -> Result<Option<BankSettlement>, LedgerError> {
    let row = sqlx::query(
        "SELECT id, invoice_id, received_on, expected_settle_on, settled_on, \
                amount_cents, bank_provider, payment_method, status \
         FROM bank_settlements WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(row.as_ref().map(row_to_settlement))
}

/// Flip a pending row to settled; the caller has posted the
/// `finance.payment.settled` journal entry separately. No-op (returns
/// the current row) if the row is already settled.
pub async fn mark_settled(
    pool: &PgPool,
    id: &str,
    settled_on: NaiveDate,
) -> Result<BankSettlement, LedgerError> {
    let row = sqlx::query(
        "UPDATE bank_settlements \
         SET status = 'settled', settled_on = $2, updated_at = NOW() \
         WHERE id = $1 AND status = 'pending' \
         RETURNING id, invoice_id, received_on, expected_settle_on, settled_on, \
                   amount_cents, bank_provider, payment_method, status",
    )
    .bind(id)
    .bind(settled_on)
    .fetch_optional(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    match row {
        Some(r) => Ok(row_to_settlement(&r)),
        None => {
            // Either the row doesn't exist or it's already settled / returned.
            // Fall through to a lookup so the caller sees the current state.
            get(pool, id)
                .await?
                .ok_or_else(|| LedgerError::Storage(format!("bank_settlement {id} not found")))
        }
    }
}

fn row_to_settlement(row: &sqlx::postgres::PgRow) -> BankSettlement {
    BankSettlement {
        id: row.get("id"),
        invoice_id: row.get("invoice_id"),
        received_on: row.get("received_on"),
        expected_settle_on: row.get("expected_settle_on"),
        settled_on: row.get("settled_on"),
        amount_cents: row.get("amount_cents"),
        bank_provider: row.get("bank_provider"),
        payment_method: row.get("payment_method"),
        status: row.get("status"),
    }
}
