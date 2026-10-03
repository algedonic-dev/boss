//! Axum HTTP API for the calendar service.
//!
//! Routes:
//! - `POST /api/calendar/reservations` — create one. Returns 201
//!   with `{ "id": "<uuid>" }` on success, 409 with the existing
//!   conflicting rows on a hard-overlap collision, 400 on a
//!   malformed window. Create on `schedule`, reaching the subject.
//! - `GET  /api/calendar/reservations?resource_kind=...&resource_id=...&start=...&end=...`
//!   — list every active reservation on the given resource that
//!   intersects the window. RFC3339 datetimes; both `start` + `end`
//!   required.
//! - `DELETE /api/calendar/reservations/{id}[?actor=...]` — soft-cancel.
//!   Idempotent. 404 if the id doesn't exist. Delete on `schedule`,
//!   reaching the subject.
//! - `POST /api/calendar/cancel-by-reason` — cascade cancel by
//!   `(reason_kind, reason_ref_id)`. Body: `{kind, ref_id, actor?}`.
//!   Returns `{ "cancelled": <n> }`. Delete on `schedule` at `all`.
//!
//! Every write records the signed caller as its author (`created_by`,
//! `actor`): a body naming someone else is refused, except from a
//! sibling service writing for the caller it already authorised
//! (backlog 11721a25; `boss_policy_client::writes`).
//! - `POST /api/calendar/business-calendars/batch[?mode=insert-if-absent|take]`
//!   — publish
//!   business calendars, insert-if-absent by code (design e187198f: the
//!   instance is the truth; `take` replaces a held code wholesale).
//!   Body: `Vec<BusinessCalendar>`. Create on `business-calendar`, and
//!   Update too under `take` (backlog 59deda40). Returns
//!   `{ received, inserted, kept: [{id, differs}], updated: [{id, changes}], unchanged }`.
//!   Each code it inserts or replaces stages one fact signed by the
//!   caller (backlog 05f61acf).
//! - `GET  /api/calendar/business-calendars` — every business calendar
//!   with its closed-day set, sorted by code (the batch's own input
//!   shape: what `boss tenant export` writes the seed file from, backlog
//!   e618f3ac). Open read.
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
use boss_core::publish::ModeQuery;
use boss_policy_client::writes::{recorded_author, require_reaching, require_reaching_row};
use boss_policy_client::{CurrentUser, PolicyClient, controls};

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
    /// Gates every reservation write: Create on `schedule` to reserve,
    /// Delete to cancel, in a scope that reaches the subject (backlog
    /// 11721a25).
    pub policy: Arc<dyn PolicyClient>,
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
            "/api/calendar/business-calendars",
            get(list_business_calendars),
        )
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

/// The person a reservation's subject is, when it is one: a caller is
/// an employee id, so only an `employee` subject can be a caller's own
/// or a report's. Any other subject (a room, an asset) is nobody's, and
/// only an `all` grant reaches it.
fn person_of(subject: &Subject) -> Option<&str> {
    (subject.kind == "employee").then_some(subject.id.as_str())
}

/// Reserving time is Create on `schedule`, in a scope that reaches the
/// subject, and the reservation's `created_by` is the signed caller
/// (backlog 11721a25, 2026-09-27: this door took no caller, so anyone
/// reaching the port — or a signed-in session off the read-only floor —
/// could put a hard reservation of any reason_kind on any employee's
/// calendar, under any author it typed). A sibling service signs as
/// itself and names the person it reserves for
/// (`ReqwestCalendarClient::signed_as`).
async fn create_reservation(
    State(state): State<CalendarApiState>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<ReservationRequest>,
) -> Response {
    if let Err(refused) = require_reaching(
        state.policy.as_ref(),
        &user,
        controls::CREATE_SCHEDULE,
        person_of(&req.subject),
    )
    .await
    {
        return refused;
    }
    let created_by = match recorded_author(&user, Some(&req.created_by)) {
        Ok(author) => author,
        Err(refused) => return refused.into_response(),
    };
    let req = ReservationRequest { created_by, ..req };
    // OUTBOX (phase 2): the adapter records the reserved event (full
    // post-INSERT row state) inside the domain transaction; nothing
    // publishes post-commit and no fetch-back read is needed. The
    // stamp mints wall time; the row binds the same instant so live,
    // payload, and replay agree.
    let stamp = event_stamp(&state).await;
    match state
        .calendar
        .reserve_at(req, stamp.timestamp, &stamp)
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => calendar_error_response(e),
    }
}

/// Resolve the outbox event stamp for this request. The write handlers
/// read `CurrentUser` to gate and to name the row's author; the event's
/// actor is the same signed caller, which the publisher's
/// `default_actor` resolves from the task-local
/// context (else `automation:calendar`), and its clock probe settles
/// `_simulated` — the same envelope the retired post-commit emits
/// carried (outbox phase 2).
async fn event_stamp(state: &CalendarApiState) -> boss_core::publisher::EventStamp {
    match &state.publisher {
        Some(p) => p.stamp_with_actor(p.default_actor()).await,
        None => boss_core::publisher::EventStamp::new(
            "calendar",
            boss_core::actor::ActorId::Automation("calendar".into()),
        ),
    }
}

/// The outbox stamp signed as `actor` — the caller a door's policy
/// ladder resolved, never a fallback (the classes doors' shape). With a
/// publisher, its clock probe still settles `_simulated`.
async fn stamp_as(
    state: &CalendarApiState,
    actor: boss_core::actor::ActorId,
) -> boss_core::publisher::EventStamp {
    match &state.publisher {
        Some(p) => p.stamp_with_actor(actor).await,
        None => boss_core::publisher::EventStamp::new("calendar", actor),
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
    /// Who cancels — recorded in the cancellation log. Absent is the
    /// signed caller; naming anyone else is a sibling's alone (backlog
    /// 11721a25 — it defaulted to "unknown" and took any text).
    #[serde(default)]
    actor: Option<String>,
}

/// Cancelling is Delete on `schedule`, in a scope that reaches the
/// reservation's subject (backlog 11721a25: this door took no caller
/// either). The caller named an id, not a person, so a refusal is the
/// one by-id answer (`require_reaching_row`): it names nobody, and a
/// reservation outside the grant and an id with no reservation get the
/// same words. Only an `all` grant goes on to learn that an id does not
/// exist, from the cancel's own 404.
async fn cancel_reservation(
    State(state): State<CalendarApiState>,
    CurrentUser(user): CurrentUser,
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
    let subject = match state.calendar.get(res_id).await {
        Ok(row) => row.map(|r| r.subject),
        Err(e) => return calendar_error_response(e),
    };
    if let Err(refused) = require_reaching_row(
        state.policy.as_ref(),
        &user,
        controls::DELETE_SCHEDULE,
        subject.as_ref().and_then(person_of),
    )
    .await
    {
        return refused;
    }
    let actor = match recorded_author(&user, q.actor.as_deref()) {
        Ok(actor) => actor,
        Err(refused) => return refused.into_response(),
    };
    // OUTBOX (phase 2): the adapter records the cancelled event (full
    // post-cancel row state) with the flip — and only on an actual
    // flip, so a repeat cancel no longer publishes a duplicate event.
    let stamp = event_stamp(&state).await;
    match state
        .calendar
        .cancel_at(res_id, &actor, stamp.timestamp, &stamp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => calendar_error_response(e),
    }
}

#[derive(Deserialize)]
struct CancelByReasonBody {
    kind: String,
    ref_id: String,
    /// As [`CancelQuery::actor`].
    #[serde(default)]
    actor: Option<String>,
}

/// A cascade cancel is Delete on `schedule` at `all`: it flips every
/// reservation with the reason, on whichever subjects hold them, so no
/// narrower grant can be shown to reach them before it runs.
async fn cancel_by_reason(
    State(state): State<CalendarApiState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CancelByReasonBody>,
) -> Response {
    if let Err(refused) = require_reaching(
        state.policy.as_ref(),
        &user,
        controls::DELETE_SCHEDULE,
        None,
    )
    .await
    {
        return refused;
    }
    let actor = match recorded_author(&user, body.actor.as_deref()) {
        Ok(actor) => actor,
        Err(refused) => return refused.into_response(),
    };
    // OUTBOX (phase 2): the adapter enumerates the flipped rows via
    // UPDATE ... RETURNING and records one cancelled event per row in
    // the SAME transaction — the racy list-then-emit snapshot this
    // handler used to take around the UPDATE is gone.
    let stamp = event_stamp(&state).await;
    match state
        .calendar
        .cancel_by_reason_at(&body.kind, &body.ref_id, &actor, stamp.timestamp, &stamp)
        .await
    {
        Ok(n) => Json(serde_json::json!({ "cancelled": n })).into_response(),
        Err(e) => calendar_error_response(e),
    }
}

/// Batch-upsert business calendars — the seed surface, used to load
/// the `us-banking` / `us-tax` reference calendars from JSON instead of
/// `psql -f`. Each calendar is upserted by `code`; its closed-day set is
/// replaced wholesale.
///
/// Asks policy (backlog 59deda40, 2026-09-28): Create on
/// `business-calendar` to declare, and Update as well under `?mode=take`,
/// which overwrites a held code. Until then the door checked the
/// caller's access tier (`Operator`, or a sim caller on a sim instance)
/// and never asked policy, so no rule could widen or narrow who edits
/// the calendars the dispatcher's timing triggers resolve business days
/// from. Through the registry-write ladder
/// ([`boss_policy_client::writes::require_registry_write`], the classes
/// doors' since 553cf479): no caller 401, a deny or a grant narrower
/// than `all` 403, a policy service that cannot answer 503. Both grants
/// are platform-admin's in the core defaults — what `boss tenant
/// publish` and a tenant engine's prepare sign as — and the sim is admitted by
/// the binary's `SimBypassPolicyClient::from_env` (85e7f10f). Reads stay
/// open.
///
/// Who did it is on the record (backlog 05f61acf, 2026-09-28): the door
/// threw away the actor the ladder resolved, and the publish staged no
/// event, so a take that replaced a held calendar's closed-day set left
/// no row, event or log line naming its caller. The stamp now carries
/// that actor, and each code the batch changes stages one
/// `business-calendar.declared` / `.updated` fact in the write's own
/// transaction ([`crate::port::published_fact`]).
async fn batch_business_calendars(
    State(state): State<CalendarApiState>,
    CurrentUser(user): CurrentUser,
    Query(ModeQuery { mode }): Query<ModeQuery>,
    Json(calendars): Json<Vec<BusinessCalendar>>,
) -> Response {
    let ask = |control| {
        boss_policy_client::writes::require_registry_write(state.policy.as_ref(), &user, control)
    };
    // The actor the ladder resolves is the one every fact this batch
    // stages is signed with (backlog 05f61acf: it was discarded here).
    let actor = match ask(controls::CREATE_BUSINESS_CALENDAR).await {
        Ok(actor) => actor,
        Err(refusal) => return refusal,
    };
    if mode.is_take()
        && let Err(refusal) = ask(controls::UPDATE_BUSINESS_CALENDAR).await
    {
        return refusal;
    }
    let stamp = stamp_as(&state, actor).await;

    match state
        .calendar
        .publish_business_calendars(&calendars, mode, &stamp)
        .await
    {
        Ok(out) => Json(out).into_response(),
        Err(e) => calendar_error_response(e),
    }
}

/// Every business calendar, sorted by code, each with its closed-day
/// set. Open read, like the per-code GET.
async fn list_business_calendars(State(state): State<CalendarApiState>) -> Response {
    match state.calendar.list_business_calendars().await {
        Ok(rows) => Json(rows).into_response(),
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
    use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
    use chrono::TimeZone;
    use tower::ServiceExt;

    use crate::in_memory::InMemoryCalendar;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 27, h, 0, 0).unwrap()
    }

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

    fn app() -> Router {
        app_with(defaults())
    }

    fn app_with(policy: Arc<dyn PolicyClient>) -> Router {
        app_over(policy).0
    }

    /// [`app_with`], and the store behind it — so a test can read the
    /// facts a request did (or did not) record.
    fn app_over(policy: Arc<dyn PolicyClient>) -> (Router, Arc<InMemoryCalendar>) {
        let cal = Arc::new(InMemoryCalendar::new());
        let app = router(CalendarApiState {
            calendar: cal.clone(),
            publisher: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
            policy,
        });
        (app, cal)
    }

    /// `x-boss-user` for `id` at `role`.
    fn caller(id: &str, role: &str) -> String {
        serde_json::json!({ "id": id, "role": role }).to_string()
    }

    /// What `ReqwestCalendarClient::signed_as` sends: a sibling service
    /// at the deploy superuser's role.
    fn sibling() -> String {
        caller("automation:jobs", "platform-admin")
    }

    fn reservation(subject_kind: &str, id: &str, h: u32, created_by: &str) -> serde_json::Value {
        serde_json::json!({
            "subject": {"subject_kind": subject_kind, "id": id},
            "window": {"start": t(h), "end": t(h + 1)},
            "reason_kind": "pto",
            "reason_ref_id": format!("pto-{id}-{h}"),
            "strength": "hard",
            "created_by": created_by,
        })
    }

    /// POST a reservation as `user` (`None`: no identity header).
    async fn reserve(app: &Router, user: Option<&str>, body: &serde_json::Value) -> Response {
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/calendar/reservations")
            .header("content-type", "application/json");
        if let Some(user) = user {
            req = req.header("x-boss-user", user);
        }
        app.clone()
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap()
    }

    /// Every active reservation on `emp` across the test day.
    async fn held(app: &Router, emp: &str) -> Vec<serde_json::Value> {
        let start = t(0).to_rfc3339().replace('+', "%2B");
        let end = t(23).to_rfc3339().replace('+', "%2B");
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/calendar/reservations?resource_kind=employee&resource_id={emp}&start={start}&end={end}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn send(app: &Router, req: Request<Body>) -> StatusCode {
        app.clone().oneshot(req).await.unwrap().status()
    }

    fn cancel(id: &str, user: Option<&str>, actor: Option<&str>) -> Request<Body> {
        let uri = match actor {
            Some(a) => format!("/api/calendar/reservations/{id}?actor={a}"),
            None => format!("/api/calendar/reservations/{id}"),
        };
        let mut req = Request::builder().method("DELETE").uri(uri);
        if let Some(user) = user {
            req = req.header("x-boss-user", user);
        }
        req.body(Body::empty()).unwrap()
    }

    fn cancel_by_reason(user: Option<&str>, ref_id: &str, actor: &str) -> Request<Body> {
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/calendar/cancel-by-reason")
            .header("content-type", "application/json");
        if let Some(user) = user {
            req = req.header("x-boss-user", user);
        }
        req.body(Body::from(
            serde_json::json!({"kind": "pto", "ref_id": ref_id, "actor": actor}).to_string(),
        ))
        .unwrap()
    }

    // ---- The write gate (backlog 11721a25) ---------------------------

    /// Measured on origin/main 2026-09-27: `POST /api/calendar/
    /// reservations` took no caller, so a request with no identity (the
    /// LAN machine door), the visitor and auditor sessions, and any
    /// employee put a hard reservation — any reason_kind — on anyone's
    /// calendar. On the core default rules none of them holds Create on
    /// `schedule`, and nothing is held.
    #[tokio::test]
    async fn a_reservation_is_refused_to_every_caller_without_a_create_grant() {
        let app = app();
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
            let resp = reserve(
                &app,
                user.as_deref(),
                &reservation("employee", "emp-cto", 9, "emp-cto"),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{who}");
        }
        assert!(held(&app, "emp-cto").await.is_empty());
    }

    /// The deploy superuser and a sibling service signing as it both
    /// reserve on the default rules; the sibling's `created_by` names
    /// the person it reserves for, the superuser's is itself.
    #[tokio::test]
    async fn the_deploy_superuser_and_a_sibling_reserve_on_the_default_rules() {
        let app = app();
        let admin = caller("emp-david", "platform-admin");
        let resp = reserve(
            &app,
            Some(&admin),
            &reservation("employee", "emp-cto", 9, ""),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let resp = reserve(
            &app,
            Some(&sibling()),
            &reservation("employee", "emp-cto", 11, "emp-starter"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let mut authors: Vec<String> = held(&app, "emp-cto")
            .await
            .iter()
            .map(|r| r["created_by"].as_str().unwrap().to_string())
            .collect();
        authors.sort();
        assert_eq!(authors, ["emp-david", "emp-starter"]);
    }

    /// Status and body text of `req`.
    async fn answer(app: &Router, req: Request<Body>) -> (StatusCode, String) {
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    /// The adversarial review of this car (2026-09-27) reproduced it: a
    /// cancel by id refused with the row's employee in the message, and
    /// with different words for an id that does not exist, so a caller
    /// with a narrow grant learned whose a reservation was and which ids
    /// are real. A by-id refusal is one answer, naming nobody.
    #[tokio::test]
    async fn a_cancel_refusal_says_neither_whose_row_nor_whether_it_exists() {
        let policy: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow(
                    "platform-admin",
                    Action::Create,
                    Resource::schedule(),
                    Scope::All,
                )
                .allow("staff", Action::Delete, Resource::schedule(), Scope::Self_)
                .build(),
        );
        let app = app_with(policy);
        let resp = reserve(
            &app,
            Some(&sibling()),
            &reservation("employee", "emp-ceo", 9, "emp-ceo"),
        )
        .await;
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let id = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let me = caller("emp-1", "staff");
        let existing = answer(&app, cancel(&id, Some(&me), None)).await;
        let missing = answer(
            &app,
            cancel("6f1e9b99-0000-4000-8000-00000000dead", Some(&me), None),
        )
        .await;
        assert_eq!(existing.0, StatusCode::FORBIDDEN);
        assert!(
            !existing.1.contains("emp-ceo"),
            "names the row's employee: {existing:?}"
        );
        assert_eq!(existing, missing, "tells a real id from a missing one");
    }

    /// A policy service that cannot be asked.
    struct Dark;
    #[async_trait::async_trait]
    impl PolicyClient for Dark {
        async fn check(
            &self,
            _: &boss_policy_client::User,
            _: Action,
            _: Resource,
        ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
            Err(boss_policy_client::PolicyClientError::Unreachable(
                "dark".into(),
            ))
        }
        async fn scope_predicate(
            &self,
            _: &boss_policy_client::User,
            _: Resource,
        ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
            Err(boss_policy_client::PolicyClientError::Unreachable(
                "dark".into(),
            ))
        }
    }

    /// A policy service that cannot be asked is an outage, not a
    /// permission fact: the write answers 503 and holds nothing.
    #[tokio::test]
    async fn a_policy_outage_refuses_the_write_with_503() {
        let app = app_with(Arc::new(Dark));
        let admin = caller("emp-david", "platform-admin");
        let resp = reserve(
            &app,
            Some(&admin),
            &reservation("employee", "emp-cto", 9, ""),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(held(&app, "emp-cto").await.is_empty());
    }

    /// The example tenant's data seed (`ensure_reservations`) books its
    /// scripted week — PTO among it — straight on this door, not through
    /// `/api/people/pto`: an automation identity at the deploy
    /// superuser's role and operator tier, soft strength, naming
    /// `system-seed` as the author. That shape, copied from the seed,
    /// still reserves on the default rules.
    #[tokio::test]
    async fn the_tenant_data_seeds_reservation_still_lands() {
        let app = app();
        let seed = serde_json::json!({
            "id": "automation:tenant-seed",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "platform",
        })
        .to_string();
        let mut body = reservation("employee", "emp-coo", 13, "system-seed");
        body["strength"] = "soft".into();
        body["notes"] = "Friday PTO".into();
        let resp = reserve(&app, Some(&seed), &body).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let rows = held(&app, "emp-coo").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["created_by"], "system-seed");
    }

    /// `created_by` was body text, so the record said whatever the
    /// caller typed. A caller records itself: naming someone else is
    /// refused even with a grant that covers the reservation.
    #[tokio::test]
    async fn a_caller_naming_someone_else_as_author_is_refused() {
        let app = app();
        let admin = caller("emp-david", "platform-admin");
        let resp = reserve(
            &app,
            Some(&admin),
            &reservation("employee", "emp-cto", 9, "emp-ceo"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(held(&app, "emp-cto").await.is_empty());
    }

    /// A `self` grant is how an install offers self-service: the caller
    /// reserves their own time and nobody else's — and a subject that is
    /// not an employee is nobody's own.
    #[tokio::test]
    async fn a_self_grant_reserves_only_the_callers_own_time() {
        let policy: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow("staff", Action::Create, Resource::schedule(), Scope::Self_)
                .build(),
        );
        let app = app_with(policy);
        let me = caller("emp-cto", "staff");
        let own = reserve(&app, Some(&me), &reservation("employee", "emp-cto", 9, "")).await;
        assert_eq!(own.status(), StatusCode::CREATED);
        let other = reserve(&app, Some(&me), &reservation("employee", "emp-1", 9, "")).await;
        assert_eq!(other.status(), StatusCode::FORBIDDEN);
        let room = reserve(&app, Some(&me), &reservation("location", "emp-cto", 12, "")).await;
        assert_eq!(room.status(), StatusCode::FORBIDDEN);
    }

    /// A cancel is Delete on `schedule`: by id or by reason, a caller
    /// without the grant is refused and the reservation stands; the
    /// `actor` it records is the signed caller, as a reservation's
    /// author is.
    #[tokio::test]
    async fn a_cancel_is_refused_without_a_delete_grant_and_records_its_caller() {
        let app = app();
        let resp = reserve(
            &app,
            Some(&sibling()),
            &reservation("employee", "emp-cto", 9, "emp-cto"),
        )
        .await;
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let id = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        let staff = caller("emp-cto", "staff");
        for user in [None, Some(staff.as_str())] {
            assert_eq!(
                send(&app, cancel(&id, user, None)).await,
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                send(&app, cancel_by_reason(user, "pto-emp-cto-9", "emp-cto")).await,
                StatusCode::FORBIDDEN
            );
        }
        let admin = caller("emp-david", "platform-admin");
        assert_eq!(
            send(&app, cancel(&id, Some(&admin), Some("emp-ceo"))).await,
            StatusCode::FORBIDDEN,
            "an actor naming someone else"
        );
        assert_eq!(held(&app, "emp-cto").await.len(), 1, "still held");

        assert_eq!(
            send(&app, cancel(&id, Some(&admin), None)).await,
            StatusCode::NO_CONTENT
        );
        assert!(held(&app, "emp-cto").await.is_empty());
        assert_eq!(
            send(
                &app,
                cancel_by_reason(Some(&sibling()), "pto-emp-cto-9", "emp-cto")
            )
            .await,
            StatusCode::OK
        );
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
                    .header("x-boss-user", sibling())
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
                    .header("x-boss-user", sibling())
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
                    .header("x-boss-user", sibling())
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

    // --- business-calendar batch (policy-gated) + get round-trip ---

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

    /// `boss tenant export` writes the instance's calendars back into
    /// `seeds/business_calendars.json` (design e187198f car 3, backlog
    /// e618f3ac): until 2026-09-18 the only read was per code, so an
    /// export had no way to learn WHICH codes the instance held. The
    /// list is every calendar with its closed set, sorted by code —
    /// the batch's own input shape, so export and publish are inverses.
    #[tokio::test]
    async fn list_business_calendars_answers_every_code_sorted_in_the_batch_shape() {
        let app = app();
        let body = serde_json::json!([
            {"code": "us-tax", "name": "US Tax", "weekend": [5, 6], "closed": ["2026-04-15"]},
            {"code": "us-banking", "name": "US Banking", "weekend": [5, 6], "closed": ["2026-01-01"]},
        ]);
        let resp = app
            .clone()
            .oneshot(batch_request(Some(&operator_header()), body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/calendar/business-calendars")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let rows: Vec<BusinessCalendar> = serde_json::from_slice(&bytes).unwrap();
        let codes: Vec<&str> = rows.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(codes, ["us-banking", "us-tax"], "sorted by code");
        assert_eq!(
            rows[1]
                .closed
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>(),
            ["2026-04-15"],
            "each row carries its full closed set, as the per-code GET does"
        );
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
        assert_eq!(v["inserted"], serde_json::json!(1));

        let (status, cal) = get_calendar(&app, "us-banking").await;
        assert_eq!(status, StatusCode::OK);
        let cal = cal.unwrap();
        assert_eq!(cal["code"], "us-banking");
        assert_eq!(
            cal["closed"],
            serde_json::json!(["2026-01-01", "2026-07-03"])
        );
    }

    /// THE INSTANCE IS THE TRUTH (design e187198f, 2026-09-18): a
    /// re-seed of a held code KEEPS it by default and names the field
    /// that differs; only `?mode=take` replaces the closed set
    /// wholesale (no merge), naming the change from → to.
    #[tokio::test]
    async fn batch_keeps_a_held_code_by_default_and_take_replaces_the_closed_set_wholesale() {
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
        // v2 (same code), by default: kept, and the answer says on what.
        let r2 = app
            .clone()
            .oneshot(batch_request(
                Some(&operator_header()),
                one_calendar(&["2026-07-03", "2026-12-25"]),
            ))
            .await
            .unwrap();
        assert_eq!(r2.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(r2.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["inserted"], serde_json::json!(0));
        assert_eq!(v["kept"][0]["id"], "us-banking");
        assert_eq!(v["kept"][0]["differs"], serde_json::json!(["closed"]));
        assert_eq!(v["updated"], serde_json::json!([]));
        let (_, cal) = get_calendar(&app, "us-banking").await;
        assert_eq!(
            cal.unwrap()["closed"],
            serde_json::json!(["2026-01-01"]),
            "the instance's closed set is kept under the default"
        );

        // v2 under take: replaces, does NOT merge, and names the change.
        let r3 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/calendar/business-calendars/batch?mode=take")
                    .header("content-type", "application/json")
                    .header("x-boss-user", operator_header())
                    .body(Body::from(
                        one_calendar(&["2026-07-03", "2026-12-25"]).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r3.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(r3.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["kept"], serde_json::json!([]));
        assert_eq!(v["updated"][0]["id"], "us-banking");
        assert_eq!(v["updated"][0]["changes"][0]["field"], "closed");
        assert_eq!(
            v["updated"][0]["changes"][0]["from"],
            serde_json::json!(["2026-01-01"])
        );
        let (_, cal) = get_calendar(&app, "us-banking").await;
        assert_eq!(
            cal.unwrap()["closed"],
            serde_json::json!(["2026-07-03", "2026-12-25"]),
            "take replaces the closed set wholesale (no merge with v1)"
        );
    }

    // ---- the policy question (backlog 59deda40) -------------------------

    /// `x-boss-user` JSON for a caller of the given role and tier.
    fn signed(id: &str, role: &str, tier: &str) -> String {
        serde_json::json!({
            "id": id,
            "role": role,
            "access_tier": tier,
            "territory_account_ids": [],
            "direct_report_ids": [],
        })
        .to_string()
    }

    /// A `?mode=take` batch as `user`.
    fn take_request(user_header: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/api/calendar/business-calendars/batch?mode=take")
            .header("content-type", "application/json")
            .header("x-boss-user", user_header)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// A caller the tier check refused — USER tier, a role the core
    /// defaults grant nothing — declares a calendar once a policy rule
    /// grants it Create on `business-calendar`; overwriting a held code
    /// with `?mode=take` is Update as well, so a Create-only grant is
    /// refused it and the held closed set stays.
    #[tokio::test]
    async fn a_granting_rule_lets_a_non_admin_declare_and_take_needs_update_too() {
        let grant = |actions: &[Action]| -> Arc<dyn PolicyClient> {
            Arc::new(
                actions
                    .iter()
                    .fold(FakePolicyClient::builder().with_default_rules(), |b, a| {
                        b.allow(
                            "calendar-keeper",
                            *a,
                            Resource::business_calendar(),
                            Scope::All,
                        )
                    })
                    .build(),
            )
        };
        let keeper = signed("emp-keeper", "calendar-keeper", "user");
        let (create_only, cal) = app_over(grant(&[Action::Create]));
        let resp = create_only
            .clone()
            .oneshot(batch_request(Some(&keeper), one_calendar(&["2026-01-01"])))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = create_only
            .clone()
            .oneshot(take_request(&keeper, one_calendar(&["2026-12-25"])))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let (_, held) = get_calendar(&create_only, "us-banking").await;
        assert_eq!(held.unwrap()["closed"], serde_json::json!(["2026-01-01"]));
        assert_eq!(
            calendar_facts(&cal).len(),
            1,
            "the declaration's fact alone: the refused take records nothing"
        );

        let both = app_with(grant(&[Action::Create, Action::Update]));
        let resp = both
            .clone()
            .oneshot(take_request(&keeper, one_calendar(&["2026-12-25"])))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let (_, cal) = get_calendar(&both, "us-banking").await;
        assert_eq!(cal.unwrap()["closed"], serde_json::json!(["2026-12-25"]));
    }

    /// A platform-admin at operator tier — everything the tier check
    /// admitted — is refused 403 by a user override that denies it, with
    /// policy's reason, and no calendar lands.
    #[tokio::test]
    async fn a_denying_override_refuses_a_platform_admin() {
        let admin_id = "claude@algedonic.dev";
        let (app, cal) = app_over(Arc::new(
            FakePolicyClient::builder()
                .with_default_rules()
                .with_override(boss_policy_client::UserOverride {
                    id: "deny-calendars".into(),
                    user_id: admin_id.into(),
                    resource: Resource::business_calendar(),
                    action: Action::Create,
                    scope: Scope::None,
                    reason: "calendars frozen for the audit".into(),
                    expires_at: None,
                })
                .build(),
        ));
        let resp = app
            .clone()
            .oneshot(batch_request(
                Some(&signed(admin_id, "platform-admin", "operator")),
                one_calendar(&["2026-01-01"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("calendars frozen"));
        let (status, _) = get_calendar(&app, "us-banking").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(calendar_facts(&cal).is_empty(), "a refusal records nothing");
    }

    /// The rest of the ladder, and each refusal writes nothing: no
    /// identity is 401 — and so is a header claiming the anonymous id
    /// with a platform role, or a blank id — a role the defaults grant
    /// nothing is 403, a policy service that cannot answer is its own
    /// 503, never an allow.
    #[tokio::test]
    async fn the_batch_refuses_before_it_writes() {
        let dark: Arc<dyn PolicyClient> = Arc::new(Dark);
        let cases: [(Option<String>, Arc<dyn PolicyClient>, StatusCode); 5] = [
            (None, defaults(), StatusCode::UNAUTHORIZED),
            (
                Some(signed(
                    boss_policy_client::User::ANONYMOUS_ID,
                    "platform-admin",
                    "operator",
                )),
                defaults(),
                StatusCode::UNAUTHORIZED,
            ),
            (
                Some(signed("", "platform-admin", "operator")),
                defaults(),
                StatusCode::UNAUTHORIZED,
            ),
            (
                Some(signed("emp-audit", "audit-readonly", "operator")),
                defaults(),
                StatusCode::FORBIDDEN,
            ),
            (
                Some(operator_header()),
                dark,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];
        for (user, policy, want) in cases {
            let (app, cal) = app_over(policy);
            let resp = app
                .clone()
                .oneshot(batch_request(
                    user.as_deref(),
                    one_calendar(&["2026-01-01"]),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), want, "{user:?}");
            let (status, _) = get_calendar(&app, "us-banking").await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{user:?}");
            assert!(
                calendar_facts(&cal).is_empty(),
                "a refusal records nothing: {user:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_sim_chain_alone_is_not_a_caller() {
        // Backlog 85e7f10f (2026-09-25): a sim chain is not an identity.
        // Anonymous on a chain (the task-local set directly — the router
        // omits the middleware) is refused 401 and no calendar lands:
        // only a sim caller on a sim instance takes the bypass, which the
        // binary's `SimBypassPolicyClient::from_env` installs.
        let (app, cal) = app_over(defaults());
        let resp = boss_core::sim_origin::with_sim_chain(
            true,
            app.clone()
                .oneshot(batch_request(None, one_calendar(&["2026-01-01"]))),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let (status, _) = get_calendar(&app, "us-banking").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(calendar_facts(&cal).is_empty(), "a refusal records nothing");
    }

    // ---- who a publish is recorded as (backlog 05f61acf) ----------------

    /// Business-calendar facts the in-memory store recorded, as
    /// `(kind, payload)`.
    fn calendar_facts(cal: &InMemoryCalendar) -> Vec<(String, serde_json::Value)> {
        cal.recorded_events()
            .into_iter()
            .filter(|e| e.kind.starts_with("business-calendar."))
            .map(|e| (e.kind, e.payload))
            .collect()
    }

    /// The door signs the fact with the actor the policy ladder resolved
    /// (backlog 05f61acf, 2026-09-28: it asked policy and threw the
    /// actor away, and the publish staged no fact, so a `?mode=take`
    /// that replaced a held calendar's closed-day set left no row, event
    /// or log line naming who did it). A declaration records who
    /// declared it; a kept row records nothing, because nothing changed;
    /// a take records who took it and each change from → to; a take
    /// that restates the held row changes nothing and records nothing.
    #[tokio::test]
    async fn a_take_that_replaces_a_held_calendar_records_who_took_it() {
        let cal = Arc::new(InMemoryCalendar::new());
        let app = router(CalendarApiState {
            calendar: cal.clone(),
            publisher: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
            policy: Arc::new(
                FakePolicyClient::builder()
                    .with_default_rules()
                    .allow(
                        "calendar-keeper",
                        Action::Create,
                        Resource::business_calendar(),
                        Scope::All,
                    )
                    .allow(
                        "calendar-keeper",
                        Action::Update,
                        Resource::business_calendar(),
                        Scope::All,
                    )
                    .build(),
            ),
        });
        let declarer = signed("emp-declarer", "calendar-keeper", "user");
        let taker = signed("emp-taker", "calendar-keeper", "user");

        let resp = app
            .clone()
            .oneshot(batch_request(
                Some(&declarer),
                one_calendar(&["2026-01-01"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let facts = calendar_facts(&cal);
        assert_eq!(facts.len(), 1, "{facts:?}");
        let (kind, payload) = &facts[0];
        assert_eq!(kind, crate::events::BUSINESS_CALENDAR_DECLARED);
        assert_eq!(payload["code"], "us-banking");
        assert_eq!(payload["closed"], serde_json::json!(["2026-01-01"]));
        assert_eq!(payload["mode"], "insert-if-absent");
        assert_eq!(payload["declared_by"], "emp-declarer");
        assert_eq!(payload["_actor"], "emp-declarer");

        // Kept under the default: nothing written, nothing recorded.
        let resp = app
            .clone()
            .oneshot(batch_request(Some(&taker), one_calendar(&["2026-12-25"])))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(calendar_facts(&cal).len(), 1, "a kept row records nothing");

        // The take replaces the held closed set, and says who did it.
        let resp = app
            .clone()
            .oneshot(take_request(&taker, one_calendar(&["2026-12-25"])))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let facts = calendar_facts(&cal);
        assert_eq!(facts.len(), 2, "{facts:?}");
        let (kind, payload) = &facts[1];
        assert_eq!(kind, crate::events::BUSINESS_CALENDAR_UPDATED);
        assert_eq!(payload["code"], "us-banking");
        assert_eq!(payload["mode"], "take");
        assert_eq!(payload["closed"], serde_json::json!(["2026-12-25"]));
        assert_eq!(payload["changes"][0]["field"], "closed");
        assert_eq!(
            payload["changes"][0]["from"],
            serde_json::json!(["2026-01-01"])
        );
        assert_eq!(
            payload["changes"][0]["to"],
            serde_json::json!(["2026-12-25"])
        );
        assert_eq!(payload["updated_by"], "emp-taker");
        assert_eq!(payload["_actor"], "emp-taker");

        // The same take again restates the held row: no second fact.
        let resp = app
            .clone()
            .oneshot(take_request(&taker, one_calendar(&["2026-12-25"])))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            calendar_facts(&cal).len(),
            2,
            "an unchanged take records nothing"
        );
    }
}
