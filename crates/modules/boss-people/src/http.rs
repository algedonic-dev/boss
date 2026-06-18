//! Axum HTTP handlers for the people API.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use boss_core::publisher::DomainPublisher;
use boss_policy::{Action, Decision, Resource};
use boss_policy_client::{CurrentUser, PolicyClient};

use crate::port::{PeopleError, PeopleRepository};
use crate::types::Employee;

pub struct PeopleApiState<R: PeopleRepository> {
    pub people: Arc<R>,
    pub publisher: Option<DomainPublisher>,
    /// Row-level authorization. None in tests that don't exercise
    /// the policy path — those handlers skip the gate and allow the
    /// request, preserving the existing test surface.
    pub policy: Option<Arc<dyn PolicyClient>>,
    /// SubjectKind registry — opt-in validator for tenant-extensible
    /// Subject discriminators. Today the boss-people surface accepts
    /// closed Subject variants (Account, Employee, Vendor, ...) so
    /// the validator never fires; it lands here as scaffolding for
    /// the future boss-accounts carve-out + account_type lift, which
    /// will introduce Subject::Custom into account-shaped writes.
    /// See `boss-jobs::http::check_custom_subject` for the canonical
    /// shape this mirrors.
    pub subject_kinds: Option<Arc<dyn boss_subject_kinds_client::SubjectKindsClient>>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router<R: PeopleRepository + 'static>(state: PeopleApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/people/health", get(health))
        .route(
            "/api/people",
            get(list_employees::<R>).post(create_employee::<R>),
        )
        .route(
            "/api/people/{id}",
            get(get_employee::<R>)
                .put(update_employee::<R>)
                .delete(delete_employee::<R>),
        )
        .route("/api/people/{id}/reports", get(get_reports::<R>))
        .route("/api/people/{id}/exists", get(employee_exists::<R>))
        .with_state(shared)
}

/// SubjectKind validator. Mirrors
/// `boss_jobs::http::check_custom_subject`. Returns `Ok(())` when:
/// - `subject` is one of the closed core variants (always valid), OR
/// - `subject` is Custom and either the registry isn't configured
///   (opt-in) or the registry confirms the kind exists.
///
/// Returns a 400 Response when the registry says the kind doesn't
/// exist; 502 on registry-call failure rather than dropping the
/// write silently.
///
/// The boss-people HTTP surface only accepts closed Subject variants
/// (Account / Employee / Vendor / Campaign / etc.), so the Custom
/// branch is currently unreachable and the function is `dead_code`;
/// it's a one-line gate for any write path that starts accepting
/// Custom subjects.
#[allow(dead_code)]
async fn check_custom_subject(
    registry: Option<&Arc<dyn boss_subject_kinds_client::SubjectKindsClient>>,
    subject: &boss_core::job::Subject,
) -> Result<(), Response> {
    // The SubjectKind registry is the single source of truth for the
    // noun vocabulary; core enumerates no kinds. When wired, every kind
    // is validated against it; when unwired (tests) all pass.
    let Some(reg) = registry else {
        return Ok(());
    };
    let kind = subject.kind.as_str();
    match reg.subject_kind_exists(kind).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown subject kind `{kind}` — register it in the subject_kinds registry first",
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            format!("subject-kinds registry unreachable: {e}"),
        )
            .into_response()),
    }
}

/// Lightweight existence check used by cross-service write guards
/// (boss-assets's actor_id validation, etc). Returns `{"exists": bool}`
/// instead of the full employee record so the caller doesn't pay for
/// data it isn't going to use.
async fn employee_exists<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.people.employee_by_id(&id).await {
        Ok(opt) => Json(serde_json::json!({ "exists": opt.is_some() })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-people-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

#[derive(Deserialize)]
struct ListEmployeesQuery {
    /// Exact role-slug filter (e.g. `bookkeeper`, `head-brewer`).
    role: Option<String>,
    /// Exact status filter (e.g. `active`). Omit for all statuses.
    status: Option<String>,
}

/// List the roster, optionally filtered by `role` and/or `status`.
/// `?role=bookkeeper&status=active` powers the role→active-employees
/// lookup the dispatcher's notifier + auto-assign need, and the SPA
/// directory. Both filters are exact-match; absent = no constraint.
async fn list_employees<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Query(q): Query<ListEmployeesQuery>,
) -> Response {
    match state.people.all_employees().await {
        Ok(employees) => {
            let filtered: Vec<Employee> = employees
                .into_iter()
                .filter(|e| q.role.as_ref().is_none_or(|r| e.role.as_ref() == Some(r)))
                .filter(|e| {
                    q.status
                        .as_ref()
                        .is_none_or(|s| e.status.as_ref() == Some(s))
                })
                .collect();
            Json(filtered).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_employee<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.people.employee_by_id(&id).await {
        Ok(Some(emp)) => Json(emp).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("no employee with ID {id}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_reports<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.people.direct_reports(&id).await {
        Ok(reports) => Json(reports).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn create_employee<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Json(emp): Json<Employee>,
) -> Response {
    if let Err(msg) = validate_email(emp.email.as_deref()) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.people.create_employee_at(&emp, now).await {
        Ok(id) => {
            if let Some(pub_) = &state.publisher {
                // Full Employee row state — what the rebuilder consumes.
                pub_.emit_at(
                    crate::events::EMPLOYEE_CREATED,
                    serde_json::to_value(&emp).unwrap_or_default(),
                    now,
                )
                .await;
            }
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Err(e) => people_error_response(e),
    }
}

async fn update_employee<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(emp): Json<Employee>,
) -> Response {
    // Policy: editing an employee record requires Action::Update on
    // Resource::employee(). Test path (policy: None) bypasses the gate.
    if let Some(ref policy) = state.policy {
        match policy
            .check(&user, Action::Update, Resource::employee())
            .await
        {
            Ok(Decision::Allow { .. }) => {}
            Ok(Decision::Deny { reason }) => {
                return (StatusCode::FORBIDDEN, reason).into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("policy check failed: {e}"),
                )
                    .into_response();
            }
        }
    }
    if let Err(msg) = validate_email(emp.email.as_deref()) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.people.update_employee_at(&id, &emp, now).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    crate::events::EMPLOYEE_UPDATED,
                    serde_json::to_value(&emp).unwrap_or_default(),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => people_error_response(e),
    }
}

async fn delete_employee<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.people.delete_employee(&id).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    crate::events::EMPLOYEE_DELETED,
                    serde_json::json!({ "id": id, "deleted_at": now }),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => people_error_response(e),
    }
}

/// Cheap email validation. The OSS quickstart auth keys
/// credentials by email; the future Authelia / OIDC migration
/// will too. We don't try to be RFC 5322 — just non-empty + has
/// the shape `local@domain.tld`. Callers that want stricter
/// validation can layer it on top; this rejects the obvious
/// "bookkeeper" / "" / "no-email" footguns the existing seeds
/// surfaced.
fn validate_email(email: Option<&str>) -> Result<(), String> {
    // Identity-first: no email yet is fine (an id-only employee record).
    // Validate only what's provided.
    let Some(email) = email else {
        return Ok(());
    };
    let trimmed = email.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let (local, domain) = match trimmed.rsplit_once('@') {
        Some(parts) => parts,
        None => return Err("email must contain '@'".into()),
    };
    if local.is_empty() {
        return Err("email must have a local-part before '@'".into());
    }
    if !domain.contains('.') {
        return Err("email domain must contain '.'".into());
    }
    Ok(())
}

fn people_error_response(e: PeopleError) -> Response {
    match e {
        PeopleError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        PeopleError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        PeopleError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::in_memory::InMemoryPeople;
    use crate::types::*;

    fn test_emp(id: &str, manager: Option<&str>) -> Employee {
        Employee {
            id: id.to_string(),
            name: Some(format!("Test {id}")),
            email: Some(format!("{id}@boss.io")),
            role: Some("service-tech".to_string()),
            department: Some("service".to_string()),
            skill_level: Some(3),
            skills: vec![],
            hire_date: Some(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            location: Some("loc-hq".to_string()),
            manager_id: manager.map(String::from),
            employment_type: Some("full-time".to_string()),
            status: Some("active".to_string()),
            certifications: vec![],
            annual_salary_cents: None,
        }
    }

    fn test_app() -> Router {
        let people = Arc::new(InMemoryPeople::new(vec![
            test_emp("emp-001", None),
            test_emp("emp-002", Some("emp-001")),
            test_emp("emp-003", Some("emp-001")),
        ]));
        let policy: Arc<dyn PolicyClient> = Arc::new(boss_policy_client::PermissivePolicyClient);
        router(PeopleApiState {
            people,
            publisher: None,
            policy: Some(policy),
            subject_kinds: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    #[tokio::test]
    async fn health_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/people/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_all() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/people")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let emps: Vec<Employee> = serde_json::from_slice(&body).unwrap();
        assert_eq!(emps.len(), 3);
    }

    #[tokio::test]
    async fn list_filtered_by_role_and_status() {
        let mut brewer = test_emp("emp-brewer-1", None);
        brewer.role = Some("brewer".to_string());
        let mut bookkeeper = test_emp("emp-bk-1", None);
        bookkeeper.role = Some("bookkeeper".to_string());
        let mut ex_brewer = test_emp("emp-brewer-2", None);
        ex_brewer.role = Some("brewer".to_string());
        ex_brewer.status = Some("terminated".to_string());
        let people = Arc::new(InMemoryPeople::new(vec![brewer, bookkeeper, ex_brewer]));
        let policy: Arc<dyn PolicyClient> = Arc::new(boss_policy_client::PermissivePolicyClient);
        let app = router(PeopleApiState {
            people,
            publisher: None,
            policy: Some(policy),
            subject_kinds: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/people?role=brewer&status=active")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let emps: Vec<Employee> = serde_json::from_slice(&body).unwrap();
        // Only the active brewer — bookkeeper (wrong role) and the
        // terminated brewer (wrong status) are filtered out.
        assert_eq!(emps.len(), 1);
        assert_eq!(emps[0].id, "emp-brewer-1");
    }

    #[tokio::test]
    async fn get_by_id_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/people/emp-001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_by_id_not_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/people/emp-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_reports() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/people/emp-001/reports")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let reports: Vec<Employee> = serde_json::from_slice(&body).unwrap();
        assert_eq!(reports.len(), 2);
    }
}
