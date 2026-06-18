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
}

pub fn requisitions_router(
    pool: PgPool,
    publisher: Option<DomainPublisher>,
    clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
) -> Router {
    let state = RequisitionsState {
        pool: Arc::new(pool),
        publisher,
        clock,
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

async fn list_requisitions(State(state): State<RequisitionsState>) -> Response {
    let rows: Result<Vec<Requisition>, _> = sqlx::query_as(
        "SELECT id, role, department, status, opened_on, target_fill_date, location, headcount, hiring_manager_id \
         FROM requisitions ORDER BY opened_on DESC",
    )
    .fetch_all(state.pool.as_ref())
    .await;

    match rows {
        Ok(data) => Json(data).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn create_requisition(
    State(state): State<RequisitionsState>,
    _headers: axum::http::HeaderMap,
    Json(req): Json<Requisition>,
) -> Response {
    if let Err(e) = upsert_requisition(state.pool.as_ref(), &req).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    if let Some(pub_) = &state.publisher {
        let now = boss_clock_client::now_from(&state.clock).await;
        pub_.emit_at(
            REQUISITION_OPENED,
            serde_json::to_value(&req).unwrap_or_default(),
            now,
        )
        .await;
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"ok": true, "id": req.id})),
    )
        .into_response()
}

/// Single canonical UPSERT for `requisitions`. Used by both the
/// handler and the rebuilder. ON CONFLICT updates `status` so a
/// requisition opening at "open" can later transition to
/// "interviewing"/"filled" via the same event kind without a
/// separate status-change family.
pub(crate) async fn upsert_requisition<'e, E>(executor: E, req: &Requisition) -> sqlx::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO requisitions (id, role, department, status, opened_on, target_fill_date, location, headcount, hiring_manager_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
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
    .execute(executor)
    .await
    .map(|_| ())
}
