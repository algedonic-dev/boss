use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::{CurrentUser, Pair, controls};
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
    /// Read only to refuse a claim: the locker is the signed caller
    /// (`super::author`, backlog 975c228f — the finance page sent the
    /// literal `operator`, which the Periods table showed as "Locked by").
    #[serde(default)]
    locked_by: Option<String>,
}

/// Closing a month or reopening one, asked of policy (backlog 25a4f7f9,
/// 2026-09-28): `action` on `ledger-period` through the registry-write
/// ladder ([`boss_policy_client::writes::require_registry_write`]) — no
/// caller 401, a deny or a grant narrower than `all` 403 (a period
/// belongs to no person and no department), a policy service that
/// cannot answer 503. Until then these two doors checked
/// `reject_if_auditor` alone, so the router-wide `ledger` READ grant was
/// all a caller needed to freeze a month's checksum or reopen it. The
/// actor the ladder answers is the one `super::author` already records,
/// since a signed caller is required first.
async fn authorize_period_write(
    state: &LedgerApiState,
    user: &boss_policy_client::User,
    control: Pair,
) -> Result<(), Response> {
    boss_policy_client::writes::require_registry_write(state.policy.as_ref(), user, control)
        .await
        .map(|_| ())
}

/// Lock is Close on `ledger-period` ([`authorize_period_write`]).
pub(super) async fn lock_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    body: Option<Json<LockBody>>,
) -> Response {
    if let Err(refused) = authorize_period_write(&state, &user, controls::CLOSE_LEDGER_PERIOD).await
    {
        return refused;
    }
    let said = body.and_then(|b| b.0.locked_by);
    let locked_by = match super::author::author(&user, "locked_by", said.as_deref()) {
        Ok(a) => a,
        Err(refused) => return refused.into_response(),
    };
    // Outbox phase 2: the who-locked-what audit event records inside
    // lock_period's own transaction.
    let stamp = super::event_stamp(&state, &user).await;
    match crate::periods::lock_period(&state.pool, id, &locked_by, &stamp, &user.id).await {
        Ok(checksum) => {
            Json(serde_json::json!({"status": "locked", "checksum": checksum})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Reopen is Update on `ledger-period` ([`authorize_period_write`]): a
/// separate grant from closing, because reopening a month is what lets
/// its history be rewritten.
pub(super) async fn unlock_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Response {
    if let Err(refused) =
        authorize_period_write(&state, &user, controls::UPDATE_LEDGER_PERIOD).await
    {
        return refused;
    }
    let stamp = super::event_stamp(&state, &user).await;
    match crate::periods::unlock_period(&state.pool, id, &stamp, &user.id).await {
        Ok(()) => Json(serde_json::json!({"status": "open"})).into_response(),
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

/// The caller of `POST /api/ledger/periods` — creating a fiscal year —
/// admitted by Create on `ledger-period` ([`authorize_period_write`]),
/// the resource locking, reopening and closing a period ask, so one
/// grant covers a year's whole life (backlog 34f0a954). It asked
/// `reject_if_auditor` alone until then, so a `ledger` reader could open
/// a year. An extractor, so the ask runs before the body is read.
pub(super) struct PeriodCreate(boss_policy_client::User);

impl axum::extract::FromRequestParts<Arc<LedgerApiState>> for PeriodCreate {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<LedgerApiState>,
    ) -> Result<Self, Response> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        authorize_period_write(state, &user, controls::CREATE_LEDGER_PERIOD).await?;
        Ok(Self(user))
    }
}

pub(super) async fn create_period_handler(
    State(state): State<Arc<LedgerApiState>>,
    PeriodCreate(user): PeriodCreate,
    Json(body): Json<CreatePeriodBody>,
) -> Response {
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
    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = super::event_stamp(&state, &user).await;
    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };
    let result = sqlx::query(
        "INSERT INTO gl_periods (id, kind, starts_on, ends_on, status) \
         VALUES ($1, 'year', $2, $3, 'open') \
         ON CONFLICT (kind, starts_on) DO NOTHING",
    )
    .bind(id)
    .bind(starts_on)
    .bind(ends_on)
    .execute(&mut *tx)
    .await;
    if let Err(e) = result {
        return storage_err(e);
    }

    // Always read back so an idempotent repeat returns the existing id.
    let row: Result<(Uuid,), _> =
        sqlx::query_as("SELECT id FROM gl_periods WHERE kind = 'year' AND starts_on = $1")
            .bind(starts_on)
            .fetch_one(&mut *tx)
            .await;
    match row {
        Ok((existing_id,)) => {
            // Operator-audit-trail event, recorded in the SAME tx as
            // the row (outbox phase 2). The period row is system of
            // record; this event is for the audit trail only, not
            // for rebuilders. Idempotent re-creates record too
            // (cheap; auditors see the second attempt for free).
            if let Err(e) = crate::events::record_ledger_event_in_tx(
                &mut tx,
                &stamp,
                "ledger.period.created",
                serde_json::json!({
                    "period_id": existing_id,
                    "kind": "year",
                    "starts_on": starts_on,
                    "ends_on": ends_on,
                    "actor_id": user.id,
                    "created_at": now,
                }),
            )
            .await
            {
                return ledger_err(e);
            }
            if let Err(e) = tx.commit().await {
                return storage_err(e);
            }
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
/// `ends_on`, plus the residual WIP balance (`wip_account`, default
/// `1310`) that writes off to retained earnings so WIP starts the new
/// year at zero. Emits a `finance.period.closed` fact with those
/// balances, posts the resulting closing journal entry into the
/// yearly period (bypassing the monthly auto-assignment so the
/// closing entries don't conflate with December's real activity),
/// then locks the yearly period with a checksum. Idempotent — a
/// second close on an already-locked yearly period returns the
/// existing checksum without re-posting.
#[derive(Deserialize)]
pub(super) struct CloseBody {
    /// Read only to refuse a claim: the closer, written to the year's
    /// `locked_by`, is the signed caller (backlog 975c228f).
    #[serde(default)]
    closed_by: Option<String>,
    /// Retained-earnings account the net income rolls into. Defaults
    /// to `3000` (matches the starter chart). Override if an
    /// operator renamed/repointed the account.
    #[serde(default)]
    retained_earnings_account: Option<String>,
    /// WIP account whose residual balance the close writes off to
    /// retained earnings (so WIP starts the new year at zero without
    /// re-inflating the drained COGS account). Defaults to `1310`
    /// (starter chart). Same override contract as
    /// `retained_earnings_account`.
    #[serde(default)]
    wip_account: Option<String>,
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
    /// Residual WIP balance the close wrote off to retained
    /// earnings (0 = WIP was already clean).
    wip_variance_cents: i64,
}

/// The year-end close locks a year, so it asks what locking a month asks:
/// Close on `ledger-period` ([`authorize_period_write`]). It checked
/// `reject_if_auditor` alone until the review of car 25a4f7f9, which
/// left any ledger reader able to post a year's closing entries and
/// lock it while locking one month needed the grant.
pub(super) async fn close_period_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    body: Option<Json<CloseBody>>,
) -> Response {
    if let Err(refused) = authorize_period_write(&state, &user, controls::CLOSE_LEDGER_PERIOD).await
    {
        return refused;
    }
    let body = body.map(|b| b.0).unwrap_or(CloseBody {
        closed_by: None,
        retained_earnings_account: None,
        wip_account: None,
    });
    let closed_by = match super::author::author(&user, "closed_by", body.closed_by.as_deref()) {
        Ok(a) => a,
        Err(refused) => return refused.into_response(),
    };
    let retained_earnings = body
        .retained_earnings_account
        .unwrap_or_else(|| "3000".to_string());
    let wip_account = body.wip_account.unwrap_or_else(|| "1310".to_string());

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

    // Residual WIP balance as of the close. Cumulative through
    // ends_on — an asset account's balance, not a period flow like
    // the revenue/expense windows above (those zero at every close;
    // WIP carries whatever prior closes + the year's activity left
    // on it). A tenant whose chart has no such account sums zero
    // rows → 0 → the close-out is a no-op.
    let wip_variance: Result<(i64,), _> = sqlx::query_as(
        "SELECT COALESCE(SUM(l.debit_cents - l.credit_cents), 0)::bigint \
         FROM gl_accounts a \
         JOIN gl_journal_lines l ON l.account_id = a.id \
         JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         WHERE a.code = $1 \
           AND e.posted_on <= $2",
    )
    .bind(&wip_account)
    .bind(ends_on)
    .fetch_one(&state.pool)
    .await;
    let wip_variance = match wip_variance {
        Ok((v,)) => v,
        Err(e) => return storage_err(e),
    };

    let revenue_lines: Vec<serde_json::Value> = revenue_rows
        .iter()
        .map(|(code, _, bal)| serde_json::json!({ "account_code": code, "balance_cents": bal }))
        .collect();
    let expense_lines: Vec<serde_json::Value> = expense_rows
        .iter()
        .map(|(code, _, bal)| serde_json::json!({ "account_code": code, "balance_cents": bal }))
        .collect();

    // wip fields are stamped even at zero variance (the rule no-ops
    // on 0): the fact then records that the close *checked* WIP, not
    // that the field didn't exist yet.
    let payload = serde_json::json!({
        "period_id": id,
        "period_end": ends_on,
        "retained_earnings_account": retained_earnings,
        "revenue_lines": revenue_lines,
        "expense_lines": expense_lines,
        "wip_variance_cents": wip_variance,
        "wip_account": wip_account,
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
        Ok(rec) => rec.id,
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
    let draft = match crate::rules::evaluate(&crate::BossRuleSet::default(), &fact_ref) {
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

    // Outbox phase 2: the close event commits with the closing
    // entry + the lock, atomically.
    {
        let stamp = super::event_stamp(&state, &user).await;
        if let Err(e) = crate::events::record_ledger_event_in_tx(
            &mut tx,
            &stamp,
            "ledger.period.closed",
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

    Json(ClosePeriodResponse {
        status: "locked",
        checksum,
        fact_id,
        revenue_closed_cents: revenue_total,
        expense_closed_cents: expense_total,
        net_income_cents: net_income,
        wip_variance_cents: wip_variance,
    })
    .into_response()
}
