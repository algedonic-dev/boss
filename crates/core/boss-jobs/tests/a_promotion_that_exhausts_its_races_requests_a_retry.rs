//! Real competing row changes can outlive every bounded readiness pass.
//! Model writers interrupted after their commit, before their own settle;
//! this proves a failure shape, not the cause of the historical incident.
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use boss_core::{
    job::{Job, JobStatus, Priority, StepId, StepStatus, Subject},
    port::EventBus,
    publisher::DomainPublisher,
};
use boss_jobs::{
    InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry,
    http::{JobsApiState, router},
    registry::materialize_steps,
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn retry(app: &Router, job: &Job, step: StepId) -> (StatusCode, String) {
    let reply = app.clone().oneshot(Request::builder()
        .method("PUT").uri(format!("/api/jobs/{}/steps/{step}", job.id))
        .header("content-type", "application/json")
        .header("x-boss-user", serde_json::json!({"id":"automation:boss-step","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}).to_string())
        .body(Body::from(r#"{"status":"completed"}"#)).unwrap()).await.unwrap();
    let status = reply.status();
    let bytes = reply.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn exercise(races: usize) {
    let spec = boss_jobs::seed_loader::load_workflows(
        boss_testing::repo_root()
            .join("infra/platform/workflows/maintenance-conservation-invariants.toml"),
    )
    .unwrap()
    .remove(0);
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(spec.clone()).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    let mut job = Job::new(
        &spec.kind,
        Subject::new("custom", "chore"),
        "Interrupted settle",
        "emp-david",
        Priority::Urgent,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 23).unwrap(),
    );
    job.workflow_version = spec.version;
    job.status = JobStatus::Open;
    jobs.create_job(&job).await.unwrap();
    let mut rows = materialize_steps(&spec, &job.subject, job.id, &job.metadata, StepId::new);
    for row in &mut rows {
        row.status = if row.sort_order < 2 {
            StepStatus::Completed
        } else {
            StepStatus::Pending
        };
        if row.spec_slug.as_deref() == Some("run") {
            row.metadata["result"] = "ok".into();
            row.metadata["output"] = "original output".into();
            row.completed_at = Some(chrono::Utc::now());
        }
        jobs.add_step(row).await.unwrap();
    }
    let run = rows
        .iter()
        .find(|r| r.spec_slug.as_deref() == Some("run"))
        .unwrap()
        .clone();
    let outcome = rows
        .iter()
        .find(|r| r.spec_slug.as_deref() == Some("completed"))
        .unwrap()
        .id;
    let changes = (0..races)
        .map(|i| {
            Box::new(move |row: &mut boss_core::job::Step| {
                assert_eq!(row.status, StepStatus::Pending);
                row.notes = Some(format!(
                    "competing writer {i}: committed, then interrupted before settle"
                ));
            }) as Box<dyn FnOnce(&mut boss_core::job::Step) + Send>
        })
        .collect();
    jobs.changes_before_judged_writes(&outcome, changes);
    let bus = RecordingEventBus::new();
    let events: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .build(),
    );
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(events, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let app = router(state);
    let (status, text) = retry(&app, &job, run.id).await;
    let after = jobs.get_step(&outcome).await.unwrap().unwrap();
    assert!(
        after.notes.is_some(),
        "the competing row write was not reached: {status} {text}"
    );
    if races == 3 {
        assert_eq!(after.status, StepStatus::Pending);
        assert!(
            after.notes.as_deref().unwrap().contains("writer 2"),
            "all three genuine writes must land"
        );
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "success hides exhausted promotion races: {text}"
        );
        assert!(
            text.contains("again") && text.contains(&job.id.to_string()),
            "retry must be actionable: {text}"
        );
        let (status, text) = retry(&app, &job, run.id).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    } else {
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "one race must converge: {text}"
        );
    }
    assert_eq!(
        jobs.get_step(&outcome).await.unwrap().unwrap().status,
        StepStatus::Ready
    );
    assert_eq!(
        jobs.get_step(&run.id).await.unwrap().unwrap(),
        run,
        "retry never rewrites original evidence"
    );
    assert_eq!(
        jobs.get_job(&job.id).await.unwrap().unwrap().status,
        JobStatus::Open,
        "readiness alone never fabricates the dispatcher's outcome completion"
    );
}

#[tokio::test]
async fn three_interrupted_competing_writers_do_not_answer_success_over_pending_work() {
    exercise(3).await;
}

#[tokio::test]
async fn one_competing_row_change_is_settled_within_the_bound() {
    exercise(1).await;
}
