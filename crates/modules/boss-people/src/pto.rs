//! `POST /api/people/pto` — book PTO for an employee as a calendar
//! reservation.
//!
//! First consumer of `boss-calendar-client`
//! (docs/architecture-decisions.md §Calendar: PTO lives in HR and
//! the calendar sees only approved PTO). The endpoint
//! returns 503 when calendar isn't configured so boss-people-api
//! can deploy independently of the calendar service.
//!
//! On collision (the employee already has an overlapping hard
//! reservation — e.g. a job-step), the calendar service replies
//! 409 with the existing rows; we forward that body to the caller
//! so the UI can render "your PTO overlaps with Job-12345".

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use boss_calendar_client::{CalendarClient, CalendarClientError};
use boss_core::calendar::{
    Reservation, ReservationId, ReservationRequest, ReservationStrength, TimeWindow, reason,
};
use boss_core::job::Subject;

/// PTO API state — wraps the calendar client. `Option` reflects
/// the config being optional; if the calendar isn't configured the
/// endpoint returns 503 rather than panicking on a missing client.
#[derive(Clone)]
pub struct PtoApiState {
    pub calendar: Option<Arc<dyn CalendarClient>>,
}

pub fn pto_router(state: PtoApiState) -> Router {
    Router::new()
        .route("/api/people/pto", post(create_pto))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
pub struct CreatePtoRequest {
    pub employee_id: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Free-form context — optional. "Family vacation",
    /// "personal day", etc. Recorded on the reservation row's
    /// `notes` column.
    #[serde(default)]
    pub notes: Option<String>,
    /// Who's submitting this (HR generalist, the employee
    /// themselves, an admin). Lands in `created_by`.
    pub created_by: String,
    /// Stable identifier for this PTO request. Drives the
    /// reservation's `reason_ref_id` so cancellation cascades
    /// cleanly when an HR workflow rejects/withdraws the
    /// request. Caller picks the value (typically `"pto-<uuid>"`
    /// or the HR-side row id).
    pub request_id: String,
}

#[derive(Debug, Serialize)]
pub struct CreatePtoResponse {
    pub reservation_id: ReservationId,
}

#[derive(Debug, Serialize)]
pub struct ConflictBody {
    pub error: &'static str,
    pub existing: Vec<Reservation>,
}

async fn create_pto(
    State(state): State<PtoApiState>,
    Json(req): Json<CreatePtoRequest>,
) -> Response {
    let Some(calendar) = state.calendar else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "calendar service not configured (set calendar_api_url \
             in boss-people-api.toml)",
        )
            .into_response();
    };
    let window = match TimeWindow::new(req.start, req.end) {
        Ok(w) => w,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let cal_req = ReservationRequest {
        subject: Subject::new("employee", req.employee_id),
        window,
        reason_kind: reason::PTO.to_string(),
        reason_ref_id: req.request_id,
        strength: ReservationStrength::Hard,
        notes: req.notes,
        created_by: req.created_by,
    };
    match calendar.reserve(cal_req).await {
        Ok(id) => (
            StatusCode::CREATED,
            Json(CreatePtoResponse { reservation_id: id }),
        )
            .into_response(),
        Err(CalendarClientError::Conflict { existing }) => (
            StatusCode::CONFLICT,
            Json(ConflictBody {
                error: "conflict",
                existing,
            }),
        )
            .into_response(),
        Err(CalendarClientError::Invalid(msg)) => (StatusCode::BAD_REQUEST, msg).into_response(),
        Err(CalendarClientError::Unreachable(msg)) => (
            StatusCode::BAD_GATEWAY,
            format!("calendar unreachable: {msg}"),
        )
            .into_response(),
        Err(other) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("calendar error: {other}"),
        )
            .into_response(),
    }
}

// boss-people doesn't redefine a CalendarClient test fake — the
// boss-calendar-client crate's `FakeCalendarClient` is the canonical
// one; boss-people just constructs it in its tests.

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_calendar_client::FakeCalendarClient;
    use chrono::TimeZone;
    use tower::ServiceExt;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 27, h, 0, 0).unwrap()
    }

    fn app(calendar: Option<Arc<dyn CalendarClient>>) -> Router {
        pto_router(PtoApiState { calendar })
    }

    fn req_body(emp: &str, start_h: u32, end_h: u32) -> serde_json::Value {
        serde_json::json!({
            "employee_id": emp,
            "start": t(start_h),
            "end": t(end_h),
            "notes": "family vacation",
            "created_by": "hr-test",
            "request_id": "pto-test-1",
        })
    }

    #[tokio::test]
    async fn create_pto_returns_201_with_reservation_id() {
        let cal: Arc<dyn CalendarClient> = Arc::new(FakeCalendarClient::new());
        let resp = app(Some(cal))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people/pto")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body("emp-1", 9, 17).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json.get("reservation_id").is_some(),
            "expected reservation_id in body, got {json}"
        );
    }

    #[tokio::test]
    async fn create_pto_forwards_calendar_conflict_as_409_with_existing() {
        let fake = Arc::new(FakeCalendarClient::new());
        // Stage a conflict — calendar will reject the next reserve.
        fake.stage_conflict(vec![]);
        let cal: Arc<dyn CalendarClient> = fake;
        let resp = app(Some(cal))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people/pto")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body("emp-1", 9, 17).to_string()))
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
        assert!(json["existing"].is_array());
    }

    #[tokio::test]
    async fn create_pto_returns_503_when_calendar_unconfigured() {
        let resp = app(None)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people/pto")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body("emp-1", 9, 17).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn create_pto_rejects_zero_duration_window() {
        let cal: Arc<dyn CalendarClient> = Arc::new(FakeCalendarClient::new());
        let resp = app(Some(cal))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people/pto")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body("emp-1", 9, 9).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_pto_translates_unreachable_to_502() {
        let fake = Arc::new(FakeCalendarClient::new());
        fake.stage_unreachable();
        let cal: Arc<dyn CalendarClient> = fake;
        let resp = app(Some(cal))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people/pto")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body("emp-1", 9, 17).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn create_pto_passes_employee_id_to_calendar() {
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let _ = app(Some(cal))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people/pto")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body("emp-cto", 9, 17).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            boss_calendar_client::FakeCall::Reserve(req) => {
                assert_eq!(req.subject.kind, "employee");
                assert_eq!(req.subject.id, "emp-cto");
                assert_eq!(req.reason_kind, reason::PTO);
                assert_eq!(req.reason_ref_id, "pto-test-1");
                assert_eq!(req.strength, ReservationStrength::Hard);
            }
            other => panic!("expected Reserve, got {other:?}"),
        }
    }
}
