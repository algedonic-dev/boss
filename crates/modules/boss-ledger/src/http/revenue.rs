use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::CurrentUser;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::*;

// --- revenue schedules ----------------------------------------------------

/// Request body for `POST /api/ledger/revenue-schedules` — mirrors the
/// `revenue_schedules` row shape. Called by the contracts sim
/// generator (and any future caller that opens a ratable obligation)
/// to register the monthly-recognition schedule that drives
/// `boss-ledger-recognize`. See ASC 606 step 4.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateRevenueScheduleBody {
    id: String,
    source_kind: String,
    source_id: String,
    account_id: String,
    revenue_category: String,
    revenue_account: String,
    deferred_account: String,
    total_cents: i64,
    start_date: NaiveDate,
    end_date: NaiveDate,
    frequency: String,
    next_recognition_date: NaiveDate,
}

/// Insert a `revenue_schedules` row. Idempotent on `id` — a repeat
/// POST returns `200 OK` with the existing row rather than
/// double-creating. `status` is seeded as `'active'` and
/// `recognized_to_date_cents = 0`; the scheduler advances both.
pub(super) async fn create_revenue_schedule(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateRevenueScheduleBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    // Basic validation — the CHECKs on the table are the belt, these
    // are the suspenders. A clear 400 beats a Postgres RAISE.
    if body.total_cents < 0 {
        return (StatusCode::BAD_REQUEST, "total_cents must be non-negative").into_response();
    }
    if body.end_date < body.start_date {
        return (StatusCode::BAD_REQUEST, "end_date must be >= start_date").into_response();
    }
    if !matches!(body.frequency.as_str(), "monthly" | "quarterly") {
        return (
            StatusCode::BAD_REQUEST,
            "frequency must be 'monthly' or 'quarterly'",
        )
            .into_response();
    }

    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = super::event_stamp(&state, &user, now).await;
    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };
    let result = sqlx::query(
        "INSERT INTO revenue_schedules \
             (id, source_kind, source_id, account_id, revenue_category, \
              revenue_account, deferred_account, total_cents, start_date, \
              end_date, frequency, recognized_to_date_cents, \
              next_recognition_date, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 0, $12, 'active') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(&body.id)
    .bind(&body.source_kind)
    .bind(&body.source_id)
    .bind(&body.account_id)
    .bind(&body.revenue_category)
    .bind(&body.revenue_account)
    .bind(&body.deferred_account)
    .bind(body.total_cents)
    .bind(body.start_date)
    .bind(body.end_date)
    .bind(&body.frequency)
    .bind(body.next_recognition_date)
    .execute(&mut *tx)
    .await;

    match result {
        Ok(_) => {
            // Operator-audit-trail event, recorded in the SAME tx as
            // the row (outbox phase 2). revenue_schedules is a
            // forecast configuration (not a derived projection),
            // so this event doesn't drive a rebuilder — just the
            // who-set-up-what trail. ON CONFLICT DO NOTHING above
            // means re-records land in the log even when the row
            // already existed; that's fine for the audit-trail
            // purpose (auditors see the second attempt at no
            // additional cost).
            if let Err(e) = crate::events::record_ledger_event_in_tx(
                &mut tx,
                &stamp,
                "ledger.revenue_schedule.created",
                serde_json::json!({
                    "schedule_id": body.id,
                    "source_kind": body.source_kind,
                    "source_id": body.source_id,
                    "account_id": body.account_id,
                    "total_cents": body.total_cents,
                    "frequency": body.frequency,
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
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "id": body.id })),
            )
                .into_response()
        }
        Err(e) => storage_err(e),
    }
}

// --- deferred revenue runoff ----------------------------------------------

#[derive(Deserialize)]
pub(super) struct RunoffQuery {
    /// ISO date anchoring the horizon. Defaults to today.
    as_of: Option<NaiveDate>,
    /// Months to project forward. Defaults to 12; clamped to [1, 60] so
    /// a caller can't ask for a 600-month table that grinds the DB.
    months: Option<u32>,
}

#[derive(Serialize)]
struct RunoffMonthView {
    month: NaiveDate,
    amount_cents: i64,
}

#[derive(Serialize)]
struct DeferredRevenueRunoffResponse {
    as_of: NaiveDate,
    horizon_months: u32,
    /// Current GL balance of `2200 Deferred Revenue` as of `as_of`.
    /// Exposed so the UI can highlight drift between the ledger and
    /// the sum of active schedules — a gap means either schedules
    /// aren't registered or deferred revenue was posted outside v2.
    deferred_account_balance_cents: i64,
    /// Sum of remaining un-recognized cents across all active schedules.
    /// Ideally equal to `deferred_account_balance_cents`.
    schedules_remaining_cents: i64,
    /// Signed: positive means GL > schedules (under-scheduled).
    drift_cents: i64,
    months: Vec<RunoffMonthView>,
    beyond_horizon_cents: i64,
    currency: String,
}

pub(super) async fn deferred_revenue_runoff(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<RunoffQuery>,
) -> Response {
    let as_of = q
        .as_of
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());
    let horizon_months = q.months.unwrap_or(12).clamp(1, 60);

    // Pull every active schedule, translate to the in-memory shape
    // the pure projection expects. We don't filter to "due" because
    // the projection needs the cursor-current state of every live
    // schedule, not just the ones ready to post today.
    type SchedRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        NaiveDate,
        NaiveDate,
        String,
        i64,
        NaiveDate,
    );

    let rows: Result<Vec<SchedRow>, _> = sqlx::query_as(
        "SELECT id, source_kind, source_id, account_id, revenue_category, \
                revenue_account, deferred_account, total_cents, start_date, \
                end_date, frequency, recognized_to_date_cents, next_recognition_date \
         FROM revenue_schedules \
         WHERE status = 'active'",
    )
    .fetch_all(&state.pool)
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };

    let schedules: Vec<crate::recognize::ScheduleRow> = rows
        .into_iter()
        .filter_map(|r| {
            crate::recognize::Frequency::parse_db_str(&r.10).map(|frequency| {
                crate::recognize::ScheduleRow {
                    id: r.0,
                    source_kind: r.1,
                    source_id: r.2,
                    account_id: r.3,
                    revenue_category: r.4,
                    revenue_account: r.5,
                    deferred_account: r.6,
                    total_cents: r.7,
                    start_date: r.8,
                    end_date: r.9,
                    frequency,
                    recognized_to_date_cents: r.11,
                    next_recognition_date: r.12,
                }
            })
        })
        .collect();

    let projection = crate::recognize::project_runoff(&schedules, as_of, horizon_months);

    // Ledger balance of 2200 Deferred Revenue as of the cutoff. It's
    // a liability — credits increase, debits decrease — so
    // `credits - debits`.
    let bal_result: Result<(i64, i64), _> = sqlx::query_as(
        "SELECT COALESCE(SUM(l.debit_cents), 0)::bigint, \
                COALESCE(SUM(l.credit_cents), 0)::bigint \
         FROM gl_journal_lines l \
         JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         JOIN gl_accounts a ON a.id = l.account_id \
         WHERE a.code = '2200' AND e.posted_on <= $1",
    )
    .bind(as_of)
    .fetch_one(&state.pool)
    .await;

    let (debit_total, credit_total) = match bal_result {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };
    let deferred_account_balance_cents = credit_total - debit_total;

    let months: Vec<RunoffMonthView> = projection
        .months
        .into_iter()
        .map(|m| RunoffMonthView {
            month: m.month,
            amount_cents: m.amount_cents,
        })
        .collect();

    let schedules_remaining_cents = projection.remaining_total_cents;
    let drift_cents = deferred_account_balance_cents - schedules_remaining_cents;

    Json(DeferredRevenueRunoffResponse {
        as_of,
        horizon_months,
        deferred_account_balance_cents,
        schedules_remaining_cents,
        drift_cents,
        months,
        beyond_horizon_cents: projection.beyond_horizon_cents,
        currency: "USD".to_string(),
    })
    .into_response()
}
