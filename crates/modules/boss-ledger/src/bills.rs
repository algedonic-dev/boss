//! Ledger bills — the general accounts-payable subledger owned by the GL.
//!
//! A "bill" here is a general AP obligation (rent, utilities, insurance,
//! services, …), decoupled from the inventory parts vendor-invoice in
//! `boss-inventory`. The free `bill_category` is routed to a debit account
//! by `bill_accounts.toml` (`boss-ledger/seeds/`); the credit is always
//! 2100 A/P. The `lines` field is an opaque metadata bag — no `part_sku`
//! coupling.
//!
//! A bill's life moves `approved` → `paid`:
//! - **approve** posts the accrual via the existing `finance.bill.approved`
//!   rule (DR `<bill_account(category)>` / CR 2100 A/P);
//! - **pay** drains it via `finance.bill.paid` (DR 2100 / CR 1000 Cash).
//!
//! Both posting rules in `rules.rs` are reused UNCHANGED — this module only
//! owns the subledger row + its lifecycle. Adding a new kind of spend is a
//! JobKind writing a `bill_category` + a `bill_accounts.toml` row, no code.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::error::LedgerError;

/// The fact kind a bill validation failure reports against (both
/// lifecycle transitions share the `finance.bill.*` family).
const BILL_KIND: &str = "finance.bill.approved";

const BILL_COLS: &str = "id, vendor, bill_category, amount_cents, currency, \
     issued_on, due_on, approved_on, paid_on, status, lines, memo";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bill {
    pub id: String,
    pub vendor: String,
    /// Free text; routed to a GL debit account by `bill_accounts.toml`.
    pub bill_category: String,
    pub amount_cents: i64,
    pub currency: String,
    pub issued_on: NaiveDate,
    pub due_on: Option<NaiveDate>,
    pub approved_on: Option<NaiveDate>,
    pub paid_on: Option<NaiveDate>,
    pub status: String,
    /// Opaque per-bill metadata bag (no part_sku). Defaults to `[]`.
    pub lines: Value,
    pub memo: Option<String>,
}

/// Borrowed insert shape for approving a new bill.
#[derive(Debug, Clone)]
pub struct NewBill<'a> {
    pub id: &'a str,
    pub vendor: &'a str,
    pub bill_category: &'a str,
    pub amount_cents: i64,
    pub currency: &'a str,
    pub issued_on: NaiveDate,
    pub due_on: Option<NaiveDate>,
    /// Approval date — becomes the `happened_on` of the `finance.bill.approved`
    /// JE. A bill lands already-approved (the `expense-bill` step *is* the
    /// approval).
    pub approved_on: NaiveDate,
    pub lines: &'a Value,
    pub memo: Option<&'a str>,
    pub created_by: &'a str,
}

fn validate_new(new: &NewBill<'_>) -> Result<(), LedgerError> {
    if new.amount_cents <= 0 {
        return Err(LedgerError::InvalidPayload {
            kind: BILL_KIND.to_string(),
            reason: format!("amount_cents must be positive; got {}", new.amount_cents),
        });
    }
    if new.vendor.trim().is_empty() {
        return Err(LedgerError::InvalidPayload {
            kind: BILL_KIND.to_string(),
            reason: "vendor must not be empty".to_string(),
        });
    }
    if new.bill_category.trim().is_empty() {
        return Err(LedgerError::InvalidPayload {
            kind: BILL_KIND.to_string(),
            reason: "bill_category must not be empty".to_string(),
        });
    }
    if new.currency.len() != 3 {
        return Err(LedgerError::InvalidPayload {
            kind: BILL_KIND.to_string(),
            reason: format!("currency must be a 3-letter code; got `{}`", new.currency),
        });
    }
    Ok(())
}

/// Insert (or reuse) an approved bill row inside the caller's tx — so the
/// row, its `finance.bill.approved` fact, and the journal entry all land
/// (or roll back) atomically. Idempotent on the PK `id`, so a replay of
/// the same `expense-bill` step is a no-op that returns the existing row.
pub async fn upsert_approved_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    new: NewBill<'_>,
) -> Result<Bill, LedgerError> {
    validate_new(&new)?;

    let sql = format!(
        "INSERT INTO ledger_bills \
            (id, vendor, bill_category, amount_cents, currency, issued_on, \
             due_on, approved_on, paid_on, status, lines, memo, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, 'approved', $9, $10, $11) \
         ON CONFLICT (id) DO UPDATE SET updated_at = NOW() \
         RETURNING {BILL_COLS}"
    );
    let row = sqlx::query(&sql)
        .bind(new.id)
        .bind(new.vendor)
        .bind(new.bill_category)
        .bind(new.amount_cents)
        .bind(new.currency)
        .bind(new.issued_on)
        .bind(new.due_on)
        .bind(new.approved_on)
        .bind(new.lines)
        .bind(new.memo)
        .bind(new.created_by)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(row_to_bill(&row))
}

/// Flip an approved bill to `paid` inside the caller's tx (atomic with the
/// `finance.bill.paid` fact + drain JE). Returns `None` if no such bill, so
/// the HTTP handler can 404.
pub async fn mark_paid_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    paid_on: NaiveDate,
) -> Result<Option<Bill>, LedgerError> {
    let sql = format!(
        "UPDATE ledger_bills \
            SET status = 'paid', paid_on = $2, updated_at = NOW() \
          WHERE id = $1 AND status = 'approved' \
          RETURNING {BILL_COLS}"
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(paid_on)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(row.as_ref().map(row_to_bill))
}

pub async fn get(pool: &PgPool, id: &str) -> Result<Option<Bill>, LedgerError> {
    let sql = format!("SELECT {BILL_COLS} FROM ledger_bills WHERE id = $1");
    let row = sqlx::query(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(row.as_ref().map(row_to_bill))
}

/// List bills, optionally filtered by status, newest-issued first.
pub async fn list(
    pool: &PgPool,
    status: Option<&str>,
    limit: i64,
) -> Result<Vec<Bill>, LedgerError> {
    let sql = format!(
        "SELECT {BILL_COLS} FROM ledger_bills \
         WHERE ($1::TEXT IS NULL OR status = $1) \
         ORDER BY issued_on DESC, id \
         LIMIT $2"
    );
    let rows = sqlx::query(&sql)
        .bind(status)
        .bind(limit.clamp(1, 5000))
        .fetch_all(pool)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(rows.iter().map(row_to_bill).collect())
}

fn row_to_bill(row: &sqlx::postgres::PgRow) -> Bill {
    Bill {
        id: row.get("id"),
        vendor: row.get("vendor"),
        bill_category: row.get("bill_category"),
        amount_cents: row.get("amount_cents"),
        currency: row.get("currency"),
        issued_on: row.get("issued_on"),
        due_on: row.get("due_on"),
        approved_on: row.get("approved_on"),
        paid_on: row.get("paid_on"),
        status: row.get("status"),
        lines: row.get("lines"),
        memo: row.get("memo"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid<'a>(lines: &'a Value) -> NewBill<'a> {
        NewBill {
            id: "bill-1",
            vendor: "Acme Property Mgmt",
            bill_category: "rent",
            amount_cents: 120_000_00,
            currency: "USD",
            issued_on: "2025-04-01".parse().unwrap(),
            due_on: Some("2025-04-15".parse().unwrap()),
            approved_on: "2025-04-01".parse().unwrap(),
            created_by: "test",
            lines,
            memo: Some("April warehouse lease"),
        }
    }

    #[test]
    fn accepts_a_well_formed_bill() {
        let lines = json!([]);
        assert!(validate_new(&valid(&lines)).is_ok());
    }

    #[test]
    fn rejects_non_positive_amount() {
        let lines = json!([]);
        let mut b = valid(&lines);
        b.amount_cents = 0;
        assert!(validate_new(&b).is_err());
        b.amount_cents = -5;
        assert!(validate_new(&b).is_err());
    }

    #[test]
    fn rejects_empty_vendor_or_category() {
        let lines = json!([]);
        let mut b = valid(&lines);
        b.vendor = "   ";
        assert!(validate_new(&b).is_err());
        let mut b = valid(&lines);
        b.bill_category = "";
        assert!(validate_new(&b).is_err());
    }

    #[test]
    fn rejects_bad_currency() {
        let lines = json!([]);
        let mut b = valid(&lines);
        b.currency = "DOLLARS";
        assert!(validate_new(&b).is_err());
    }
}
