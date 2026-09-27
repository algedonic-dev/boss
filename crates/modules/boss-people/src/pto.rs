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
use boss_policy_client::writes::{recorded_author, require_reaching};
use boss_policy_client::{Action, CurrentUser, PolicyClient, Resource};

/// PTO API state — wraps the calendar client. `Option` reflects
/// the config being optional; if the calendar isn't configured the
/// endpoint returns 503 rather than panicking on a missing client.
#[derive(Clone)]
pub struct PtoApiState {
    pub calendar: Option<Arc<dyn CalendarClient>>,
    /// Gates the booking: Create on `schedule`, in a scope that covers
    /// the employee (backlog dda8fd97). `None` only in tests, where
    /// the gate allows.
    pub policy: Option<Arc<dyn PolicyClient>>,
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
    /// Who is submitting this. The signed caller is, and it is what
    /// lands in `created_by`: absent is the caller, and a body naming
    /// anyone else is refused (backlog 11721a25 — it was free text, so
    /// the reservation recorded whatever author the caller typed).
    #[serde(default)]
    pub created_by: Option<String>,
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

/// Booking PTO is Create on `schedule`, in a scope that covers the
/// employee (backlog dda8fd97, 2026-09-27: this route took no caller,
/// so a request with no identity, a header naming the visitor role, or
/// any employee's session put a hard reservation on anyone's
/// calendar). The calendar sees only APPROVED PTO
/// (docs/architecture-decisions.md §Calendar), so booking one's own
/// time is not implied: an install offering self-service grants
/// `schedule` Create at `self`. A department grant is refused — the
/// request carries an employee id and no department, so it cannot be
/// shown to fall inside one; the schedule reads answer a department
/// grant the same way (a621d091). Asked before the calendar check, so
/// a refused caller learns nothing about how the install is wired.
async fn create_pto(
    State(state): State<PtoApiState>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<CreatePtoRequest>,
) -> Response {
    // Whether the grant reaches this one employee has ONE answer in the
    // tree, `writes::require_reaching`, which the calendar and the
    // scheduling writes ask too (CLAUDE.md §9a); it was a second
    // definition here for a day (dda8fd97, 11721a25). The calendar now
    // trusts this service's signed calls, so this gate is the only one
    // PTO passes — and it comes first, before the author check below, so
    // a refused caller learns nothing from that either.
    if let Some(policy) = state.policy.as_ref()
        && let Err(refused) = require_reaching(
            policy.as_ref(),
            &user,
            Action::Create,
            Resource::schedule(),
            Some(&req.employee_id),
        )
        .await
    {
        return refused;
    }
    let created_by = match recorded_author(&user, req.created_by.as_deref()) {
        Ok(author) => author,
        Err(refused) => return refused.into_response(),
    };
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
        created_by,
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
        pto_router(PtoApiState {
            calendar,
            policy: None,
        })
    }

    fn req_body(emp: &str, start_h: u32, end_h: u32) -> serde_json::Value {
        serde_json::json!({
            "employee_id": emp,
            "start": t(start_h),
            "end": t(end_h),
            "notes": "family vacation",
            "request_id": "pto-test-1",
        })
    }

    /// POST `body` as `user`, answering the status and the calls the
    /// calendar saw.
    async fn book_as(
        user: &str,
        body: serde_json::Value,
    ) -> (StatusCode, Vec<boss_calendar_client::FakeCall>) {
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let resp = app(Some(cal))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people/pto")
                    .header("content-type", "application/json")
                    .header("x-boss-user", user)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        (resp.status(), fake.calls())
    }

    /// Backlog 11721a25 (2026-09-27): `created_by` was body text, so the
    /// reservation recorded whatever author the caller typed. It is the
    /// signed caller now.
    #[tokio::test]
    async fn the_reservation_is_authored_by_the_signed_caller() {
        let hr = serde_json::json!({"id": "emp-hr", "role": "hr-generalist"}).to_string();
        let (status, calls) = book_as(&hr, req_body("emp-cto", 9, 17)).await;
        assert_eq!(status, StatusCode::CREATED);
        match &calls[..] {
            [boss_calendar_client::FakeCall::Reserve(req)] => {
                assert_eq!(req.created_by, "emp-hr")
            }
            other => panic!("expected one Reserve, got {other:?}"),
        }
    }

    /// The policy gate answers before the author check: a caller with no
    /// grant, whose body also names someone else, gets the grant refusal
    /// and learns nothing from the author check (the review of this
    /// merge, 2026-09-27).
    #[tokio::test]
    async fn the_grant_is_asked_before_the_author() {
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let app = pto_router(PtoApiState {
            calendar: Some(cal),
            policy: Some(defaults()),
        });
        let mut body = req_body("emp-cto", 9, 17);
        body["created_by"] = "emp-ceo".into();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people/pto")
                    .header("content-type", "application/json")
                    .header("x-boss-user", caller("emp-1", "staff"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("author"),
            "the author check answered: {text}"
        );
        assert!(fake.calls().is_empty());
    }

    /// A body naming someone else as the author is refused before the
    /// calendar is asked; naming the caller is the caller.
    #[tokio::test]
    async fn a_body_naming_another_author_is_refused() {
        let hr = serde_json::json!({"id": "emp-hr", "role": "hr-generalist"}).to_string();
        let mut forged = req_body("emp-cto", 9, 17);
        forged["created_by"] = "emp-ceo".into();
        let (status, calls) = book_as(&hr, forged).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(calls.is_empty(), "{calls:?}");
        let mut own = req_body("emp-cto", 9, 17);
        own["created_by"] = "emp-hr".into();
        assert_eq!(book_as(&hr, own).await.0, StatusCode::CREATED);
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

    // ---- The Create gate (backlog dda8fd97) -------------------------

    use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};

    /// The core default rules, as the live policy service holds them.
    fn defaults() -> Arc<dyn PolicyClient> {
        Arc::new(
            boss_policy_client::defaults::default_rules()
                .into_iter()
                .fold(FakePolicyClient::builder(), |b, r| {
                    b.allow(r.role, r.action, r.resource, r.scope)
                })
                .build(),
        )
    }

    /// POST a PTO request for `emp` as `user` (`None`: no identity at
    /// all), answering the status and every call the calendar saw.
    async fn book(
        policy: Arc<dyn PolicyClient>,
        user: Option<&str>,
        emp: &str,
    ) -> (StatusCode, Vec<boss_calendar_client::FakeCall>) {
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let app = pto_router(PtoApiState {
            calendar: Some(cal),
            policy: Some(policy),
        });
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/people/pto")
            .header("content-type", "application/json");
        if let Some(user) = user {
            req = req.header("x-boss-user", user);
        }
        let resp = app
            .oneshot(
                req.body(Body::from(req_body(emp, 9, 17).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        (resp.status(), fake.calls())
    }

    fn caller(id: &str, role: &str) -> String {
        serde_json::json!({ "id": id, "role": role }).to_string()
    }

    /// Measured on origin/main 2026-09-27: this route took no caller,
    /// so a request with no identity (the LAN machine door), a header
    /// naming the visitor or auditor role, and any employee's session
    /// each booked a hard reservation on ANY employee's calendar. On
    /// the core default rules none of them holds Create on `schedule`,
    /// and the calendar is never asked. An employee booking their OWN
    /// time is refused too: the calendar sees only approved PTO
    /// (docs/architecture-decisions.md §Calendar), so self-service is a
    /// grant the install makes, not a default.
    #[tokio::test]
    async fn pto_is_refused_to_every_caller_without_a_create_grant() {
        for (who, user) in [
            ("no identity", None),
            ("visitor", Some(caller("guest@algedonic.dev", "visitor"))),
            (
                "audit-readonly",
                Some(caller("guest@algedonic.dev", "audit-readonly")),
            ),
            (
                "an employee, for someone else",
                Some(caller("emp-1", "staff")),
            ),
            (
                "an employee, for themself",
                Some(caller("emp-cto", "staff")),
            ),
        ] {
            let (status, calls) = book(defaults(), user.as_deref(), "emp-cto").await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who}");
            assert!(calls.is_empty(), "{who} reached the calendar: {calls:?}");
        }
    }

    /// The deploy superuser books PTO on the core default rules — it
    /// holds every action on `schedule` that it holds on a shipped
    /// resource, so the route is not dead on an install that has not
    /// written its own HR grants yet.
    #[tokio::test]
    async fn the_deploy_superuser_books_pto_on_the_default_rules() {
        let (status, calls) = book(
            defaults(),
            Some(&caller("emp-bootstrap-admin", "platform-admin")),
            "emp-cto",
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(calls.len(), 1);
    }

    /// A `self` grant is how an install offers self-service PTO: the
    /// employee books their own time and nobody else's.
    #[tokio::test]
    async fn a_self_grant_books_only_the_callers_own_time() {
        let policy: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow("staff", Action::Create, Resource::schedule(), Scope::Self_)
                .build(),
        );
        let me = caller("emp-cto", "staff");
        let (own, _) = book(policy.clone(), Some(&me), "emp-cto").await;
        assert_eq!(own, StatusCode::CREATED);
        let (other, calls) = book(policy, Some(&me), "emp-1").await;
        assert_eq!(other, StatusCode::FORBIDDEN);
        assert!(calls.is_empty(), "{calls:?}");
    }

    /// A `team` grant books its holder's and their direct reports'
    /// time, and nobody else's.
    #[tokio::test]
    async fn a_team_grant_books_its_teams_time_only() {
        let policy: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow("manager", Action::Create, Resource::schedule(), Scope::Team)
                .build(),
        );
        let mgr = serde_json::json!({
            "id": "emp-mgr",
            "role": "manager",
            "direct_report_ids": ["emp-1"],
        })
        .to_string();
        for (emp, expected) in [
            ("emp-1", StatusCode::CREATED),
            ("emp-mgr", StatusCode::CREATED),
            ("emp-2", StatusCode::FORBIDDEN),
        ] {
            let (status, calls) = book(policy.clone(), Some(&mgr), emp).await;
            assert_eq!(status, expected, "{emp}");
            assert_eq!(calls.len(), usize::from(expected == StatusCode::CREATED));
        }
    }

    /// A department grant fails closed: the request names only an
    /// employee id, not the employee's department, so it cannot be
    /// shown to be inside the grant — the same answer the schedule
    /// reads give a department grant (a621d091).
    #[tokio::test]
    async fn a_department_grant_cannot_place_the_request_and_refuses() {
        let policy: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow(
                    "hr-generalist",
                    Action::Create,
                    Resource::schedule(),
                    Scope::Department("hr".into()),
                )
                .build(),
        );
        let (status, calls) =
            book(policy, Some(&caller("emp-hr", "hr-generalist")), "emp-cto").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(calls.is_empty(), "{calls:?}");
    }
}
