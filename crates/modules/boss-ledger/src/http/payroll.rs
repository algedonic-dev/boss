use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::CurrentUser;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::*;

// --- payroll runs ---------------------------------------------------------

/// Create a biweekly payroll run. Writes the header + per-employee lines
/// AND emits `finance.payroll.run` + posts the compound journal entry in
/// one transaction. Idempotent on `id` — a repeat POST with the same run
/// id returns the existing row without double-posting.
#[derive(Deserialize)]
pub(super) struct CreatePayrollRunBody {
    id: String,
    run_date: NaiveDate,
    period_start: NaiveDate,
    period_end: NaiveDate,
    employer_tax_cents: i64,
    #[serde(default = "default_provider")]
    provider: String,
    lines: Vec<crate::payroll::PayrollRunLine>,
}

fn default_provider() -> String {
    "adp".to_string()
}

#[derive(Serialize)]
struct PayrollRunView {
    id: String,
    run_date: NaiveDate,
    period_start: NaiveDate,
    period_end: NaiveDate,
    gross_cents: i64,
    employer_tax_cents: i64,
    withheld_cents: i64,
    net_cents: i64,
    employee_count: i32,
    provider: String,
    status: String,
}

impl From<crate::payroll::PayrollRun> for PayrollRunView {
    fn from(r: crate::payroll::PayrollRun) -> Self {
        Self {
            id: r.id,
            run_date: r.run_date,
            period_start: r.period_start,
            period_end: r.period_end,
            gross_cents: r.gross_cents,
            employer_tax_cents: r.employer_tax_cents,
            withheld_cents: r.withheld_cents,
            net_cents: r.net_cents,
            employee_count: r.employee_count,
            provider: r.provider,
            status: r.status,
        }
    }
}

#[derive(Serialize)]
struct PayrollRunDetail {
    #[serde(flatten)]
    run: PayrollRunView,
    lines: Vec<crate::payroll::PayrollRunLine>,
}

pub(super) async fn create_payroll_run(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreatePayrollRunBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if body.lines.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "lines must not be empty".to_string(),
        )
            .into_response();
    }

    // Idempotency: if the run already exists, return it without emitting
    // a second fact or double-posting the journal entry.
    if let Ok(Some(existing)) = crate::payroll::get(&state.pool, &body.id).await {
        return Json(PayrollRunView::from(existing)).into_response();
    }

    let run = match crate::payroll::create_run(
        &state.pool,
        crate::payroll::NewPayrollRun {
            id: &body.id,
            run_date: body.run_date,
            period_start: body.period_start,
            period_end: body.period_end,
            employer_tax_cents: body.employer_tax_cents,
            provider: &body.provider,
            lines: &body.lines,
        },
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return ledger_err(e),
    };

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return storage_err(e),
    };

    // Carry the per-employee lines in the fact payload so payroll_runs
    // AND payroll_run_lines are both pure projections of the log
    // (rebuilt by crate::rebuild_payroll). The aggregate JE rule reads
    // only the totals, so the GL is unaffected.
    let lines_json = match serde_json::to_value(&body.lines) {
        Ok(v) => v,
        Err(e) => {
            return ledger_err(crate::error::LedgerError::Storage(format!(
                "serialize payroll lines: {e}"
            )));
        }
    };
    let payload = serde_json::json!({
        "run_id": run.id,
        "run_date": run.run_date,
        "period_start": run.period_start,
        "period_end": run.period_end,
        "gross_cents": run.gross_cents,
        "withheld_cents": run.withheld_cents,
        "employer_tax_cents": run.employer_tax_cents,
        "net_cents": run.net_cents,
        "employee_count": run.employee_count,
        "provider": run.provider,
        "lines": lines_json,
    });
    let live_fact_id = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.payroll.run",
            happened_on: run.run_date,
            payload: &payload,
            source_table: Some("payroll_runs"),
            source_id: Some(&run.id),
            // Matches the event source ("ledger") — the projection's
            // created_by fallback — so rebuilt facts match live ones.
            created_by: "ledger",
        },
    )
    .await
    {
        Ok(rec) => rec.id,
        Err(e) => return ledger_err(e),
    };

    let fact = crate::types::FactRef {
        id: live_fact_id,
        kind: "finance.payroll.run",
        happened_on: run.run_date,
        payload: &payload,
    };
    if let Err(e) = crate::postgres::post_fact_in_tx(&mut tx, &fact).await {
        return ledger_err(e);
    }

    // Outbox phase 2: the audit event records in the SAME tx as the
    // fact + JE, so a crash can no longer commit the run without its
    // rebuild source.
    {
        let now = boss_clock_client::now_from(&state.clock).await;
        let stamp = super::event_stamp(&state, &user, now).await;
        if let Err(e) = crate::events::record_ledger_event_in_tx(
            &mut tx,
            &stamp,
            "ledger.payroll.run",
            payload.clone(),
        )
        .await
        {
            return ledger_err(e);
        }
    }

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    Json(PayrollRunView::from(run)).into_response()
}

/// `POST /api/ledger/payroll-runs/synthesize` — server-side
/// payroll computation for the simulator path.
///
/// The brewery sim's `payroll-run` JobKind walks
/// calculate → review → release; the terminal `payroll-release`
/// step's `ledger.payroll.run.submit` side-effect handler emits
/// a `ledger.payroll.run.submit` event whose payload (this
/// `SynthesizePayrollBody`) lands here. The handler does NOT
/// know per-employee gross / withheld / net; that calculation
/// runs server-side against the live `employees` projection so
/// the lines reflect actual headcount + comp at the run date.
///
/// Computation: gross = annual_salary / periods_per_year per active
/// full-time employee with a salary; withheld = gross ×
/// withholding_bps / 10_000; employer_tax = sum_gross ×
/// employer_cost_bps / 10_000. Same canonical event +
/// `finance.payroll.run` fact + journal entry as the manual
/// POST path.
///
/// Idempotency: derives the run id from `run_date` (e.g.
/// `payroll-2026-04-09`) so re-firing the same step doesn't
/// double-create.
#[derive(Deserialize)]
pub(super) struct SynthesizePayrollBody {
    run_date: NaiveDate,
    period_start: NaiveDate,
    period_end: NaiveDate,
    /// Pay periods per year — drives `gross / N` per run. 26 =
    /// biweekly, 52 = weekly, 24 = semi-monthly, 12 = monthly.
    periods_per_year: u32,
    /// Employee-side withholding as basis points of gross.
    /// 2200 = 22% blended (federal + state + FICA).
    withholding_bps: i64,
    /// Employer-side cost as basis points of gross — FICA match
    /// + FUTA + SUTA + benefit bundle blended. 1500 = 15%.
    employer_cost_bps: i64,
    #[serde(default = "default_provider")]
    provider: String,
}

pub(super) async fn synthesize_payroll_run(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<SynthesizePayrollBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if body.periods_per_year == 0 {
        return (
            StatusCode::BAD_REQUEST,
            "periods_per_year must be > 0".to_string(),
        )
            .into_response();
    }
    if body.period_end < body.period_start {
        return (
            StatusCode::BAD_REQUEST,
            "period_end must be >= period_start".to_string(),
        )
            .into_response();
    }

    // Deterministic id from run_date — re-firing the same step
    // returns the existing run rather than double-posting.
    let id = format!("payroll-{}", body.run_date.format("%Y%m%d"));

    if let Ok(Some(existing)) = crate::payroll::get(&state.pool, &id).await {
        return Json(PayrollRunView::from(existing)).into_response();
    }

    // Pull active full-time employees with a real salary. Same
    // skip rules the deleted `boss-people-batch::PayrollRule`
    // enforced: no contractors (paid through AP), no missing
    // salaries (don't invent a paycheck), no terminated
    // employees.
    let rows: Vec<(String, String, String, Option<i64>)> = match sqlx::query_as(
        "SELECT id, role, department, annual_salary_cents \
         FROM employees \
         WHERE status = 'active' \
           AND employment_type != 'contractor' \
           AND annual_salary_cents IS NOT NULL",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };

    if rows.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "no eligible employees in the live roster — \
             check status='active' + employment_type filter"
                .to_string(),
        )
            .into_response();
    }

    let periods = body.periods_per_year as i64;
    let lines: Vec<crate::payroll::PayrollRunLine> = rows
        .into_iter()
        .filter_map(|(emp_id, role, dept, salary)| {
            let salary = salary?;
            let gross = salary / periods;
            let withheld = (gross * body.withholding_bps) / 10_000;
            let net = gross - withheld;
            Some(crate::payroll::PayrollRunLine {
                employee_id: emp_id,
                role,
                department: dept,
                gross_cents: gross,
                withheld_cents: withheld,
                net_cents: net,
            })
        })
        .collect();

    let total_gross: i64 = lines.iter().map(|l| l.gross_cents).sum();
    let employer_tax = (total_gross * body.employer_cost_bps) / 10_000;

    // From here on this mirrors create_payroll_run's body almost
    // verbatim — same persistence + fact emission + journal-entry
    // posting + after-commit canonical event publish. Kept
    // duplicated for now (the two callers' validation differs;
    // refactor when a third caller appears).
    let run = match crate::payroll::create_run(
        &state.pool,
        crate::payroll::NewPayrollRun {
            id: &id,
            run_date: body.run_date,
            period_start: body.period_start,
            period_end: body.period_end,
            employer_tax_cents: employer_tax,
            provider: &body.provider,
            lines: &lines,
        },
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return ledger_err(e),
    };

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return storage_err(e),
    };

    // Carry the per-employee lines in the fact payload so payroll_runs
    // AND payroll_run_lines are both pure projections of the log
    // (rebuilt by crate::rebuild_payroll). The aggregate JE rule reads
    // only the totals, so the GL is unaffected.
    let lines_json = match serde_json::to_value(&lines) {
        Ok(v) => v,
        Err(e) => {
            return ledger_err(crate::error::LedgerError::Storage(format!(
                "serialize payroll lines: {e}"
            )));
        }
    };
    let payload = serde_json::json!({
        "run_id": run.id,
        "run_date": run.run_date,
        "period_start": run.period_start,
        "period_end": run.period_end,
        "gross_cents": run.gross_cents,
        "withheld_cents": run.withheld_cents,
        "employer_tax_cents": run.employer_tax_cents,
        "net_cents": run.net_cents,
        "employee_count": run.employee_count,
        "provider": run.provider,
        "lines": lines_json,
    });
    let live_fact_id = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.payroll.run",
            happened_on: run.run_date,
            payload: &payload,
            source_table: Some("payroll_runs"),
            source_id: Some(&run.id),
            // Matches the event source ("ledger") — the projection's
            // created_by fallback — so rebuilt facts match live ones.
            created_by: "ledger",
        },
    )
    .await
    {
        Ok(rec) => rec.id,
        Err(e) => return ledger_err(e),
    };

    let fact = crate::types::FactRef {
        id: live_fact_id,
        kind: "finance.payroll.run",
        happened_on: run.run_date,
        payload: &payload,
    };
    if let Err(e) = crate::postgres::post_fact_in_tx(&mut tx, &fact).await {
        return ledger_err(e);
    }

    // Outbox phase 2: the audit event records in the SAME tx as the
    // fact + JE, so a crash can no longer commit the run without its
    // rebuild source.
    {
        let now = boss_clock_client::now_from(&state.clock).await;
        let stamp = super::event_stamp(&state, &user, now).await;
        if let Err(e) = crate::events::record_ledger_event_in_tx(
            &mut tx,
            &stamp,
            "ledger.payroll.run",
            payload.clone(),
        )
        .await
        {
            return ledger_err(e);
        }
    }

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    Json(PayrollRunView::from(run)).into_response()
}

#[derive(Deserialize)]
pub(super) struct ListPayrollRunsQuery {
    #[serde(default)]
    limit: Option<i64>,
}

pub(super) async fn list_payroll_runs(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<ListPayrollRunsQuery>,
) -> Response {
    match crate::payroll::list_recent(&state.pool, q.limit.unwrap_or(50)).await {
        Ok(runs) => {
            let views: Vec<PayrollRunView> = runs.into_iter().map(PayrollRunView::from).collect();
            Json(views).into_response()
        }
        Err(e) => ledger_err(e),
    }
}

pub(super) async fn get_payroll_run(
    State(state): State<Arc<LedgerApiState>>,
    Path(id): Path<String>,
) -> Response {
    let run = match crate::payroll::get(&state.pool, &id).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found".to_string()).into_response(),
        Err(e) => return ledger_err(e),
    };
    let lines = match crate::payroll::list_lines(&state.pool, &id).await {
        Ok(ls) => ls,
        Err(e) => return ledger_err(e),
    };
    Json(PayrollRunDetail {
        run: PayrollRunView::from(run),
        lines,
    })
    .into_response()
}
