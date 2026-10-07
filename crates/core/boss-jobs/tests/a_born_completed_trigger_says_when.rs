//! A trigger born `Completed` at admission records WHEN, not only which day.
//!
//! Measured 2026-09-28 on gate-run 6d5d85fb (backlog 4d088a7e): its
//! `launched` trigger step read `status completed` with `completed_on`
//! set and `completed_at` null. Every other completion path stamps both
//! (c17871fe); the admission path resolved the firing trigger in
//! `registry::resolve_triggers`, which only has the day, and nothing
//! stamped the instant — so a reader asking "when did this packet's
//! first step finish" got a date, and the yard fell back to guessing.
//!
//! Run through the create handler with the REAL platform bundle's
//! `gate-run` row, the shape the incident had.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::StepStatus;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{WorkflowRegistry, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use serde_json::json;
use tower::ServiceExt;

fn admin_header() -> String {
    json!({
        "id": "emp-david",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

#[tokio::test]
async fn the_gate_runs_launched_trigger_is_born_with_its_completed_at() {
    let spec = load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == "gate-run")
        .expect("the bundle ships gate-run");
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(spec).expect("spec seeds");
    let jobs = Arc::new(InMemoryJobs::new());
    let kind_registry: Arc<dyn WorkflowRegistry> = kinds;
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kind_registry),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let app = router(state);

    let body = json!({
        "kind": "gate-run",
        "status": "open",
        "title": "Gate fix/x",
        "owner_id": "emp-david",
        "priority": "standard",
        "opened_on": "2026-09-28",
        "tags": [],
        "subject": { "subject_kind": "custom", "id": "boss-platform" },
        "metadata": { "branch": "fix/x" },
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", admin_header())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );

    let (stored, _) = jobs.list_jobs(&Default::default(), 10, 0).await.unwrap();
    let job = stored.first().expect("job stored");
    let steps = jobs.list_steps(&job.id).await.unwrap();
    let launched = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("launched"))
        .expect("the launched trigger materialized");
    assert_eq!(launched.status, StepStatus::Completed);
    assert!(
        launched.completed_on.is_some(),
        "the day was always stamped"
    );
    let at = launched
        .completed_at
        .expect("a trigger born completed carries the instant it completed");
    assert_eq!(
        Some(at.date_naive()),
        launched.completed_on,
        "the instant and the day are one reading of one clock"
    );
    // A step that is NOT born completed gains no stamp.
    let verdict = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("record-verdict"))
        .expect("the verdict step materialized");
    assert_eq!(verdict.completed_at, None);
}
