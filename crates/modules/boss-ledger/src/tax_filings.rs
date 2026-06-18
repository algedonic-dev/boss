//! Tax filings projection.
//!
//! One `tax_filings` row per (kind, jurisdiction, period) tuple:
//!
//! - **sales** — monthly per-state. The `tax_authorities` sim generator
//!   aggregates every `2300 Sales Tax Payable` credit posted during a
//!   month, groups by jurisdiction, writes one `tax_filings` row per
//!   jurisdiction with `due_on = 20th of the following month`.
//! - **payroll_941** — quarterly federal payroll tax (employee
//!   withholding + employer-side FICA/Medicare/FUTA). Drains 2150.
//! - **payroll_940** — annual federal unemployment tax. Also 2150.
//! - **income** — quarterly estimated corporate income tax (US-FEDERAL
//!   + single-state in v1). Drains 2310.
//!
//! A filing's life moves `accrued` → `filed` → `paid`. The rule sheet
//! only engages when the status flips to `paid`: that's when the
//! `finance.tax.remitted` fact posts + the liability drains to 1000
//! Cash. `filed` is carried for shape even though today's flow moves
//! accrued → paid in one HTTP call; a future integration (self-file
//! via Avalara vs. pay-later through ACH) would separate the two.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::error::LedgerError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxFiling {
    pub id: String,
    pub kind: String,
    pub jurisdiction: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub due_on: NaiveDate,
    pub filed_on: Option<NaiveDate>,
    pub amount_cents: i64,
    pub liability_account: String,
    pub status: String,
    pub provider: String,
}

#[derive(Debug, Clone)]
pub struct NewTaxFiling<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub jurisdiction: &'a str,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub due_on: NaiveDate,
    pub amount_cents: i64,
    pub liability_account: &'a str,
    pub provider: &'a str,
}

fn validate_new(new: &NewTaxFiling<'_>) -> Result<(), LedgerError> {
    if new.amount_cents <= 0 {
        return Err(LedgerError::InvalidPayload {
            kind: "finance.tax.remitted".to_string(),
            reason: "amount_cents must be positive".to_string(),
        });
    }
    if new.period_end < new.period_start {
        return Err(LedgerError::InvalidPayload {
            kind: "finance.tax.remitted".to_string(),
            reason: "period_end must be >= period_start".to_string(),
        });
    }
    // kind validity is enforced by the FK on tax_filings.kind ->
    // tax_kinds(kind), and liability_account is resolved from
    // tax_kinds by the HTTP handler — so no closed allow-list lives
    // here (lifting the tax taxonomy out of core code into data).
    Ok(())
}

/// Insert (or reuse) a tax filing row. Idempotent on the PK id and on
/// the `(kind, jurisdiction, period_start, period_end)` unique index.
/// Returns the row that ended up in the table so replay callers can
/// short-circuit without a second round trip.
pub async fn upsert(pool: &PgPool, new: NewTaxFiling<'_>) -> Result<TaxFiling, LedgerError> {
    validate_new(&new)?;

    let row = sqlx::query(
        "INSERT INTO tax_filings \
            (id, kind, jurisdiction, period_start, period_end, due_on, \
             filed_on, amount_cents, liability_account, status, provider) \
         VALUES ($1, $2, $3, $4, $5, $6, NULL, $7, $8, 'accrued', $9) \
         ON CONFLICT (kind, jurisdiction, period_start, period_end) \
         DO UPDATE SET updated_at = NOW() \
         RETURNING id, kind, jurisdiction, period_start, period_end, due_on, \
                   filed_on, amount_cents, liability_account, status, provider",
    )
    .bind(new.id)
    .bind(new.kind)
    .bind(new.jurisdiction)
    .bind(new.period_start)
    .bind(new.period_end)
    .bind(new.due_on)
    .bind(new.amount_cents)
    .bind(new.liability_account)
    .bind(new.provider)
    .fetch_one(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(row_to_filing(&row))
}

/// Flip an accrued filing to `paid`. Called from the HTTP handler right
/// after the `finance.tax.remitted` fact is inserted and posted.
pub async fn mark_paid(
    pool: &PgPool,
    id: &str,
    filed_on: NaiveDate,
) -> Result<TaxFiling, LedgerError> {
    let row = sqlx::query(
        "UPDATE tax_filings \
            SET status = 'paid', filed_on = $2, updated_at = NOW() \
          WHERE id = $1 \
          RETURNING id, kind, jurisdiction, period_start, period_end, due_on, \
                    filed_on, amount_cents, liability_account, status, provider",
    )
    .bind(id)
    .bind(filed_on)
    .fetch_one(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(row_to_filing(&row))
}

pub async fn get(pool: &PgPool, id: &str) -> Result<Option<TaxFiling>, LedgerError> {
    let row = sqlx::query(
        "SELECT id, kind, jurisdiction, period_start, period_end, due_on, \
                filed_on, amount_cents, liability_account, status, provider \
         FROM tax_filings WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(row.as_ref().map(row_to_filing))
}

/// List filings, optionally filtered by status. Ordered by due date
/// descending (most recent obligations first).
pub async fn list(
    pool: &PgPool,
    status: Option<&str>,
    limit: i64,
) -> Result<Vec<TaxFiling>, LedgerError> {
    let rows = sqlx::query(
        "SELECT id, kind, jurisdiction, period_start, period_end, due_on, \
                filed_on, amount_cents, liability_account, status, provider \
         FROM tax_filings \
         WHERE ($1::TEXT IS NULL OR status = $1) \
         ORDER BY due_on DESC, id \
         LIMIT $2",
    )
    .bind(status)
    .bind(limit.clamp(1, 1000))
    .fetch_all(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(rows.iter().map(row_to_filing).collect())
}

fn row_to_filing(row: &sqlx::postgres::PgRow) -> TaxFiling {
    TaxFiling {
        id: row.get("id"),
        kind: row.get("kind"),
        jurisdiction: row.get("jurisdiction"),
        period_start: row.get("period_start"),
        period_end: row.get("period_end"),
        due_on: row.get("due_on"),
        filed_on: row.get("filed_on"),
        amount_cents: row.get("amount_cents"),
        liability_account: row.get("liability_account"),
        status: row.get("status"),
        provider: row.get("provider"),
    }
}
