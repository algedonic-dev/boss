//! Employee change endpoints — role/salary/department change tracking.
//!
//! Audit-chain note: `POST /api/people/changes` emits
//! `EMPLOYEE_CHANGE_RECORDED` so the row survives `rebuild_people`
//! (rather than being lost on rebuild via the employees CASCADE).

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_clock_client::ClockClient;
use boss_core::publisher::DomainPublisher;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::events::EMPLOYEE_CHANGE_RECORDED;
use crate::workflows::EmployeeChangeRecord;

#[derive(Clone)]
pub struct EmployeeChangesState {
    pub pool: Arc<PgPool>,
    pub publisher: Option<DomainPublisher>,
    pub clock: Arc<dyn ClockClient>,
}

pub fn employee_changes_router(
    pool: PgPool,
    publisher: Option<DomainPublisher>,
    clock: Arc<dyn ClockClient>,
) -> Router {
    let state = EmployeeChangesState {
        pool: Arc::new(pool),
        publisher,
        clock,
    };
    Router::new()
        .route("/api/people/changes", get(list_changes).post(create_change))
        .with_state(state)
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct EmployeeChange {
    pub employee_id: String,
    pub kind: String,
    pub from_value: Option<String>,
    pub to_value: String,
    pub effective_date: NaiveDate,
    pub notes: Option<String>,
    pub initiated_by: Option<String>,
}

async fn list_changes(State(state): State<EmployeeChangesState>) -> Response {
    let rows: Result<Vec<EmployeeChange>, _> = sqlx::query_as(
        "SELECT employee_id, kind, from_value, to_value, effective_date, notes, initiated_by \
         FROM employee_changes ORDER BY effective_date DESC",
    )
    .fetch_all(state.pool.as_ref())
    .await;

    match rows {
        Ok(data) => Json(data).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn create_change(
    State(state): State<EmployeeChangesState>,
    Json(req): Json<EmployeeChange>,
) -> Response {
    let now = state.clock.now().await.now;
    let rec = EmployeeChangeRecord {
        employee_id: req.employee_id,
        kind: req.kind,
        from_value: req.from_value,
        to_value: Some(req.to_value),
        effective_date: req.effective_date,
        notes: req.notes,
        initiated_by: req.initiated_by,
    };

    if let Some(pub_) = &state.publisher {
        pub_.emit_at(
            EMPLOYEE_CHANGE_RECORDED,
            serde_json::to_value(&rec).unwrap_or_default(),
            now,
        )
        .await;
    }

    let result = sqlx::query(
        "INSERT INTO employee_changes (employee_id, kind, from_value, to_value, effective_date, notes, initiated_by, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT DO NOTHING",
    )
    .bind(&rec.employee_id)
    .bind(&rec.kind)
    .bind(&rec.from_value)
    .bind(&rec.to_value)
    .bind(rec.effective_date)
    .bind(&rec.notes)
    .bind(&rec.initiated_by)
    .bind(now)
    .execute(state.pool.as_ref())
    .await;

    match result {
        Ok(_) => (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
