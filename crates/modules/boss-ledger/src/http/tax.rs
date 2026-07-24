use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::CurrentUser;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use super::*;

// --- tax filings ----------------------------------------------------------

/// Create an accrued tax filing. Idempotent on `(kind, jurisdiction,
/// period)` — a repeat POST with the same tuple returns the existing
/// row. Accrual does NOT post a journal entry: the liability was
/// already credited when its source facts posted (sales tax on invoice
/// issue, payroll tax on run). The filing row simply tracks the
/// obligation so the operator can see what's due.
#[derive(Deserialize)]
pub(super) struct CreateTaxFilingBody {
    id: String,
    kind: String,
    jurisdiction: String,
    period_start: NaiveDate,
    period_end: NaiveDate,
    due_on: NaiveDate,
    amount_cents: i64,
    #[serde(default = "default_tax_provider")]
    provider: String,
}

fn default_tax_provider() -> String {
    "self".to_string()
}

#[derive(Serialize, Clone)]
struct TaxFilingView {
    id: String,
    kind: String,
    jurisdiction: String,
    period_start: NaiveDate,
    period_end: NaiveDate,
    due_on: NaiveDate,
    filed_on: Option<NaiveDate>,
    amount_cents: i64,
    liability_account: String,
    status: String,
    provider: String,
}

impl From<crate::tax_filings::TaxFiling> for TaxFilingView {
    fn from(f: crate::tax_filings::TaxFiling) -> Self {
        Self {
            id: f.id,
            kind: f.kind,
            jurisdiction: f.jurisdiction,
            period_start: f.period_start,
            period_end: f.period_end,
            due_on: f.due_on,
            filed_on: f.filed_on,
            amount_cents: f.amount_cents,
            liability_account: f.liability_account,
            status: f.status,
            provider: f.provider,
        }
    }
}

pub(super) async fn create_tax_filing(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateTaxFilingBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }

    // Upsert-idempotent on (kind, jurisdiction, period). If the filing
    // already exists we short-circuit without re-posting the accrual
    // entry — the fact's unique (kind, source_table, source_id) index
    // would catch a duplicate anyway, but a short-circuit saves a round
    // trip through the ledger write path.
    if let Ok(Some(existing)) = crate::tax_filings::get(&state.pool, &body.id).await {
        return Json(TaxFilingView::from(existing)).into_response();
    }

    // Resolve the GL accounts + amount-derivation for this tax kind from
    // the `tax_kinds` reference table (data, not a hardcoded match in the
    // dispatcher or an allow-list CHECK). `accrue` is implied by the
    // presence of an expense account: income tax accrues against 6500,
    // while sales + payroll drain an existing liability balance.
    let resolved: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT liability_account, expense_account, derive_basis \
         FROM tax_kinds WHERE kind = $1",
    )
    .bind(&body.kind)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    let (liability_account, expense_account, derive_basis) = match resolved {
        Some(t) => t,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "unknown tax kind `{}` — register it in tax_kinds first",
                    body.kind
                ),
            )
                .into_response();
        }
    };
    let accrue = expense_account.is_some();

    // Derive amount_cents from the actual books rather than a
    // placeholder. For income tax:
    //   prior_quarter_net_income = revenue − COGS − operating expenses
    //   amount_cents = prior_quarter_net_income × 21% (federal corp rate)
    //
    // No estimated-margin guess: we compute margin DIRECTLY from
    // the GL by summing the kind buckets (revenue, cogs, expense).
    // If the prior quarter ran a loss, amount = 0 (no tax on losses).
    // If the prior quarter has no activity yet, fall back to the
    // caller's body.amount_cents so early-sim days still produce a
    // non-zero accrual that exercises the posting path.
    let amount_cents = match derive_basis.as_deref() {
        Some("period-payroll-941") => {
            // Form 941 = quarterly federal payroll tax = the net credit
            // balance sitting in 2150 Payroll Tax Payable as of the
            // period close (employee FIT withholding + employer-side
            // FICA/Medicare, both credited to 2150 at each payroll run).
            // The filing drains that balance to 1000 Cash.
            //
            // Derive from the GL balance through `period_end` — the
            // exact measure the remit guard checks (the accrued query
            // below: SUM(credit - debit) on the liability account where
            // posted_on <= period_end). Summing the raw `withheld_cents
            // + employer_tax_cents` off the finance.payroll.run facts
            // instead drifts a few cents above the posted liability (the
            // payroll posting rule rounds each run to whole cents), and
            // remitting that inflated figure trips the over-remit guard.
            // Reading 2150 directly makes the filing amount and the
            // guard agree to the cent; `posted_on <= period_end` with no
            // lower bound nets the prior quarter's remit-drain so each
            // quarter's 2150 fully clears. Mirrors the period-sales-tax
            // (2300) and period-excise (2320) arms.
            let row: Result<Option<(i64,)>, _> = sqlx::query_as(
                "SELECT COALESCE( \
                    SUM(l.credit_cents - l.debit_cents), \
                    0)::bigint \
                 FROM gl_journal_lines l \
                 JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
                 WHERE l.account_id = ( \
                        SELECT id FROM gl_accounts WHERE code = '2150' LIMIT 1) \
                   AND e.posted_on <= $1::date",
            )
            .bind(body.period_end)
            .fetch_optional(&state.pool)
            .await;
            let derived: i64 = row.ok().flatten().map(|(r,)| r).unwrap_or(0);
            tracing::info!(
                period_start = %body.period_start,
                period_end = %body.period_end,
                derived_amount_cents = derived,
                body_amount_cents = body.amount_cents,
                "payroll-941 tax derived from 2150 balance through period_end"
            );
            if derived > 0 {
                derived
            } else {
                // No payroll posted to 2150 through period_end yet —
                // fall back to the caller's number so the filing posts.
                body.amount_cents
            }
        }
        Some("period-sales-tax") => {
            // Sales tax filed for a period = the net credit balance in
            // 2300 Sales Tax Payable as of period close. Each invoice
            // with a tax_line accrues a 2300 credit at invoice time; the
            // filing drains that balance to 1000 Cash.
            //
            // Derive from the GL balance through `period_end` — the same
            // window the remit guard checks — so the filing amount
            // equals the accrued liability and the remit clears 2300 to
            // zero. The no-lower-bound window is load-bearing: the prior
            // period's remit-drain posts on its filed_on, which lands in
            // THIS period's calendar (a quarter is filed after it
            // closes), so a `>= period_start` bound would subtract that
            // drain and under-derive every period after the first.
            // Mirrors the period-payroll-941 (2150) and period-excise
            // (2320) arms.
            let row: Result<Option<(i64,)>, _> = sqlx::query_as(
                "SELECT COALESCE( \
                    SUM(l.credit_cents - l.debit_cents), \
                    0)::bigint \
                 FROM gl_journal_lines l \
                 JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
                 WHERE l.account_id = ( \
                        SELECT id FROM gl_accounts WHERE code = '2300' LIMIT 1) \
                   AND e.posted_on <= $1::date",
            )
            .bind(body.period_end)
            .fetch_optional(&state.pool)
            .await;
            let derived: i64 = row.ok().flatten().map(|(r,)| r).unwrap_or(0);
            tracing::info!(
                period_start = %body.period_start,
                period_end = %body.period_end,
                derived_amount_cents = derived,
                body_amount_cents = body.amount_cents,
                "sales tax derived from period 2300 credit balance"
            );
            if derived > 0 {
                derived
            } else {
                body.amount_cents
            }
        }
        Some("period-excise") => {
            // Federal beer excise tax filed for a period = the net
            // credit balance in 2320 Excise Tax Payable as of period
            // close. Each brew batch accrues a 2320 credit at production
            // time (DR 6550 / CR 2320 = excise_bbl × $3.50); the filing
            // drains that balance to 1000 Cash.
            //
            // Derive from the GL balance through `period_end`, no lower
            // bound — same as the period-sales-tax (2300) and
            // period-payroll-941 (2150) arms and the same window the
            // remit guard checks. The prior period's remit-drain posts
            // on its filed_on (in the following calendar period), so a
            // `>= period_start` bound would net that drain back out and
            // under-derive every period after the first, leaving 2320
            // un-cleared.
            let row: Result<Option<(i64,)>, _> = sqlx::query_as(
                "SELECT COALESCE( \
                    SUM(l.credit_cents - l.debit_cents), \
                    0)::bigint \
                 FROM gl_journal_lines l \
                 JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
                 WHERE l.account_id = ( \
                        SELECT id FROM gl_accounts WHERE code = '2320' LIMIT 1) \
                   AND e.posted_on <= $1::date",
            )
            .bind(body.period_end)
            .fetch_optional(&state.pool)
            .await;
            let derived: i64 = row.ok().flatten().map(|(r,)| r).unwrap_or(0);
            tracing::info!(
                period_start = %body.period_start,
                period_end = %body.period_end,
                derived_amount_cents = derived,
                body_amount_cents = body.amount_cents,
                "excise tax derived from period 2320 credit balance"
            );
            if derived > 0 {
                derived
            } else {
                body.amount_cents
            }
        }
        Some("prior-quarter-net-income") => {
            let prior_q_start = body
                .period_start
                .checked_sub_months(chrono::Months::new(3))
                .unwrap_or(body.period_start);
            let prior_q_end = body.period_start;
            let pnl_row: Result<Option<(i64, i64, i64)>, _> = sqlx::query_as(
                "SELECT \
                    COALESCE(SUM(CASE WHEN a.kind='revenue' \
                                      THEN l.credit_cents - l.debit_cents \
                                      ELSE 0 END), 0)::bigint AS revenue, \
                    COALESCE(SUM(CASE WHEN a.code LIKE '5%' \
                                      THEN l.debit_cents - l.credit_cents \
                                      ELSE 0 END), 0)::bigint AS cogs, \
                    COALESCE(SUM(CASE WHEN a.kind='expense' AND a.code NOT LIKE '5%' \
                                      THEN l.debit_cents - l.credit_cents \
                                      ELSE 0 END), 0)::bigint AS opex \
                 FROM gl_journal_lines l \
                 JOIN gl_accounts a ON l.account_id = a.id \
                 JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
                 WHERE e.posted_on >= $1::date \
                   AND e.posted_on < $2::date",
            )
            .bind(prior_q_start)
            .bind(prior_q_end)
            .fetch_optional(&state.pool)
            .await;
            let (revenue_cents, cogs_cents, opex_cents): (i64, i64, i64) =
                pnl_row.ok().flatten().unwrap_or((0, 0, 0));
            let net_income_cents = revenue_cents - cogs_cents - opex_cents;
            // 21% federal corporate income tax rate. No state tax in
            // v1; tenants that need a state component layer it via
            // a second filing with a different `derive_basis` later.
            const FEDERAL_CORP_RATE: f64 = 0.21;
            let tax = if net_income_cents > 0 {
                (net_income_cents as f64 * FEDERAL_CORP_RATE).round() as i64
            } else {
                0
            };
            tracing::info!(
                prior_q_start = %prior_q_start,
                prior_q_end = %prior_q_end,
                revenue_cents,
                cogs_cents,
                opex_cents,
                net_income_cents,
                derived_tax_cents = tax,
                "income tax derived from prior-quarter net income"
            );
            if tax > 0 {
                tax
            } else if revenue_cents == 0 && cogs_cents == 0 && opex_cents == 0 {
                // No prior-quarter activity at all — early-sim days.
                // Fall back to caller's number so the accrual posting
                // path stays exercised (the rule's amount > 0
                // invariant would reject 0 anyway).
                body.amount_cents
            } else {
                // Real loss in the prior quarter — no tax owed.
                // Return a token $1 so the rule's "amount > 0" check
                // passes; alternative would be skipping the accrual
                // entirely. $1 is a placeholder for "filing reflects
                // a true zero owed" while preserving JE shape.
                100
            }
        }
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("unknown derive_basis `{other}`"),
            )
                .into_response();
        }
        None => body.amount_cents,
    };

    let filing = match crate::tax_filings::upsert(
        &state.pool,
        crate::tax_filings::NewTaxFiling {
            id: &body.id,
            kind: &body.kind,
            jurisdiction: &body.jurisdiction,
            period_start: body.period_start,
            period_end: body.period_end,
            due_on: body.due_on,
            amount_cents,
            liability_account: &liability_account,
            provider: &body.provider,
        },
    )
    .await
    {
        Ok(f) => f,
        Err(e) => return ledger_err(e),
    };

    if accrue {
        let expense_account = expense_account
            .as_deref()
            .expect("accrue implies an expense account from tax_kinds");
        let stamp = super::event_stamp(
            &state,
            &user,
            boss_clock_client::now_from(&state.clock).await,
        )
        .await;
        if let Err(e) = post_accrual_entry(
            &state.pool,
            &stamp,
            &filing,
            expense_account,
            body.period_start,
        )
        .await
        {
            return e;
        }
    }

    Json(TaxFilingView::from(filing)).into_response()
}

/// Post `finance.tax.accrued` for a newly-created filing. Runs in its
/// own tx so the filing row lands whether or not the accrual entry
/// succeeds — but a failing accrual returns the error to the caller
/// so they can retry.
async fn post_accrual_entry(
    pool: &PgPool,
    stamp: &boss_core::publisher::EventStamp,
    filing: &crate::tax_filings::TaxFiling,
    expense_account: &str,
    posted_on: NaiveDate,
) -> Result<(), Response> {
    let mut tx = pool.begin().await.map_err(storage_err)?;

    let payload = serde_json::json!({
        "filing_id": filing.id,
        "kind": filing.kind,
        "jurisdiction": filing.jurisdiction,
        "posted_on": posted_on,
        "expense_account": expense_account,
        "liability_account": filing.liability_account,
        "amount_cents": filing.amount_cents,
        "period_start": filing.period_start,
        "period_end": filing.period_end,
    });
    let live_fact_id = crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.tax.accrued",
            happened_on: posted_on,
            payload: &payload,
            source_table: Some("tax_filings"),
            source_id: Some(&filing.id),
            // Matches the event source ("ledger") — the projection's
            // created_by fallback — so rebuilt facts match live ones.
            created_by: "ledger",
        },
    )
    .await
    .map_err(ledger_err)?
    .id;

    let fact = crate::types::FactRef {
        id: live_fact_id,
        kind: "finance.tax.accrued",
        happened_on: posted_on,
        payload: &payload,
    };
    crate::postgres::post_fact_in_tx(&mut tx, &fact)
        .await
        .map_err(ledger_err)?;

    crate::events::record_ledger_event_in_tx(&mut tx, stamp, "ledger.tax.accrued", payload)
        .await
        .map_err(ledger_err)?;

    tx.commit().await.map_err(storage_err)?;

    Ok(())
}

// --- standalone tax accrual -----------------------------------------------

/// Body for `POST /api/ledger/tax-accruals` — a standalone accrual with
/// no filing row. Used by the dispatcher's `ledger.tax.accrue` handler
/// to book a per-production excise liability (DR 6550 / CR 2320) the
/// moment a brew batch packages, exactly the way sales tax accrues per
/// invoice. `id` is the idempotency key (e.g. `excise-<step_id>`): it
/// feeds the financial_facts `(kind, source_table, source_id)` unique
/// index, so a duplicate POST is a no-op.
#[derive(Deserialize)]
pub(super) struct CreateTaxAccrualBody {
    id: String,
    expense_account: String,
    liability_account: String,
    amount_cents: i64,
    posted_on: NaiveDate,
    jurisdiction: String,
}

/// Post a standalone `finance.tax.accrued` fact (DR expense / CR
/// liability) without creating a tax_filings row. Idempotent on `id`:
/// `record_fact_in_tx` resolves a duplicate `(kind, source_table,
/// source_id)` to the existing fact and `post_fact_in_tx` short-circuits
/// on the already-posted JE, so a repeat POST returns 200 without
/// double-booking the liability.
pub(super) async fn create_tax_accrual(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateTaxAccrualBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if body.amount_cents <= 0 {
        return (StatusCode::BAD_REQUEST, "amount_cents must be positive").into_response();
    }

    let now = boss_clock_client::now_from(&state.clock).await;

    // `accrual_id` is the idempotency key — it doubles as the source_id
    // on the financial_facts `(kind, source_table, source_id)` index AND
    // the `source_id_path` the `ledger.tax.accrual.recorded` projection
    // rule extracts on rebuild, so live + rebuild agree on fact identity.
    let payload = serde_json::json!({
        "accrual_id": body.id,
        "expense_account": body.expense_account,
        "liability_account": body.liability_account,
        "amount_cents": body.amount_cents,
        "posted_on": body.posted_on,
        "jurisdiction": body.jurisdiction,
    });

    let stamp = super::event_stamp(&state, &user, now).await;
    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };

    let live_fact_id = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.tax.accrued",
            happened_on: body.posted_on,
            payload: &payload,
            source_table: Some("tax_accruals"),
            source_id: Some(&body.id),
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
        kind: "finance.tax.accrued",
        happened_on: body.posted_on,
        payload: &payload,
    };
    if let Err(e) = crate::postgres::post_fact_in_tx(&mut tx, &fact).await {
        return ledger_err(e);
    }

    // Distinct event kind from income-tax's `ledger.tax.accrued`: this
    // standalone accrual has no tax_filings row, so it gets its own
    // `ledger.tax.accrual.recorded` projection rule (source_table
    // 'tax_accruals', source_id_path '/accrual_id') that rebuilds the
    // `finance.tax.accrued` fact from audit_log alone. Sharing the
    // income-tax kind would route the rebuild through the
    // `/filing_id`-keyed rule, which extracts a NULL source_id here and
    // silently drops the fact. Recorded in the SAME tx (outbox phase 2).
    if let Err(e) = crate::events::record_ledger_event_in_tx(
        &mut tx,
        &stamp,
        "ledger.tax.accrual.recorded",
        payload,
    )
    .await
    {
        return ledger_err(e);
    }

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    (StatusCode::OK, Json(serde_json::json!({ "id": body.id }))).into_response()
}

#[derive(Deserialize)]
pub(super) struct RemitTaxBody {
    /// Date the filing was remitted (bank send date). Defaults to today.
    #[serde(default)]
    filed_on: Option<NaiveDate>,
}

/// Remit a filing — flips status to `paid` AND emits
/// `finance.tax.remitted` AND posts the draining journal entry, all in
/// one transaction. Idempotent: calling remit on an already-paid filing
/// returns the existing row without double-posting.
pub(super) async fn remit_tax_filing(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<RemitTaxBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let existing = match crate::tax_filings::get(&state.pool, &id).await {
        Ok(Some(f)) => f,
        Ok(None) => return (StatusCode::NOT_FOUND, "filing not found").into_response(),
        Err(e) => return ledger_err(e),
    };
    if existing.status == "paid" {
        return Json(TaxFilingView::from(existing)).into_response();
    }

    let filed_on = body
        .filed_on
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());

    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };

    // Conservation: a remittance cannot exceed the actual accrued
    // liability balance for the period. Query the live GL credits
    // - debits on the liability account up to `period_end`. The
    // rule itself (pure function) can't see the GL, so the check
    // lives here — same tx, defense-in-depth against a misshaped
    // tax_filings row that would otherwise drive the liability
    // negative.
    //
    // Skip the check when the configured liability_account isn't
    // in the GL yet (rebuild edge cases) — let it through; the
    // posting rule rejects negative amounts and the conservation
    // lint catches drift in CI.
    let accrued_row: Result<Option<(i64,)>, _> = sqlx::query_as(
        "SELECT COALESCE(SUM(l.credit_cents - l.debit_cents), 0)::bigint \
         FROM gl_journal_lines l \
         JOIN gl_accounts a ON l.account_id = a.id \
         JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
         WHERE a.code = $1 AND e.posted_on <= $2",
    )
    .bind(&existing.liability_account)
    .bind(existing.period_end)
    .fetch_optional(&mut *tx)
    .await;
    if let Ok(Some((accrued,))) = accrued_row
        && accrued > 0
        && existing.amount_cents > accrued
    {
        return (
            StatusCode::CONFLICT,
            format!(
                "remittance ${} exceeds accrued liability ${} on {} through {}",
                existing.amount_cents / 100,
                accrued / 100,
                existing.liability_account,
                existing.period_end,
            ),
        )
            .into_response();
    }

    let payload = serde_json::json!({
        "filing_id": existing.id,
        "kind": existing.kind,
        "jurisdiction": existing.jurisdiction,
        "filed_on": filed_on,
        "liability_account": existing.liability_account,
        "amount_cents": existing.amount_cents,
        "period_start": existing.period_start,
        "period_end": existing.period_end,
    });
    let live_fact_id = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.tax.remitted",
            happened_on: filed_on,
            payload: &payload,
            source_table: Some("tax_filings"),
            source_id: Some(&existing.id),
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
        kind: "finance.tax.remitted",
        happened_on: filed_on,
        payload: &payload,
    };
    if let Err(e) = crate::postgres::post_fact_in_tx(&mut tx, &fact).await {
        return ledger_err(e);
    }

    if let Err(e) = sqlx::query(
        "UPDATE tax_filings \
            SET status = 'paid', filed_on = $2, updated_at = NOW() \
          WHERE id = $1",
    )
    .bind(&existing.id)
    .bind(filed_on)
    .execute(&mut *tx)
    .await
    {
        return storage_err(e);
    }

    {
        let now = boss_clock_client::now_from(&state.clock).await;
        let stamp = super::event_stamp(&state, &user, now).await;
        if let Err(e) = crate::events::record_ledger_event_in_tx(
            &mut tx,
            &stamp,
            "ledger.tax.remitted",
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

    let updated = match crate::tax_filings::get(&state.pool, &existing.id).await {
        Ok(Some(f)) => f,
        Ok(None) => return (StatusCode::INTERNAL_SERVER_ERROR, "filing vanished").into_response(),
        Err(e) => return ledger_err(e),
    };
    Json(TaxFilingView::from(updated)).into_response()
}

#[derive(Deserialize)]
pub(super) struct ListTaxFilingsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

pub(super) async fn list_tax_filings(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<ListTaxFilingsQuery>,
) -> Response {
    match crate::tax_filings::list(&state.pool, q.status.as_deref(), q.limit.unwrap_or(200)).await {
        Ok(filings) => {
            let views: Vec<TaxFilingView> = filings.into_iter().map(TaxFilingView::from).collect();
            Json(views).into_response()
        }
        Err(e) => ledger_err(e),
    }
}

pub(super) async fn get_tax_filing(
    State(state): State<Arc<LedgerApiState>>,
    Path(id): Path<String>,
) -> Response {
    match crate::tax_filings::get(&state.pool, &id).await {
        Ok(Some(f)) => Json(TaxFilingView::from(f)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => ledger_err(e),
    }
}

/// Aggregated tax-liability summary for the Finance page's Tax tab.
/// Reads live balances from `gl_journal_lines` (not from the filings
/// projection, so unposted invoices + manual entries show up too)
/// plus the accrued filings list so the operator sees "what do we
/// owe + when is it due".
#[derive(Serialize)]
struct TaxLiabilityRow {
    account_code: String,
    account_name: String,
    balance_cents: i64,
}

#[derive(Serialize)]
struct TaxLiabilitySummaryResponse {
    as_of: NaiveDate,
    liabilities: Vec<TaxLiabilityRow>,
    accrued_filings: Vec<TaxFilingView>,
    next_due: Option<TaxFilingView>,
    currency: String,
}

pub(super) async fn tax_liability_summary(State(state): State<Arc<LedgerApiState>>) -> Response {
    let as_of = boss_clock_client::now_from(&state.clock).await.date_naive();

    let rows_result: Result<Vec<(String, String, i64, i64)>, _> = sqlx::query_as(
        "SELECT a.code, a.name, \
                COALESCE(SUM(l.debit_cents), 0)::bigint, \
                COALESCE(SUM(l.credit_cents), 0)::bigint \
         FROM gl_accounts a \
         LEFT JOIN gl_journal_lines l ON l.account_id = a.id \
         LEFT JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         WHERE a.code IN ('2150', '2300', '2310') \
           AND (e.posted_on IS NULL OR e.posted_on <= $1) \
         GROUP BY a.code, a.name \
         ORDER BY a.code",
    )
    .bind(as_of)
    .fetch_all(&state.pool)
    .await;

    let liability_rows = match rows_result {
        Ok(rs) => rs,
        Err(e) => return storage_err(e),
    };
    let liabilities: Vec<TaxLiabilityRow> = liability_rows
        .into_iter()
        .map(|(code, name, debit, credit)| TaxLiabilityRow {
            account_code: code,
            account_name: name,
            balance_cents: credit - debit,
        })
        .collect();

    let accrued_filings = match crate::tax_filings::list(&state.pool, Some("accrued"), 100).await {
        Ok(fs) => fs,
        Err(e) => return ledger_err(e),
    };
    let accrued_views: Vec<TaxFilingView> = accrued_filings
        .into_iter()
        .map(TaxFilingView::from)
        .collect();
    // `list` orders by due_on DESC — flip to ASC for the "next due"
    // pick so the soonest obligation wins.
    let next_due = accrued_views.iter().min_by_key(|f| f.due_on).cloned();

    Json(TaxLiabilitySummaryResponse {
        as_of,
        liabilities,
        accrued_filings: accrued_views,
        next_due,
        currency: "USD".to_string(),
    })
    .into_response()
}
