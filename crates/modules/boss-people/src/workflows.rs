//! HR workflow endpoints — onboarding, offboarding, and employee changes.
//!
//! Audit-chain note: write paths route through `DomainPublisher`
//! so every status flip + change-log row lands in `audit_log`.
//! `rebuild_people` consumes both `people.employee.updated` (full
//! Employee row state) and `people.employee.change-recorded` (the
//! per-flip audit-trail row), so a `boss-rebuild-all` cycle
//! reproduces both the projection AND the change history.
//!
//! Both the status flip and the `employee_changes` row are emitted as
//! audit_log events, not written straight to the projection — a raw
//! `UPDATE employees SET status` + `INSERT INTO employee_changes`
//! would be silently wiped on the next rebuild (CASCADE through the FK
//! to `employees`).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use boss_core::publisher::DomainPublisher;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::events::{EMPLOYEE_CHANGE_RECORDED, EMPLOYEE_UPDATED};
use crate::port::PeopleRepository;

#[derive(Clone)]
pub struct WorkflowState {
    pub pool: Arc<PgPool>,
    /// Canonical Employee writer. Used to mutate the projection row
    /// in lockstep with the audit_log emit so both stay in sync.
    pub people: Arc<dyn PeopleRepository>,
    /// Audit_log + NATS bus. `None` is allowed for tests that don't
    /// exercise the audit-chain path (the handler then writes to the
    /// projection only).
    pub publisher: Option<DomainPublisher>,
    /// Authoritative clock. Every handler reads `now` through it so
    /// audit_log timestamps follow the deployment's sim/wall mode
    /// instead of leaking wallclock.
    pub clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
}

pub fn workflow_router(
    pool: PgPool,
    people: Arc<dyn PeopleRepository>,
    publisher: Option<DomainPublisher>,
    clock: Arc<dyn boss_clock_client::ClockClient>,
) -> Router {
    let state = WorkflowState {
        pool: Arc::new(pool),
        people,
        publisher,
        clock,
    };
    Router::new()
        .route("/api/people/{id}/status", put(update_status))
        .route("/api/people/{id}/changes", get(list_changes))
        .route("/api/people/{id}/changes", post(record_change))
        .route("/api/people/{id}/onboard", post(start_onboarding))
        .route("/api/people/{id}/offboard", post(start_offboarding))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct UpdateStatus {
    status: String,
    notes: Option<String>,
    initiated_by: Option<String>,
}

#[derive(Deserialize)]
struct RecordChange {
    kind: String,
    from_value: Option<String>,
    to_value: Option<String>,
    effective_date: NaiveDate,
    notes: Option<String>,
    initiated_by: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
struct EmployeeChange {
    id: i64,
    employee_id: String,
    kind: String,
    from_value: Option<String>,
    to_value: Option<String>,
    effective_date: NaiveDate,
    notes: Option<String>,
    initiated_by: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Audit-trail event payload for `EMPLOYEE_CHANGE_RECORDED`. The
/// rebuilder deserialises this directly to repopulate the
/// `employee_changes` projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeChangeRecord {
    pub employee_id: String,
    pub kind: String,
    pub from_value: Option<String>,
    pub to_value: Option<String>,
    pub effective_date: NaiveDate,
    pub notes: Option<String>,
    pub initiated_by: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn update_status(
    State(state): State<WorkflowState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
    Json(body): Json<UpdateStatus>,
) -> Response {
    // Read the canonical Employee — we need its full state to emit
    // the `EMPLOYEE_UPDATED` event with a complete row payload (the
    // rebuilder requires it for upsert).
    let mut emp = match state.people.employee_by_id(&id).await {
        Ok(Some(e)) => e,
        Ok(None) => return (StatusCode::NOT_FOUND, "employee not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // `status` is a free String validated against the Class registry
    // at write time. The handler just records the transition; the
    // Postgres adapter rejects unknown values via
    // validate_employee_class("status", code).
    let from_status = emp.status.clone();
    let new_status = body.status.clone();
    emp.status = Some(new_status);

    let now = boss_clock_client::now_from(&state.clock).await;
    if let Err(e) = state.people.update_employee_at(&id, &emp, now).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    let change_kind = derive_change_kind(from_status.as_deref().unwrap_or(""), &body.status);

    if let Err(e) = record_change_inner(
        &state,
        EmployeeChangeRecord {
            employee_id: id.clone(),
            kind: change_kind.to_string(),
            from_value: from_status,
            to_value: Some(body.status.clone()),
            effective_date: now.date_naive(),
            notes: body.notes,
            initiated_by: body.initiated_by,
        },
        now,
    )
    .await
    {
        return e;
    }

    if let Some(pub_) = &state.publisher {
        pub_.emit_at(
            EMPLOYEE_UPDATED,
            serde_json::to_value(&emp).unwrap_or_default(),
            now,
        )
        .await;
    }

    Json(serde_json::json!({"ok": true})).into_response()
}

async fn list_changes(State(state): State<WorkflowState>, Path(id): Path<String>) -> Response {
    let rows: Result<Vec<EmployeeChange>, _> = sqlx::query_as(
        "SELECT id, employee_id, kind, from_value, to_value, effective_date, notes, initiated_by, created_at \
         FROM employee_changes WHERE employee_id = $1 ORDER BY created_at DESC",
    )
    .bind(&id)
    .fetch_all(state.pool.as_ref())
    .await;

    match rows {
        Ok(changes) => Json(changes).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn record_change(
    State(state): State<WorkflowState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
    Json(body): Json<RecordChange>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    if let Err(e) = record_change_inner(
        &state,
        EmployeeChangeRecord {
            employee_id: id,
            kind: body.kind,
            from_value: body.from_value,
            to_value: body.to_value,
            effective_date: body.effective_date,
            notes: body.notes,
            initiated_by: body.initiated_by,
        },
        now,
    )
    .await
    {
        return e;
    }
    (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn start_onboarding(
    State(state): State<WorkflowState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
) -> Response {
    update_employee_status(
        &state,
        &id,
        "active",
        "onboard",
        boss_clock_client::now_from(&state.clock).await,
    )
    .await
}

async fn start_offboarding(
    State(state): State<WorkflowState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
) -> Response {
    update_employee_status(
        &state,
        &id,
        "terminated",
        "offboard",
        boss_clock_client::now_from(&state.clock).await,
    )
    .await
}

async fn update_employee_status(
    state: &WorkflowState,
    employee_id: &str,
    new_status: &str,
    change_kind: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Response {
    let mut emp = match state.people.employee_by_id(employee_id).await {
        Ok(Some(e)) => e,
        Ok(None) => return (StatusCode::NOT_FOUND, "employee not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Registry validates at write time (see above).
    let from_status = emp.status.clone();
    emp.status = Some(new_status.to_string());

    if let Err(e) = state
        .people
        .update_employee_at(employee_id, &emp, now)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    if let Err(resp) = record_change_inner(
        state,
        EmployeeChangeRecord {
            employee_id: employee_id.to_string(),
            kind: change_kind.to_string(),
            from_value: from_status,
            to_value: Some(new_status.to_string()),
            effective_date: now.date_naive(),
            notes: None,
            initiated_by: None,
        },
        now,
    )
    .await
    {
        return resp;
    }

    if let Some(pub_) = &state.publisher {
        pub_.emit_at(
            EMPLOYEE_UPDATED,
            serde_json::to_value(&emp).unwrap_or_default(),
            now,
        )
        .await;
    }

    Json(serde_json::json!({"ok": true})).into_response()
}

/// Single canonical write path for `employee_changes`. Emits the
/// audit_log event first, then writes the projection in the same
/// shape the rebuilder will reproduce. Returns the response only on
/// failure (success continues — caller may chain further work).
async fn record_change_inner(
    state: &WorkflowState,
    rec: EmployeeChangeRecord,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), Response> {
    if let Some(pub_) = &state.publisher {
        pub_.emit_at(
            EMPLOYEE_CHANGE_RECORDED,
            serde_json::to_value(&rec).unwrap_or_default(),
            now,
        )
        .await;
    }
    sqlx::query(
        "INSERT INTO employee_changes (employee_id, kind, from_value, to_value, effective_date, notes, initiated_by, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
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
    .await
    .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response())?;
    Ok(())
}

/// Map a `(from, to)` status pair to the change-log `kind`. Pulled
/// out of the handlers so the same derivation runs for the
/// `update_status` path AND the rebuilder's regression coverage.
fn derive_change_kind(from: &str, to: &str) -> &'static str {
    match (from, to) {
        (_, "terminated") => "offboard",
        (_, "on-leave") => "leave-start",
        ("on-leave", "active") => "leave-end",
        (_, "active") => "onboard",
        _ => "role-change",
    }
}
