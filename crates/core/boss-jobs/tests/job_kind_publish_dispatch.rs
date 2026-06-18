//! End-to-end coverage for the `job-kind-publish` StepType
//! dispatch path.
//!
//! When a step of kind `job-kind-publish` flips to Done via PUT
//! /api/jobs/{id}/steps/{step_id}, the handler must:
//! 1. Pull `job_kind_spec` from the step metadata.
//! 2. Validate it via `validate_all`.
//! 3. Call `JobKindRegistry::publish_authored(spec, job_id)`.
//! 4. Emit `jobs.kind.published` with the full published spec.
//! 5. Persist STEP_UPDATED only AFTER the registry write succeeds.
//!
//! Decision record: `docs/architecture-decisions.md` §Jobs,
//! JobKinds, Steps (JobKinds bootstrap through Jobs).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{JobId, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::events::JOB_KIND_PUBLISHED;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{
    InMemoryJobKinds, JobKindRegistry, JobKindSpec, JobKindStatus, StepSpec, Terminal,
};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

fn cto() -> User {
    User {
        id: "emp-cto".into(),
        role: "cto".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("executive".into()),
    }
}

fn user_header(u: &User) -> String {
    serde_json::to_string(u).unwrap()
}

fn build_app(
    kinds: Arc<dyn JobKindRegistry>,
) -> (Router, Arc<InMemoryJobs>, Arc<RecordingEventBus>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Update, Resource::step(), Scope::All)
            .allow("cto", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        jobs: jobs.clone(),
        bus: bus.clone(),
        publisher,
        step_registry,
        policy,
        kind_registry: Some(kinds),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    };
    (router(state), jobs, bus)
}

async fn seed_publish_step(
    jobs: &dyn JobsRepository,
    metadata: serde_json::Value,
) -> (JobId, StepId) {
    use boss_core::job::Job as JobRow;
    let mut job = JobRow::new(
        "job-kind-design",
        Subject::new("job-kind", "morning-brew"),
        "Design morning-brew",
        "emp-cto",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
    );
    job.status = boss_core::job::JobStatus::Open;
    let job_id = job.id;
    jobs.create_job(&job).await.unwrap();

    // Single active step that will flip to Done in the test.
    let step = Step {
        id: StepId::new(),
        job_id,
        kind: "job-kind-publish".into(),
        title: "Publish".into(),
        assignee_id: None,
        status: StepStatus::Active,
        sort_order: 0,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        metadata,
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    };
    let step_id = step.id;
    jobs.add_step(&step).await.unwrap();
    (job_id, step_id)
}

fn valid_spec(kind: &str) -> JobKindSpec {
    // Must pass validate_all (the dispatch path lints before
    // publishing): a viable trigger → terminal pair.
    JobKindSpec::platform_seed(
        kind,
        "Morning Brew",
        "production",
        vec!["location".into()],
        vec![
            StepSpec {
                title: "start".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                ..Default::default()
            },
            StepSpec {
                title: "finish".into(),
                kind: "task".into(),
                ready_when: "steps.start.done".into(),
                terminal: Some(Terminal {
                    outcome: "brewed".into(),
                }),
                ..Default::default()
            },
        ],
    )
}

async fn put_step_done(
    app: &Router,
    job_id: JobId,
    step_id: StepId,
    user_json: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job_id, step_id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_json)
                .body(Body::from(json!({ "status":"completed" }).to_string()))
                .unwrap(),
        )
        .await
        .expect("router responds")
}

#[tokio::test]
async fn done_dispatches_publish_authored_and_emits_kind_published_event() {
    let kinds: Arc<dyn JobKindRegistry> = Arc::new(InMemoryJobKinds::new());
    let (app, jobs, bus) = build_app(kinds.clone());

    let spec = valid_spec("morning-brew");
    let metadata = json!({
        "job_kind_spec": serde_json::to_value(&spec).unwrap(),
    });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    let status = resp.status();
    assert!(
        status.is_success(),
        "PUT step → done must succeed, got {status}"
    );

    // Registry now has the published kind, with the meta-Job's id
    // recorded as authoring_job_id.
    let live = kinds.get_active("morning-brew").await.expect("active");
    assert_eq!(live.kind, "morning-brew");
    assert_eq!(live.version, 1);
    assert_eq!(live.status, JobKindStatus::Active);
    assert_eq!(
        live.authoring_job_id.expect("authoring stamped"),
        *job_id.inner().as_uuid(),
    );

    // The audit-bearing event landed.
    let events = bus.events();
    let published: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JOB_KIND_PUBLISHED)
        .collect();
    assert_eq!(
        published.len(),
        1,
        "exactly one jobs.kind.published event should fire"
    );
    let payload = &published[0].payload;
    assert_eq!(payload["kind"], "morning-brew");
    assert_eq!(payload["version"], 1);
    assert_eq!(payload["status"], "active");
}

#[tokio::test]
async fn missing_job_kind_spec_metadata_returns_400_no_publish() {
    let kinds: Arc<dyn JobKindRegistry> = Arc::new(InMemoryJobKinds::new());
    let (app, jobs, bus) = build_app(kinds.clone());

    let (job_id, step_id) =
        seed_publish_step(jobs.as_ref(), json!({ "previous_kind_version": 0 })).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "missing job_kind_spec must abort the step write"
    );

    // No publish event should have fired.
    let events = bus.events();
    assert!(
        events.iter().all(|e| e.kind != JOB_KIND_PUBLISHED),
        "no jobs.kind.published event must fire when dispatch fails"
    );

    // STEP_UPDATED must NOT have landed — the dispatch fails before
    // update_step_at is called, preserving audit_log integrity.
    let updated_count = events
        .iter()
        .filter(|e| e.kind == "jobs.step.updated")
        .count();
    assert_eq!(
        updated_count, 0,
        "STEP_UPDATED must not be emitted when dispatch aborts"
    );

    // Registry untouched.
    assert!(
        kinds.list_active(None).await.unwrap().is_empty(),
        "registry must stay empty when dispatch aborts"
    );
}

#[tokio::test]
async fn malformed_job_kind_spec_returns_400() {
    let kinds: Arc<dyn JobKindRegistry> = Arc::new(InMemoryJobKinds::new());
    let (app, jobs, bus) = build_app(kinds.clone());

    let (job_id, step_id) = seed_publish_step(
        jobs.as_ref(),
        json!({ "job_kind_spec": "not even an object" }),
    )
    .await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let events = bus.events();
    assert!(events.iter().all(|e| e.kind != JOB_KIND_PUBLISHED));
}

#[tokio::test]
async fn publish_step_without_kind_registry_returns_503() {
    // Mirror prod's degraded mode: the registry handle is unset.
    // Dispatch should refuse rather than silently no-op so the
    // operator notices the misconfiguration.
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Update, Resource::step(), Scope::All)
            .allow("cto", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        jobs: jobs.clone(),
        bus: bus.clone(),
        publisher,
        step_registry,
        policy,
        kind_registry: None,
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    };
    let app = router(state);

    let spec = valid_spec("morning-brew");
    let metadata = json!({
        "job_kind_spec": serde_json::to_value(&spec).unwrap(),
    });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// Silence unused import warnings when this file is the only one
// touching these names.
#[allow(dead_code)]
fn _ensure_uuid_used(_id: Uuid) {}
