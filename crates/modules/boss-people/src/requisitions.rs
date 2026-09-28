//! Requisition endpoints — hiring pipeline management.
//!
//! Audit-chain note: write paths route through `DomainPublisher`
//! so every requisition open / status flip lands in `audit_log`.
//! `rebuild_people` consumes `people.requisition.opened` and
//! reproduces the projection from the log alone.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_core::publisher::DomainPublisher;
use boss_policy_client::{Action, CurrentUser, PolicyClient, Resource};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::events::REQUISITION_OPENED;

#[derive(Clone)]
pub struct RequisitionsState {
    pub pool: Arc<PgPool>,
    /// Audit-log + NATS publisher. `None` allowed for tests that
    /// only exercise the projection write.
    pub publisher: Option<DomainPublisher>,
    /// Authoritative clock. Every handler reads `now` through it so
    /// audit_log timestamps follow the deployment's sim/wall mode
    /// instead of leaking wallclock.
    pub clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
    /// Gates the list the way the roster is gated (backlog cda177ef),
    /// and the open: Create on `employee`, in a scope that covers the
    /// requisition (backlog dda8fd97). `None` only in tests, where the
    /// gate allows.
    pub policy: Option<Arc<dyn PolicyClient>>,
}

pub fn requisitions_router(
    pool: PgPool,
    publisher: Option<DomainPublisher>,
    clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
    policy: Option<Arc<dyn PolicyClient>>,
) -> Router {
    let state = RequisitionsState {
        pool: Arc::new(pool),
        publisher,
        clock,
        policy,
    };
    Router::new()
        .route(
            "/api/people/requisitions",
            get(list_requisitions).post(create_requisition),
        )
        .with_state(state)
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Requisition {
    pub id: String,
    pub role: String,
    pub department: String,
    pub status: String,
    pub opened_on: NaiveDate,
    pub target_fill_date: NaiveDate,
    pub location: String,
    pub headcount: i16,
    pub hiring_manager_id: String,
}

/// The open requisitions the caller's `employee` Read grant covers
/// (backlog cda177ef: this list asked nobody, so a Basic guest read
/// the hiring plan — roles, departments, headcount and each hiring
/// manager's id). A requisition is covered as its hiring manager's row
/// in its department would be: `all` sees every one, `department:<d>`
/// that department's, `team` and `self` the ones the caller or a
/// report of theirs is hiring for.
async fn list_requisitions(
    State(state): State<RequisitionsState>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let scope = match crate::grants::roster_scope(state.policy.as_ref(), &user).await {
        Ok(scope) => scope,
        Err(refused) => return refused,
    };
    let rows: Result<Vec<Requisition>, _> = sqlx::query_as(
        "SELECT id, role, department, status, opened_on, target_fill_date, location, headcount, hiring_manager_id \
         FROM requisitions ORDER BY opened_on DESC",
    )
    .fetch_all(state.pool.as_ref())
    .await;

    match rows {
        Ok(data) => Json(
            data.into_iter()
                .filter(|r| {
                    crate::grants::covers(&scope, &user, &r.hiring_manager_id, Some(&r.department))
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Opening a requisition is hiring, so it asks what hiring asks —
/// Create on `employee` (backlog dda8fd97, 2026-09-27: this route took
/// no caller, so a request with no identity, a header naming the
/// visitor role, or any employee's session opened one, and through the
/// upsert's conflict arm rewrote the status of one already open). The
/// grant must cover the requisition as its hiring manager's row in its
/// department would be: `all` every one, `department:<d>` that
/// department's, `team` and `self` the ones the caller or a report of
/// theirs is hiring for. The request is judged before the transaction
/// opens, and an existing row of the same id is judged again as stored,
/// inside it.
async fn create_requisition(
    State(state): State<RequisitionsState>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<Requisition>,
) -> Response {
    let scope = match crate::grants::require(
        state.policy.as_ref(),
        &user,
        Action::Create,
        Resource::employee(),
    )
    .await
    {
        Ok(scope) => scope,
        Err(refused) => return refused,
    };
    if !crate::grants::covers(&scope, &user, &req.hiring_manager_id, Some(&req.department)) {
        return (
            StatusCode::FORBIDDEN,
            format!(
                "requisition {} ({}) is outside your Create grant on `employee`",
                req.id, req.department
            ),
        )
            .into_response();
    }
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // An EXISTING row is judged as stored (adversarial review of
    // 8388132a): the upsert's conflict arm rewrites THAT row's status,
    // so checking the request alone let a `department:hr` grant restate
    // a service requisition as hr and flip it, and a `self` or `team`
    // grant do the same by naming itself as the hiring manager — while
    // the event recorded a department the row never had. The advisory
    // lock serialises writers of one id, so a row created by a racing
    // request is seen here rather than overwritten unjudged; FOR UPDATE
    // holds the row itself.
    if let Err(e) = sqlx::query("SELECT pg_advisory_xact_lock(hashtext('requisition:' || $1))")
        .bind(&req.id)
        .execute(&mut *tx)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let stored: Option<Requisition> = match sqlx::query_as(
        "SELECT id, role, department, status, opened_on, target_fill_date, location, headcount, hiring_manager_id \
         FROM requisitions WHERE id = $1 FOR UPDATE",
    )
    .bind(&req.id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let recorded = match stored {
        None => req,
        Some(stored) => {
            if !crate::grants::covers(
                &scope,
                &user,
                &stored.hiring_manager_id,
                Some(&stored.department),
            ) {
                // Names only the id the caller sent: the stored
                // department is not theirs to learn from a refusal.
                return (
                    StatusCode::FORBIDDEN,
                    format!(
                        "requisition {} is outside your Create grant on `employee`",
                        req.id
                    ),
                )
                    .into_response();
            }
            let restated = restated_fields(&stored, &req);
            if !restated.is_empty() {
                return (
                    StatusCode::CONFLICT,
                    format!(
                        "requisition {} exists; this route moves only its status, and the \
                         request restates {}",
                        req.id,
                        restated.join(", ")
                    ),
                )
                    .into_response();
            }
            Requisition {
                status: req.status,
                ..stored
            }
        }
    };
    // Stamp minted before the row write: created_at and the event's
    // timestamp must be ONE instant or replay diverges.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    if let Err(e) = upsert_requisition(&mut *tx, &recorded, stamp.timestamp).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    // OUTBOX (phase 2): the opened event (full row state; status
    // transitions ride the same kind via the ON CONFLICT DO UPDATE
    // path) records with the row — the row as stored, so the log
    // never holds a field the projection does not.
    let event = stamp.event(
        REQUISITION_OPENED,
        serde_json::to_value(&recorded).unwrap_or_default(),
    );
    if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"ok": true, "id": recorded.id})),
    )
        .into_response()
}

/// The fields of `req` that differ from the `stored` requisition of
/// the same id, other than `status` — the one field this route moves on
/// an existing row. The upsert would keep every other stored value, so
/// a request that restates one is refused rather than half-applied.
fn restated_fields(stored: &Requisition, req: &Requisition) -> Vec<&'static str> {
    [
        ("role", stored.role != req.role),
        ("department", stored.department != req.department),
        ("opened_on", stored.opened_on != req.opened_on),
        (
            "target_fill_date",
            stored.target_fill_date != req.target_fill_date,
        ),
        ("location", stored.location != req.location),
        ("headcount", stored.headcount != req.headcount),
        (
            "hiring_manager_id",
            stored.hiring_manager_id != req.hiring_manager_id,
        ),
    ]
    .into_iter()
    .filter_map(|(field, differs)| differs.then_some(field))
    .collect()
}

/// Single canonical UPSERT for `requisitions`. Used by both the
/// handler and the rebuilder. ON CONFLICT updates `status` so a
/// requisition opening at "open" can later transition to
/// "interviewing"/"filled" via the same event kind without a
/// separate status-change family.
///
/// `created_at` is the event's recorded instant — the live caller
/// passes `stamp.timestamp`, the rebuilder passes `ev.ts`. On
/// conflict the column keeps the first insert's instant, live and
/// replayed alike (packet d7b8158e).
pub(crate) async fn upsert_requisition<'e, E>(
    executor: E,
    req: &Requisition,
    created_at: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO requisitions (id, role, department, status, opened_on, target_fill_date, location, headcount, hiring_manager_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status",
    )
    .bind(&req.id)
    .bind(&req.role)
    .bind(&req.department)
    .bind(&req.status)
    .bind(req.opened_on)
    .bind(req.target_fill_date)
    .bind(&req.location)
    .bind(req.headcount)
    .bind(&req.hiring_manager_id)
    .bind(created_at)
    .execute(executor)
    .await
    .map(|_| ())
}
