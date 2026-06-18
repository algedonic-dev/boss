use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::CurrentUser;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;

// --- periods --------------------------------------------------------------

pub(super) async fn list_periods_handler(State(state): State<Arc<LedgerApiState>>) -> Response {
    match crate::periods::list_periods(&state.pool).await {
        Ok(periods) => Json(periods).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct LockBody {
    #[serde(default)]
    locked_by: Option<String>,
}

pub(super) async fn lock_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    body: Option<Json<LockBody>>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let locked_by = body
        .and_then(|b| b.0.locked_by)
        .unwrap_or_else(|| "ledger".to_string());
    match crate::periods::lock_period(&state.pool, id, &locked_by).await {
        Ok(checksum) => {
            // Operator-audit-trail emit. gl_periods is system-of-
            // record (not derived from audit_log) so this event
            // doesn't drive a rebuild — its purpose is the
            // who-locked-what trail for auditors. Emitted
            // post-success so a failed lock doesn't pollute the
            // log.
            let now = boss_clock_client::now_from(&state.clock).await;
            crate::events::emit_after_commit(
                &state.publisher,
                "ledger.period.locked",
                serde_json::json!({
                    "period_id": id,
                    "locked_by": locked_by,
                    "actor_id": user.id,
                    "checksum": checksum,
                    "locked_at": now,
                }),
                now,
            )
            .await;
            Json(serde_json::json!({"status": "locked", "checksum": checksum})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn unlock_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    match crate::periods::unlock_period(&state.pool, id).await {
        Ok(()) => {
            // Unlock is the destructive admin path — auditor
            // visibility matters most here. Same rationale as
            // lock_handler above.
            let now = boss_clock_client::now_from(&state.clock).await;
            crate::events::emit_after_commit(
                &state.publisher,
                "ledger.period.unlocked",
                serde_json::json!({
                    "period_id": id,
                    "actor_id": user.id,
                    "unlocked_at": now,
                }),
                now,
            )
            .await;
            Json(serde_json::json!({"status": "open"})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// --- yearly period creation -----------------------------------------------

/// Body for `POST /api/ledger/periods`. Today only yearly periods
/// get created via this endpoint — monthly periods are auto-created
/// on first posting. The API takes a `year` convenience field
/// rather than raw start/end dates so a caller can't land Q2-2026
/// + Q3-2027 into the same "period."
#[derive(Deserialize)]
pub(super) struct CreatePeriodBody {
    /// Calendar year the fiscal year ends on. FY 2026 → Jan 1 2026 to
    /// Dec 31 2026 inclusive. Mid-year fiscal years aren't modeled
    /// yet; when they land the body grows a `starts_on` override.
    year: i32,
}

pub(super) async fn create_period_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreatePeriodBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let starts_on = match NaiveDate::from_ymd_opt(body.year, 1, 1) {
        Some(d) => d,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid year {}", body.year),
            )
                .into_response();
        }
    };
    let ends_on = match NaiveDate::from_ymd_opt(body.year, 12, 31) {
        Some(d) => d,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid year {}", body.year),
            )
                .into_response();
        }
    };

    let id = Uuid::new_v4();
    let result = sqlx::query(
        "INSERT INTO gl_periods (id, kind, starts_on, ends_on, status) \
         VALUES ($1, 'year', $2, $3, 'open') \
         ON CONFLICT (kind, starts_on) DO NOTHING",
    )
    .bind(id)
    .bind(starts_on)
    .bind(ends_on)
    .execute(&state.pool)
    .await;
    if let Err(e) = result {
        return storage_err(e);
    }

    // Always read back so an idempotent repeat returns the existing id.
    let row: Result<(Uuid,), _> =
        sqlx::query_as("SELECT id FROM gl_periods WHERE kind = 'year' AND starts_on = $1")
            .bind(starts_on)
            .fetch_one(&state.pool)
            .await;
    match row {
        Ok((existing_id,)) => {
            // Operator-audit-trail emit. The period row is system
            // of record; this event is for the audit trail only,
            // not for rebuilders. Idempotent re-creates emit too
            // (cheap; auditors see the second attempt for free).
            let now = boss_clock_client::now_from(&state.clock).await;
            crate::events::emit_after_commit(
                &state.publisher,
                "ledger.period.created",
                serde_json::json!({
                    "period_id": existing_id,
                    "kind": "year",
                    "starts_on": starts_on,
                    "ends_on": ends_on,
                    "actor_id": user.id,
                    "created_at": now,
                }),
                now,
            )
            .await;
            Json(serde_json::json!({
                "id":        existing_id,
                "kind":      "year",
                "starts_on": starts_on,
                "ends_on":   ends_on,
                "status":    "open",
            }))
            .into_response()
        }
        Err(e) => storage_err(e),
    }
}

// --- yearly period close --------------------------------------------------

/// `POST /api/ledger/periods/{id}/close` — the year-end close.
///
/// Computes per-account revenue + expense balances as of the period's
/// `ends_on`, emits a `finance.period.closed` fact with those
/// balances, posts the resulting closing journal entry into the
/// yearly period (bypassing the monthly auto-assignment so the
/// closing entries don't conflate with December's real activity),
/// then locks the yearly period with a checksum. Idempotent — a
/// second close on an already-locked yearly period returns the
/// existing checksum without re-posting.
#[derive(Deserialize)]
pub(super) struct CloseBody {
    #[serde(default)]
    closed_by: Option<String>,
    /// Retained-earnings account the net income rolls into. Defaults
    /// to `3000` (matches the starter chart). Override if an
    /// operator renamed/repointed the account.
    #[serde(default)]
    retained_earnings_account: Option<String>,
}

#[derive(Serialize)]
struct ClosePeriodResponse {
    status: &'static str,
    checksum: String,
    /// Uuid of the `financial_facts` row the close wrote. Auditors
    /// follow this to the journal entry via the usual
    /// `/api/ledger/entries?fact_id=...` lookup.
    fact_id: Uuid,
    revenue_closed_cents: i64,
    expense_closed_cents: i64,
    net_income_cents: i64,
}

pub(super) async fn close_period_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    body: Option<Json<CloseBody>>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let body = body.map(|b| b.0).unwrap_or(CloseBody {
        closed_by: None,
        retained_earnings_account: None,
    });
    let closed_by = body.closed_by.unwrap_or_else(|| "ledger".to_string());
    let retained_earnings = body
        .retained_earnings_account
        .unwrap_or_else(|| "3000".to_string());

    // Load the period.
    let period: Result<Option<(String, NaiveDate, NaiveDate, String)>, _> =
        sqlx::query_as("SELECT kind, starts_on, ends_on, status FROM gl_periods WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await;
    let (kind, starts_on, ends_on, status) = match period {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, "period not found").into_response(),
        Err(e) => return storage_err(e),
    };

    if kind != "year" {
        return (
            StatusCode::BAD_REQUEST,
            "close only applies to yearly periods — monthly periods are closed via /lock",
        )
            .into_response();
    }

    // Idempotent: re-closing a locked year returns the existing checksum.
    if status == "locked" {
        let existing: Result<Option<(Option<String>,)>, _> =
            sqlx::query_as("SELECT locked_checksum FROM gl_periods WHERE id = $1")
                .bind(id)
                .fetch_optional(&state.pool)
                .await;
        match existing {
            Ok(Some((Some(cs),))) => {
                return Json(serde_json::json!({
                    "status": "locked",
                    "checksum": cs,
                    "note": "already closed",
                }))
                .into_response();
            }
            Ok(_) => {
                return (StatusCode::CONFLICT, "period is locked but has no checksum")
                    .into_response();
            }
            Err(e) => return storage_err(e),
        }
    }

    // Compute per-account revenue + expense balances within the FY,
    // filtered on entries whose posted_on falls in the range.
    type BalRow = (String, String, i64);
    let revenue_rows: Result<Vec<BalRow>, _> = sqlx::query_as(
        "SELECT a.code, a.kind, \
                COALESCE(SUM(l.credit_cents - l.debit_cents), 0)::bigint AS balance \
         FROM gl_accounts a \
         LEFT JOIN gl_journal_lines l ON l.account_id = a.id \
         LEFT JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         WHERE a.kind = 'revenue' \
           AND e.posted_on BETWEEN $1 AND $2 \
         GROUP BY a.code, a.kind \
         HAVING COALESCE(SUM(l.credit_cents - l.debit_cents), 0) != 0 \
         ORDER BY a.code",
    )
    .bind(starts_on)
    .bind(ends_on)
    .fetch_all(&state.pool)
    .await;
    let expense_rows: Result<Vec<BalRow>, _> = sqlx::query_as(
        "SELECT a.code, a.kind, \
                COALESCE(SUM(l.debit_cents - l.credit_cents), 0)::bigint AS balance \
         FROM gl_accounts a \
         LEFT JOIN gl_journal_lines l ON l.account_id = a.id \
         LEFT JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         WHERE a.kind = 'expense' \
           AND e.posted_on BETWEEN $1 AND $2 \
         GROUP BY a.code, a.kind \
         HAVING COALESCE(SUM(l.debit_cents - l.credit_cents), 0) != 0 \
         ORDER BY a.code",
    )
    .bind(starts_on)
    .bind(ends_on)
    .fetch_all(&state.pool)
    .await;

    let revenue_rows = match revenue_rows {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };
    let expense_rows = match expense_rows {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };

    if revenue_rows.is_empty() && expense_rows.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "no revenue or expense activity in this period — nothing to close",
        )
            .into_response();
    }

    let revenue_total: i64 = revenue_rows.iter().map(|r| r.2).sum();
    let expense_total: i64 = expense_rows.iter().map(|r| r.2).sum();
    let net_income = revenue_total - expense_total;

    let revenue_lines: Vec<serde_json::Value> = revenue_rows
        .iter()
        .map(|(code, _, bal)| serde_json::json!({ "account_code": code, "balance_cents": bal }))
        .collect();
    let expense_lines: Vec<serde_json::Value> = expense_rows
        .iter()
        .map(|(code, _, bal)| serde_json::json!({ "account_code": code, "balance_cents": bal }))
        .collect();

    let fact_id = Uuid::new_v4();
    let payload = serde_json::json!({
        "period_id": id,
        "period_end": ends_on,
        "retained_earnings_account": retained_earnings,
        "revenue_lines": revenue_lines,
        "expense_lines": expense_lines,
    });

    // Transactional section: insert fact + post entry into the yearly
    // period (not the monthly), then lock the yearly period.
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return storage_err(e),
    };

    let period_source_id = id.to_string();
    let fact_id = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            fact_id,
            kind: "finance.period.closed",
            happened_on: ends_on,
            payload: &payload,
            source_table: Some("gl_periods"),
            source_id: Some(&period_source_id),
            created_by: "ledger",
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => return ledger_err(e),
    };

    // Post into the yearly period explicitly. We can't use
    // post_fact_in_tx here because that auto-assigns to the monthly
    // period containing posted_on (Dec 31's month) — wrong for a
    // year-end close. Reach into the crate internals directly.
    let fact_ref = crate::FactRef {
        id: fact_id,
        kind: "finance.period.closed",
        happened_on: ends_on,
        payload: &payload,
    };
    let draft = match crate::rules::evaluate(&crate::BossRuleSet, &fact_ref) {
        Ok(d) => d,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if let Err(e) = crate::postgres::insert_closing_entry(&mut tx, fact_id, id, &draft).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Lock the yearly period. We inline the same checksum-compute
    // logic as `periods::lock_period` since that fn needs the pool
    // (not a tx) today, and closing must be atomic with the lock.
    let active_version_id: Result<(Uuid,), _> =
        sqlx::query_as("SELECT id FROM gl_rule_versions WHERE is_active = true")
            .fetch_one(&mut *tx)
            .await;
    let (active_version_id,) = match active_version_id {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };
    let checksum = match crate::periods::compute_period_checksum(&mut tx, id).await {
        Ok(cs) => cs,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let lock_result = sqlx::query(
        "UPDATE gl_periods SET \
            status = 'locked', \
            locked_at = NOW(), \
            locked_by = $2, \
            locked_rule_version_id = $3, \
            locked_checksum = $4 \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&closed_by)
    .bind(active_version_id)
    .bind(&checksum)
    .execute(&mut *tx)
    .await;
    if let Err(e) = lock_result {
        return storage_err(e);
    }

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    crate::events::emit_after_commit(
        &state.publisher,
        "ledger.period.closed",
        payload.clone(),
        boss_clock_client::now_from(&state.clock).await,
    )
    .await;

    Json(ClosePeriodResponse {
        status: "locked",
        checksum,
        fact_id,
        revenue_closed_cents: revenue_total,
        expense_closed_cents: expense_total,
        net_income_cents: net_income,
    })
    .into_response()
}
