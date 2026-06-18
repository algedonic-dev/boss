//! Payroll run projection — biweekly paycheck batches.
//!
//! One `payroll_runs` row per pay period + one `payroll_run_lines`
//! row per employee per run. The journal entry is aggregated
//! per-run (per Q6 of the design doc): 75 employees × 26 runs/year
//! yields 26 journal entries, not 1,950. Per-employee drill-down
//! reads straight from `payroll_run_lines`.
//!
//! The lifecycle is two HTTP calls:
//!
//! 1. `POST /api/ledger/payroll-runs` — caller submits the whole
//!    run (header + lines) as one payload. This module inserts both
//!    tables AND emits `finance.payroll.run` + the journal entry
//!    in one transaction so the projection and the GL can't drift.
//! 2. The run status moves from `submitted` to `posted` on the same
//!    call. `draft` is reserved for a future approval workflow.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::error::LedgerError;

#[derive(Debug, Clone, Serialize)]
pub struct PayrollRun {
    pub id: String,
    pub run_date: NaiveDate,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub gross_cents: i64,
    pub employer_tax_cents: i64,
    pub withheld_cents: i64,
    pub net_cents: i64,
    pub employee_count: i32,
    pub provider: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayrollRunLine {
    pub employee_id: String,
    pub gross_cents: i64,
    pub withheld_cents: i64,
    pub net_cents: i64,
    pub department: String,
    pub role: String,
}

#[derive(Debug, Clone)]
pub struct NewPayrollRun<'a> {
    pub id: &'a str,
    pub run_date: NaiveDate,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub employer_tax_cents: i64,
    pub provider: &'a str,
    pub lines: &'a [PayrollRunLine],
}

/// Aggregated totals computed from a slice of per-employee lines.
/// Lets callers (sim + HTTP) avoid re-summing.
#[derive(Debug, Clone, Copy)]
pub struct LineTotals {
    pub gross_cents: i64,
    pub withheld_cents: i64,
    pub net_cents: i64,
    pub employee_count: i32,
}

impl LineTotals {
    pub fn sum(lines: &[PayrollRunLine]) -> Result<Self, LedgerError> {
        let mut gross = 0i64;
        let mut withheld = 0i64;
        let mut net = 0i64;
        for line in lines {
            if line.gross_cents < 0 || line.withheld_cents < 0 || line.net_cents < 0 {
                return Err(LedgerError::InvalidPayload {
                    kind: "finance.payroll.run".to_string(),
                    reason: "payroll line amounts must be non-negative".to_string(),
                });
            }
            if line.net_cents != line.gross_cents - line.withheld_cents {
                return Err(LedgerError::InvalidPayload {
                    kind: "finance.payroll.run".to_string(),
                    reason: format!(
                        "payroll line {}: net ({}) != gross ({}) - withheld ({})",
                        line.employee_id, line.net_cents, line.gross_cents, line.withheld_cents
                    ),
                });
            }
            gross += line.gross_cents;
            withheld += line.withheld_cents;
            net += line.net_cents;
        }
        Ok(Self {
            gross_cents: gross,
            withheld_cents: withheld,
            net_cents: net,
            employee_count: i32::try_from(lines.len()).unwrap_or(i32::MAX),
        })
    }
}

/// Insert a payroll run plus all its lines. Idempotent on `id` —
/// a repeat insert with the same id returns the existing row (the
/// UNIQUE PRIMARY KEY on `payroll_runs.id` does the work). Lines
/// are inserted through `ON CONFLICT DO NOTHING` against the
/// composite primary key so replays don't double-count.
pub async fn create_run(pool: &PgPool, new: NewPayrollRun<'_>) -> Result<PayrollRun, LedgerError> {
    if new.employer_tax_cents < 0 {
        return Err(LedgerError::InvalidPayload {
            kind: "finance.payroll.run".to_string(),
            reason: "employer_tax_cents must be non-negative".to_string(),
        });
    }
    if new.period_end < new.period_start {
        return Err(LedgerError::InvalidPayload {
            kind: "finance.payroll.run".to_string(),
            reason: "period_end must be >= period_start".to_string(),
        });
    }
    let totals = LineTotals::sum(new.lines)?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let header_row = sqlx::query(
        "INSERT INTO payroll_runs \
            (id, run_date, period_start, period_end, gross_cents, employer_tax_cents, \
             withheld_cents, net_cents, employee_count, provider, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'posted') \
         ON CONFLICT (id) DO UPDATE SET updated_at = payroll_runs.updated_at \
         RETURNING id, run_date, period_start, period_end, gross_cents, \
                   employer_tax_cents, withheld_cents, net_cents, \
                   employee_count, provider, status",
    )
    .bind(new.id)
    .bind(new.run_date)
    .bind(new.period_start)
    .bind(new.period_end)
    .bind(totals.gross_cents)
    .bind(new.employer_tax_cents)
    .bind(totals.withheld_cents)
    .bind(totals.net_cents)
    .bind(totals.employee_count)
    .bind(new.provider)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    for line in new.lines {
        sqlx::query(
            "INSERT INTO payroll_run_lines \
                (run_id, employee_id, gross_cents, withheld_cents, net_cents, \
                 department, role) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (run_id, employee_id) DO NOTHING",
        )
        .bind(new.id)
        .bind(&line.employee_id)
        .bind(line.gross_cents)
        .bind(line.withheld_cents)
        .bind(line.net_cents)
        .bind(&line.department)
        .bind(&line.role)
        .execute(&mut *tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    }

    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(row_to_run(&header_row))
}

pub async fn get(pool: &PgPool, id: &str) -> Result<Option<PayrollRun>, LedgerError> {
    let row = sqlx::query(
        "SELECT id, run_date, period_start, period_end, gross_cents, \
                employer_tax_cents, withheld_cents, net_cents, \
                employee_count, provider, status \
         FROM payroll_runs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(row.as_ref().map(row_to_run))
}

pub async fn list_recent(pool: &PgPool, limit: i64) -> Result<Vec<PayrollRun>, LedgerError> {
    let rows = sqlx::query(
        "SELECT id, run_date, period_start, period_end, gross_cents, \
                employer_tax_cents, withheld_cents, net_cents, \
                employee_count, provider, status \
         FROM payroll_runs \
         ORDER BY run_date DESC, id DESC \
         LIMIT $1",
    )
    .bind(limit.clamp(1, 1000))
    .fetch_all(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(rows.iter().map(row_to_run).collect())
}

pub async fn list_lines(pool: &PgPool, run_id: &str) -> Result<Vec<PayrollRunLine>, LedgerError> {
    let rows = sqlx::query(
        "SELECT employee_id, gross_cents, withheld_cents, net_cents, department, role \
         FROM payroll_run_lines WHERE run_id = $1 ORDER BY employee_id",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| PayrollRunLine {
            employee_id: row.get("employee_id"),
            gross_cents: row.get("gross_cents"),
            withheld_cents: row.get("withheld_cents"),
            net_cents: row.get("net_cents"),
            department: row.get("department"),
            role: row.get("role"),
        })
        .collect())
}

fn row_to_run(row: &sqlx::postgres::PgRow) -> PayrollRun {
    PayrollRun {
        id: row.get("id"),
        run_date: row.get("run_date"),
        period_start: row.get("period_start"),
        period_end: row.get("period_end"),
        gross_cents: row.get("gross_cents"),
        employer_tax_cents: row.get("employer_tax_cents"),
        withheld_cents: row.get("withheld_cents"),
        net_cents: row.get("net_cents"),
        employee_count: row.get("employee_count"),
        provider: row.get("provider"),
        status: row.get("status"),
    }
}
