//! Axum routes for the scheduling surfaces.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use boss_clock_client::ClockClient;
use boss_core::publisher::DomainPublisher;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use super::events::{
    ASSIGNMENT_CREATED, ASSIGNMENT_DELETED, ASSIGNMENT_STATUS_CHANGED, AVAILABILITY_CREATED,
    AVAILABILITY_DELETED, CALENDAR_TOKEN_ROTATED, SHIFT_PATTERN_UPSERTED,
};
use super::ics::build_ics;
use super::port::{SchedulingError, SchedulingRepository};
use super::types::{AssignmentStatus, NewScheduledAssignment, NewTechAvailability};

pub struct SchedulingApiState {
    pub repo: Arc<dyn SchedulingRepository>,
    /// Audit-log + NATS publisher. `None` allowed for tests that
    /// only exercise projection writes.
    pub publisher: Option<DomainPublisher>,
    /// Authoritative clock — every emit_at stamps via clock-api so
    /// sim mode produces sim-dated audit_log rows. Same trait as
    /// the rest of the workspace.
    pub clock: Arc<dyn ClockClient>,
}

pub fn router(state: SchedulingApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route(
            "/api/scheduling/availability",
            get(list_avail).post(create_avail),
        )
        .route("/api/scheduling/availability/{id}", delete(delete_avail))
        .route(
            "/api/scheduling/assignments",
            get(list_assign).post(create_assign),
        )
        .route(
            "/api/scheduling/assignments/{id}",
            get(get_assign).delete(delete_assign),
        )
        .route(
            "/api/scheduling/assignments/{id}/status",
            post(update_assign_status),
        )
        .route(
            "/api/scheduling/shift-patterns",
            get(list_shifts).post(upsert_shift),
        )
        .route(
            "/api/scheduling/shift-patterns/materialize",
            post(materialize),
        )
        .route("/api/scheduling/week-grid", get(week_grid))
        .route(
            "/api/scheduling/techs/{emp_id}/calendar-token",
            get(get_calendar_token).post(rotate_calendar_token),
        )
        .route("/ics/{token}/calendar.ics", get(public_ics_feed))
        .with_state(shared)
}

fn err(e: SchedulingError) -> Response {
    match e {
        SchedulingError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        SchedulingError::BadRequest(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        SchedulingError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Availability
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RangeQuery {
    /// RFC 3339 timestamps; defaults to now → +7 days.
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    employee_id: Option<String>,
}

fn resolve_range(q: &RangeQuery) -> (DateTime<Utc>, DateTime<Utc>) {
    let from = q.from.unwrap_or_else(Utc::now);
    let to = q.to.unwrap_or_else(|| from + Duration::days(7));
    (from, to)
}

async fn list_avail(
    State(state): State<Arc<SchedulingApiState>>,
    Query(q): Query<RangeQuery>,
) -> Response {
    let (from, to) = resolve_range(&q);
    match state
        .repo
        .list_availability(q.employee_id.as_deref(), from, to)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn create_avail(
    State(state): State<Arc<SchedulingApiState>>,
    Json(body): Json<NewTechAvailability>,
) -> Response {
    match state.repo.create_availability(body).await {
        Ok(row) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    AVAILABILITY_CREATED,
                    serde_json::to_value(&row).unwrap_or_default(),
                    boss_clock_client::now_from(&state.clock).await,
                )
                .await;
            }
            (StatusCode::CREATED, Json(row)).into_response()
        }
        Err(e) => err(e),
    }
}

async fn delete_avail(
    State(state): State<Arc<SchedulingApiState>>,
    Path(id): Path<Uuid>,
) -> Response {
    match state.repo.delete_availability(id).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    AVAILABILITY_DELETED,
                    serde_json::json!({"id": id, "deleted_at": now}),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err(e),
    }
}

// ---------------------------------------------------------------------------
// Assignments
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AssignQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    tech_id: Option<String>,
    target_job_id: Option<Uuid>,
}

async fn list_assign(
    State(state): State<Arc<SchedulingApiState>>,
    Query(q): Query<AssignQuery>,
) -> Response {
    let from = q.from.unwrap_or_else(Utc::now);
    let to = q.to.unwrap_or_else(|| from + Duration::days(7));
    match state
        .repo
        .list_assignments(q.tech_id.as_deref(), q.target_job_id, from, to)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn create_assign(
    State(state): State<Arc<SchedulingApiState>>,
    Json(body): Json<NewScheduledAssignment>,
) -> Response {
    match state.repo.create_assignment(body).await {
        Ok(row) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    ASSIGNMENT_CREATED,
                    serde_json::to_value(&row).unwrap_or_default(),
                    boss_clock_client::now_from(&state.clock).await,
                )
                .await;
            }
            (StatusCode::CREATED, Json(row)).into_response()
        }
        Err(e) => err(e),
    }
}

async fn get_assign(
    State(state): State<Arc<SchedulingApiState>>,
    Path(id): Path<Uuid>,
) -> Response {
    match state.repo.get_assignment(id).await {
        Ok(Some(row)) => Json(row).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, id.to_string()).into_response(),
        Err(e) => err(e),
    }
}

async fn delete_assign(
    State(state): State<Arc<SchedulingApiState>>,
    Path(id): Path<Uuid>,
) -> Response {
    match state.repo.delete_assignment(id).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    ASSIGNMENT_DELETED,
                    serde_json::json!({"id": id, "deleted_at": now}),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct StatusBody {
    status: AssignmentStatus,
}

async fn update_assign_status(
    State(state): State<Arc<SchedulingApiState>>,
    Path(id): Path<Uuid>,
    Json(body): Json<StatusBody>,
) -> Response {
    match state.repo.update_assignment_status(id, body.status).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    ASSIGNMENT_STATUS_CHANGED,
                    serde_json::json!({
                        "id": id,
                        "status": body.status,
                        "changed_at": now,
                    }),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err(e),
    }
}

// ---------------------------------------------------------------------------
// Shift patterns
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ShiftQuery {
    employee_id: Option<String>,
}

async fn list_shifts(
    State(state): State<Arc<SchedulingApiState>>,
    Query(q): Query<ShiftQuery>,
) -> Response {
    match state
        .repo
        .list_shift_patterns(q.employee_id.as_deref())
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct UpsertShiftBody {
    employee_id: String,
    day_of_week: i16,
    starts_at_time: chrono::NaiveTime,
    ends_at_time: chrono::NaiveTime,
    #[serde(default = "default_tz")]
    timezone: String,
    #[serde(default)]
    effective_from: Option<chrono::NaiveDate>,
}
fn default_tz() -> String {
    "America/Los_Angeles".to_string()
}

async fn upsert_shift(
    State(state): State<Arc<SchedulingApiState>>,
    Json(body): Json<UpsertShiftBody>,
) -> Response {
    let eff = body
        .effective_from
        .unwrap_or_else(|| Utc::now().date_naive());
    match state
        .repo
        .upsert_shift_pattern(
            &body.employee_id,
            body.day_of_week,
            body.starts_at_time,
            body.ends_at_time,
            &body.timezone,
            eff,
        )
        .await
    {
        Ok(row) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    SHIFT_PATTERN_UPSERTED,
                    serde_json::to_value(&row).unwrap_or_default(),
                    boss_clock_client::now_from(&state.clock).await,
                )
                .await;
            }
            Json(row).into_response()
        }
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct MaterializeBody {
    #[serde(default = "default_weeks_ahead")]
    weeks_ahead: i64,
}
fn default_weeks_ahead() -> i64 {
    super::materialize::DEFAULT_WEEKS_AHEAD
}

async fn materialize(
    State(state): State<Arc<SchedulingApiState>>,
    Json(body): Json<MaterializeBody>,
) -> Response {
    match super::materialize::materialize_next(state.repo.as_ref(), body.weeks_ahead).await {
        Ok(inserted) => Json(serde_json::json!({ "inserted": inserted })).into_response(),
        Err(e) => err(e),
    }
}

// ---------------------------------------------------------------------------
// Week grid projection
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WeekGridQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    /// Comma-separated employee IDs; empty means "all techs".
    employees: Option<String>,
}

// ---------------------------------------------------------------------------
// ICS calendar feed — token management + public feed endpoint
// ---------------------------------------------------------------------------

/// Window the public feed exposes. 90 days back covers "what did I do
/// last quarter", 180 days forward covers the tech's visible horizon
/// without flooding their calendar client with distant tentative work.
const ICS_PAST_DAYS: i64 = 90;
const ICS_FUTURE_DAYS: i64 = 180;

async fn get_calendar_token(
    State(state): State<Arc<SchedulingApiState>>,
    Path(emp_id): Path<String>,
) -> Response {
    match state.repo.calendar_token_for(&emp_id).await {
        Ok(Some(t)) => Json(serde_json::json!({
            "employee_id": emp_id,
            "token": t,
            "ics_url": format!("/ics/{t}/calendar.ics"),
        }))
        .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("no calendar token for {emp_id}"),
        )
            .into_response(),
        Err(e) => err(e),
    }
}

async fn rotate_calendar_token(
    State(state): State<Arc<SchedulingApiState>>,
    Path(emp_id): Path<String>,
) -> Response {
    // Two v4 UUIDs concatenated = 256 bits of randomness. `simple()`
    // format emits 32 hex chars per UUID, so the token is a 64-char
    // URL-safe string.
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    match state.repo.rotate_calendar_token(&emp_id, &token).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    CALENDAR_TOKEN_ROTATED,
                    serde_json::json!({
                        "employee_id": emp_id,
                        "token": token,
                        "rotated_at": now,
                    }),
                    now,
                )
                .await;
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "employee_id": emp_id,
                    "token": token,
                    "ics_url": format!("/ics/{token}/calendar.ics"),
                })),
            )
                .into_response()
        }
        Err(e) => err(e),
    }
}

async fn public_ics_feed(
    State(state): State<Arc<SchedulingApiState>>,
    Path(token): Path<String>,
) -> Response {
    let emp_id = match state.repo.employee_by_calendar_token(&token).await {
        Ok(Some(e)) => e,
        Ok(None) => return (StatusCode::NOT_FOUND, "unknown calendar token").into_response(),
        Err(e) => return err(e),
    };

    let now = Utc::now();
    let from = now - Duration::days(ICS_PAST_DAYS);
    let to = now + Duration::days(ICS_FUTURE_DAYS);

    let assignments = match state
        .repo
        .list_assignments(Some(&emp_id), None, from, to)
        .await
    {
        Ok(rows) => rows,
        Err(e) => return err(e),
    };
    let availability = match state.repo.list_availability(Some(&emp_id), from, to).await {
        Ok(rows) => rows,
        Err(e) => return err(e),
    };

    let body = build_ics(&emp_id, &assignments, &availability, now);
    (
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/calendar; charset=utf-8",
            ),
            (axum::http::header::CACHE_CONTROL, "private, max-age=300"),
        ],
        body,
    )
        .into_response()
}

async fn week_grid(
    State(state): State<Arc<SchedulingApiState>>,
    Query(q): Query<WeekGridQuery>,
) -> Response {
    let from = q.from.unwrap_or_else(Utc::now);
    let to = q.to.unwrap_or_else(|| from + Duration::days(7));
    let emp_vec: Option<Vec<String>> = q.employees.as_deref().map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    });
    let slice: Option<&[String]> = emp_vec.as_deref();
    match state.repo.week_grid(from, to, slice).await {
        Ok(rows) => Json(serde_json::json!({
            "from": from,
            "to": to,
            "rows": rows,
        }))
        .into_response(),
        Err(e) => err(e),
    }
}
