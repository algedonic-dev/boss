//! Axum HTTP API for the calendar service.
//!
//! Routes:
//! - `POST /api/calendar/reservations` — create one. Returns 201
//!   with `{ "id": "<uuid>" }` on success, 409 with the existing
//!   conflicting rows on a hard-overlap collision, 400 on a
//!   malformed window.
//! - `GET  /api/calendar/reservations?resource_kind=...&resource_id=...&start=...&end=...`
//!   — list every active reservation on the given resource that
//!   intersects the window. RFC3339 datetimes; both `start` + `end`
//!   required.
//! - `DELETE /api/calendar/reservations/{id}?actor=...` — soft-cancel.
//!   Idempotent. 404 if the id doesn't exist.
//! - `POST /api/calendar/cancel-by-reason` — cascade cancel by
//!   `(reason_kind, reason_ref_id)`. Body: `{kind, ref_id, actor}`.
//!   Returns `{ "cancelled": <n> }`.
//! - `POST /api/calendar/business-calendars/batch` — seed/replace
//!   business calendars. Body: `Vec<BusinessCalendar>`. Operator-gated
//!   (with the `x-sim-origin` bypass). Returns
//!   `{ "received": <n>, "upserted": <m> }`.
//! - `GET  /api/calendar/business-calendars/{code}` — fetch one business
//!   calendar with its full closed-day set, or 404. Open read.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use boss_core::calendar::{BusinessCalendar, ReservationId, ReservationRequest, TimeWindow};
use boss_core::job::Subject;
use boss_policy_client::{AccessTier, CurrentUser};

use crate::port::{CalendarClient, CalendarError};

#[derive(Clone)]
pub struct CalendarApiState {
    pub calendar: Arc<dyn CalendarClient>,
    /// Domain event publisher. Optional so test setups that don't
    /// wire NATS keep working; production binaries always attach a
    /// `PgAuditWriter` so reservation events land in `audit_log`.
    pub publisher: Option<boss_core::publisher::DomainPublisher>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router(state: CalendarApiState) -> Router {
    Router::new()
        .route("/api/calendar/health", get(health))
        .route(
            "/api/calendar/reservations",
            post(create_reservation).get(list_reservations),
        )
        .route(
            "/api/calendar/reservations/{id}",
            delete(cancel_reservation),
        )
        .route("/api/calendar/cancel-by-reason", post(cancel_by_reason))
        .route(
            "/api/calendar/business-calendars/batch",
            post(batch_business_calendars),
        )
        .route(
            "/api/calendar/business-calendars/{code}",
            get(get_business_calendar),
        )
        .with_state(state)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> axum::Json<boss_core::startup::HealthResponse> {
    axum::Json(boss_core::startup::health_response(
        "boss-calendar-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

async fn create_reservation(
    State(state): State<CalendarApiState>,
    Json(req): Json<ReservationRequest>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.calendar.reserve_at(req, now).await {
        Ok(id) => {
            if let Some(pub_) = &state.publisher
                && let Ok(Some(reservation)) = state.calendar.get(id).await
            {
                pub_.emit_at(
                    crate::events::RESERVATION_RESERVED,
                    serde_json::to_value(&reservation).unwrap_or_default(),
                    now,
                )
                .await;
            }
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Err(e) => calendar_error_response(e),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    resource_kind: String,
    resource_id: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

async fn list_reservations(
    State(state): State<CalendarApiState>,
    Query(q): Query<ListQuery>,
) -> Response {
    // The kind is open (registry-validated); a reservation is on a
    // Subject. The `resource_kind`/`resource_id` query params are the
    // calendar's I/O label for that subject.
    let subject = Subject::new(q.resource_kind, q.resource_id);
    let window = match TimeWindow::new(q.start, q.end) {
        Ok(w) => w,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    match state.calendar.list(&subject, window).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => calendar_error_response(e),
    }
}

#[derive(Deserialize)]
struct CancelQuery {
    /// Audit-trail tag — recorded in the cancellation log. Defaults
    /// to "unknown" when callers forget; we don't 400 since the
    /// cancel itself is idempotent.
    #[serde(default = "default_actor")]
    actor: String,
}

fn default_actor() -> String {
    "unknown".to_string()
}

async fn cancel_reservation(
    State(state): State<CalendarApiState>,
    Path(id): Path<String>,
    Query(q): Query<CancelQuery>,
) -> Response {
    let uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "id must be a UUID").into_response();
        }
    };
    let res_id = ReservationId::from_uuid(uuid);
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.calendar.cancel_at(res_id, &q.actor, now).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher
                && let Ok(Some(reservation)) = state.calendar.get(res_id).await
            {
                pub_.emit_at(
                    crate::events::RESERVATION_CANCELLED,
                    serde_json::to_value(&reservation).unwrap_or_default(),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => calendar_error_response(e),
    }
}

#[derive(Deserialize)]
struct CancelByReasonBody {
    kind: String,
    ref_id: String,
    #[serde(default = "default_actor")]
    actor: String,
}

async fn cancel_by_reason(
    State(state): State<CalendarApiState>,
    Json(body): Json<CancelByReasonBody>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    // Snapshot the rows about to be cancelled so we can emit one
    // CANCELLED event per affected row — required for the rebuild
    // path to reproduce post-cascade state.
    let about_to_cancel = if state.publisher.is_some() {
        state
            .calendar
            .list_active_by_reason(&body.kind, &body.ref_id)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    match state
        .calendar
        .cancel_by_reason_at(&body.kind, &body.ref_id, &body.actor, now)
        .await
    {
        Ok(n) => {
            if let Some(pub_) = &state.publisher {
                for r in about_to_cancel {
                    let cancelled = boss_core::calendar::Reservation {
                        cancelled_at: Some(now),
                        ..r
                    };
                    pub_.emit_at(
                        crate::events::RESERVATION_CANCELLED,
                        serde_json::to_value(&cancelled).unwrap_or_default(),
                        now,
                    )
                    .await;
                }
            }
            Json(serde_json::json!({ "cancelled": n })).into_response()
        }
        Err(e) => calendar_error_response(e),
    }
}

/// Batch-upsert business calendars — the seed surface, used to load
/// the `us-banking` / `us-tax` reference calendars from JSON instead of
/// `psql -f`. Each calendar is upserted by `code`; its closed-day set is
/// replaced wholesale.
///
/// Gated to operator-tier callers, with the `x-sim-origin` bypass that
/// every seed path honors (the trusted simulator/seeder masquerades as
/// operators; its requests carry `x-sim-origin: true`, which the
/// request-context middleware scopes into `is_in_sim_chain`). Reads stay
/// open; only this write is privileged.
async fn batch_business_calendars(
    State(state): State<CalendarApiState>,
    CurrentUser(user): CurrentUser,
    Json(calendars): Json<Vec<BusinessCalendar>>,
) -> Response {
    let sim = boss_core::sim_origin::is_in_sim_chain();
    let tier_ok = matches!(user.access_tier, AccessTier::Operator);
    if !(sim || tier_ok) {
        return (StatusCode::FORBIDDEN, "operator tier required").into_response();
    }

    let received = calendars.len();
    match state.calendar.upsert_business_calendars(&calendars).await {
        Ok(upserted) => Json(serde_json::json!({
            "received": received,
            "upserted": upserted,
        }))
        .into_response(),
        Err(e) => calendar_error_response(e),
    }
}

/// Fetch one business calendar by `code` with its full closed-day set,
/// or 404 if no such calendar exists. Open read — callers run the
/// business-day math locally via `boss_core::calendar::BusinessCalendar`.
async fn get_business_calendar(
    State(state): State<CalendarApiState>,
    Path(code): Path<String>,
) -> Response {
    match state.calendar.get_business_calendar(&code).await {
        Ok(Some(cal)) => Json(cal).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such business calendar").into_response(),
        Err(e) => calendar_error_response(e),
    }
}

fn calendar_error_response(err: CalendarError) -> Response {
    match err {
        CalendarError::Conflict { existing } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "conflict",
                "existing": existing,
            })),
        )
            .into_response(),
        CalendarError::NotFound(id) => (
            StatusCode::NOT_FOUND,
            format!("reservation not found: {id}"),
        )
            .into_response(),
        CalendarError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        CalendarError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::TimeZone;
    use tower::ServiceExt;

    use crate::in_memory::InMemoryCalendar;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 27, h, 0, 0).unwrap()
    }

    fn app() -> Router {
        let cal: Arc<dyn CalendarClient> = Arc::new(InMemoryCalendar::new());
        router(CalendarApiState {
            calendar: cal,
            publisher: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    #[tokio::test]
    async fn health_ok() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/api/calendar/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn create_then_list_round_trips() {
        let app = app();
        let req_body = serde_json::json!({
            "subject": {"subject_kind": "employee", "id": "emp-1"},
            "window": {"start": t(10), "end": t(12)},
            "reason_kind": "job-step",
            "reason_ref_id": "stp-1",
            "strength": "hard",
            "created_by": "test",
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/calendar/reservations")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // RFC3339 contains `+00:00` for UTC offset; URL-encode the
        // `+` so axum's Query extractor doesn't read it as a space.
        let start = t(9).to_rfc3339().replace('+', "%2B");
        let end = t(13).to_rfc3339().replace('+', "%2B");
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/calendar/reservations?resource_kind=employee&resource_id=emp-1&start={start}&end={end}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["reason_ref_id"], "stp-1");
    }

    #[tokio::test]
    async fn hard_overlap_returns_409_with_existing() {
        let app = app();
        let req_a = serde_json::json!({
            "subject": {"subject_kind": "employee", "id": "emp-1"},
            "window": {"start": t(10), "end": t(12)},
            "reason_kind": "job-step",
            "reason_ref_id": "stp-1",
            "strength": "hard",
            "created_by": "test",
        });
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/calendar/reservations")
                    .header("content-type", "application/json")
                    .body(Body::from(req_a.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let req_b = serde_json::json!({
            "subject": {"subject_kind": "employee", "id": "emp-1"},
            "window": {"start": t(11), "end": t(13)},
            "reason_kind": "job-step",
            "reason_ref_id": "stp-2",
            "strength": "hard",
            "created_by": "test",
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/calendar/reservations")
                    .header("content-type", "application/json")
                    .body(Body::from(req_b.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "conflict");
        assert_eq!(json["existing"].as_array().unwrap().len(), 1);
        assert_eq!(json["existing"][0]["reason_ref_id"], "stp-1");
    }

    // --- business-calendar batch (operator-gated) + get round-trip ---

    /// `x-boss-user` JSON for an operator-tier caller — mirrors the
    /// header the gateway injects + the seed binaries send.
    fn operator_header() -> String {
        serde_json::json!({
            "id": "automation:test-seed",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
        })
        .to_string()
    }

    fn batch_request(user_header: Option<&str>, body: serde_json::Value) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/calendar/business-calendars/batch")
            .header("content-type", "application/json");
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    fn one_calendar(closed: &[&str]) -> serde_json::Value {
        serde_json::json!([{
            "code": "us-banking",
            "name": "US Banking",
            "weekend": [5, 6],
            "closed": closed,
        }])
    }

    async fn get_calendar(app: &Router, code: &str) -> (StatusCode, Option<serde_json::Value>) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/calendar/business-calendars/{code}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        if status != StatusCode::OK {
            return (status, None);
        }
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, Some(serde_json::from_slice(&bytes).unwrap()))
    }

    #[tokio::test]
    async fn get_business_calendar_404_for_missing() {
        let (status, _) = get_calendar(&app(), "no-such-calendar").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn batch_upsert_inserts_for_operator_then_get_round_trips() {
        let app = app();
        let resp = app
            .clone()
            .oneshot(batch_request(
                Some(&operator_header()),
                one_calendar(&["2026-01-01", "2026-07-03"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["received"], serde_json::json!(1));
        assert_eq!(v["upserted"], serde_json::json!(1));

        let (status, cal) = get_calendar(&app, "us-banking").await;
        assert_eq!(status, StatusCode::OK);
        let cal = cal.unwrap();
        assert_eq!(cal["code"], "us-banking");
        assert_eq!(
            cal["closed"],
            serde_json::json!(["2026-01-01", "2026-07-03"])
        );
    }

    #[tokio::test]
    async fn batch_upsert_replaces_closed_set_wholesale() {
        let app = app();
        // v1: one closed day.
        let r1 = app
            .clone()
            .oneshot(batch_request(
                Some(&operator_header()),
                one_calendar(&["2026-01-01"]),
            ))
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::OK);
        // v2 (same code): a different closed set — replaces, does NOT merge.
        let r2 = app
            .clone()
            .oneshot(batch_request(
                Some(&operator_header()),
                one_calendar(&["2026-07-03", "2026-12-25"]),
            ))
            .await
            .unwrap();
        assert_eq!(r2.status(), StatusCode::OK);

        let (_, cal) = get_calendar(&app, "us-banking").await;
        assert_eq!(
            cal.unwrap()["closed"],
            serde_json::json!(["2026-07-03", "2026-12-25"]),
            "re-seed replaces the closed set wholesale (no merge with v1)"
        );
    }

    #[tokio::test]
    async fn batch_upsert_forbidden_for_non_operator() {
        // No `x-boss-user` header → anonymous, AccessTier::User.
        let resp = app()
            .oneshot(batch_request(None, one_calendar(&["2026-01-01"])))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn batch_upsert_bypassed_by_sim_origin() {
        // Sim traffic carries `x-sim-origin: true`, scoped into
        // `is_in_sim_chain`. The router omits that middleware, so set the
        // task-local directly to exercise the bypass with an anonymous caller.
        let app = app();
        let resp = boss_core::sim_origin::with_sim_chain(
            true,
            app.clone()
                .oneshot(batch_request(None, one_calendar(&["2026-01-01"]))),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let (status, _) = get_calendar(&app, "us-banking").await;
        assert_eq!(status, StatusCode::OK);
    }
}
