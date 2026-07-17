//! The create handler must pin a new Job to its kind's *active*
//! version. Per docs/architecture-decisions.md §Jobs, JobKinds, Steps:
//! creation is blocked against draft/retired kinds, and in-flight Jobs
//! pin to the version they opened under — so a freshly created Job's
//! `job_kind_version` is the active version at open time, never the
//! schema DEFAULT 1 and never a client-supplied value.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{JobKindRegistry, JobKindSpec, JobKindStatus};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobKinds, InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use tower::ServiceExt;

fn ceo_header() -> String {
    serde_json::json!({
        "id": "emp-ceo",
        "role": "ceo",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "executive",
    })
    .to_string()
}

fn versioned_spec(version: i32, status: JobKindStatus) -> JobKindSpec {
    let mut s = JobKindSpec::platform_seed(
        "versioned",
        "Versioned",
        "test",
        vec!["system".into()],
        Vec::new(),
    );
    s.version = version;
    s.status = status;
    s
}

#[tokio::test]
async fn new_job_pins_to_active_version_not_default_one() {
    // A kind whose ACTIVE version is 3 (v1, v2 retired by prior publishes).
    let kinds = Arc::new(InMemoryJobKinds::new());
    kinds
        .seed(versioned_spec(1, JobKindStatus::Retired))
        .unwrap();
    kinds
        .seed(versioned_spec(2, JobKindStatus::Retired))
        .unwrap();
    kinds
        .seed(versioned_spec(3, JobKindStatus::Active))
        .unwrap();

    let jobs = Arc::new(InMemoryJobs::new());
    let kind_registry: Arc<dyn JobKindRegistry> = kinds;
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        jobs: jobs.clone(),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kind_registry),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    let app = router(state);

    // Client sends a deliberately-wrong version (99); the server must
    // override it with the kind's active version (3).
    let mut job = Job::new(
        "versioned",
        Subject::new("system", "sys-1"),
        "Pin test",
        "emp-ceo",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 4, 28).unwrap(),
    );
    job.job_kind_version = 99;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", ceo_header())
                .body(Body::from(serde_json::to_vec(&job).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "create should succeed against the active kind"
    );

    let stored = jobs
        .get_job(&job.id)
        .await
        .unwrap()
        .expect("job was stored");
    assert_eq!(
        stored.job_kind_version, 3,
        "new Job must pin to the active version (3), not the client value (99) or DEFAULT 1"
    );
}
