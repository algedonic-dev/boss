//! A marker that is READY but whose `step.ready` event never completed it
//! is completed by the five-minute readiness sweep (backlog ee399c2c).
//!
//! `complete-marker-on-step-ready` runs only on the event. Measured
//! 2026-10-30 04:12-04:30Z: gate-run a2e19072 had `record-verdict`
//! completed and its `green` outcome READY with no `completed_by`, and
//! stayed open; the conductor's settle re-send (a status-only PUT of
//! `record-verdict`) cannot re-emit a ready event for a step already
//! ready, and the readiness sweep looked only at PENDING steps. The jobs
//! API was logging 35-60 statement timeouts per ten minutes, so a lost
//! marker completion is expected, not rare.
//!
//! The in-memory app below has no dispatcher: every `step.ready` event it
//! publishes is never handled, which is exactly the lost event.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::actor::ActorId;
use boss_core::job::{JobId, JobStatus, StepId, StepStatus};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The sweep's own rule, as the dispatcher names it on every write.
const SWEEP: &str = "rule:pending-readiness-every-five-minutes";
/// The event's rule — the writer the sweep races.
const EVENT_RULE: &str = "rule:complete-marker-on-step-ready";

fn task(title: &str, ready_when: &str) -> StepSpec {
    StepSpec {
        title: title.into(),
        kind: "task".into(),
        ready_when: ready_when.into(),
        title_template: title.into(),
        authority_role: Some("platform-admin".into()),
        ..Default::default()
    }
}

/// An `outcome` marker: no role, zero duration, a declared terminal — the
/// shape of a gate-run's `green`.
fn outcome(title: &str, ready_when: &str) -> StepSpec {
    StepSpec {
        title: title.into(),
        kind: "outcome".into(),
        ready_when: ready_when.into(),
        title_template: title.into(),
        terminal: Some(Terminal {
            outcome: title.into(),
        }),
        ..Default::default()
    }
}

struct AdminRoster;

#[async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

fn caller(id: &str) -> String {
    serde_json::json!({
        "id": id,
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// `marks`: `verdict` (work) → `green` (an outcome marker waiting on it).
fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    let mut spec = WorkflowSpec::platform_seed(
        "marks",
        "marks",
        "platform",
        vec!["custom".into()],
        vec![
            task("verdict", "true"),
            outcome("green", "steps.verdict.done"),
        ],
    );
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    kinds.seed(spec).expect("seed the marker kind");
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn open_job(app: &axum::Router) -> JobId {
    let (status, body) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", caller("emp-bootstrap-admin"))
            .body(Body::from(
                serde_json::json!({
                    "kind": "marks",
                    "subject": { "subject_kind": "custom", "id": "/system/flow" },
                    "title": "A marks packet",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": {},
                    "tags": ["test"],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {body}");
    let job: serde_json::Value = serde_json::from_str(&body).expect("job json");
    JobId::from_uuid(uuid::Uuid::parse_str(job["id"].as_str().expect("job id")).expect("uuid"))
}

async fn step_titled(jobs: &InMemoryJobs, job_id: &JobId, title: &str) -> boss_core::job::Step {
    jobs.list_steps(job_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("no step titled {title}"))
}

/// The verdict's completion, through the door: its readiness pass makes
/// `green` READY and publishes `step.ready`, which nothing here handles.
async fn record_verdict(app: &axum::Router, jobs: &InMemoryJobs, job_id: &JobId) {
    let step = step_titled(jobs, job_id, "verdict").await;
    let (status, body) = send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{}", step.id))
            .header("content-type", "application/json")
            .header("x-boss-user", caller("emp-bootstrap-admin"))
            .body(Body::from(
                serde_json::json!({ "status": "completed" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "verdict refused: {body}");
}

/// One firing of the sweep, as the dispatcher's clock rule sends it.
async fn sweep(app: &axum::Router) -> serde_json::Value {
    let (status, body) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs/readiness")
            .header("x-boss-user", caller(SWEEP))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "readiness repair: {body}");
    let report: serde_json::Value = serde_json::from_str(&body).expect("report json");
    assert_eq!(report["complete"], true, "{body}");
    assert_eq!(report["errors"], serde_json::json!([]), "{body}");
    report
}

/// Every `jobs.step.updated` recorded for `step` that completed it.
fn completions(jobs: &InMemoryJobs, step: &StepId) -> usize {
    jobs.recorded_events()
        .into_iter()
        .filter(|e| e.kind == boss_jobs::events::STEP_UPDATED)
        .filter(|e| e.payload["step_id"] == step.to_string())
        .filter(|e| e.payload["status"] == "completed")
        .count()
}

/// The report's row for `step`, if the sweep named it.
fn named<'a>(report: &'a serde_json::Value, step: &StepId) -> Option<&'a serde_json::Value> {
    report["ready_markers"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["step_id"] == step.to_string()))
}

#[tokio::test]
async fn a_ready_marker_whose_step_ready_event_was_never_handled_is_completed_by_the_sweep() {
    let (app, jobs) = app();
    let id = open_job(&app).await;
    record_verdict(&app, &jobs, &id).await;
    let green = step_titled(&jobs, &id, "green").await;
    assert_eq!(green.status, StepStatus::Ready, "the marker went ready");
    assert!(
        green.completed_by.is_none(),
        "and its event was never handled"
    );

    let report = sweep(&app).await;

    let green = jobs.get_step(&green.id).await.unwrap().unwrap();
    assert_eq!(green.status, StepStatus::Completed, "{report}");
    assert_eq!(
        green.completed_by,
        Some(ActorId::Automation(SWEEP.into())),
        "credited to the sweep's rule, never a person"
    );
    assert_eq!(completions(&jobs, &green.id), 1);
    assert_eq!(
        jobs.get_job(&id).await.unwrap().unwrap().status,
        JobStatus::Closed,
        "the terminal marker settles its packet"
    );
    let row = named(&report, &green.id).expect("the report names what it completed");
    assert_eq!(row["outcome"], "completed", "{report}");
    assert_eq!(row["kind"], "outcome", "{report}");
    assert_eq!(row["job_id"], id.to_string(), "{report}");

    let again = sweep(&app).await;
    assert_eq!(
        completions(&jobs, &green.id),
        1,
        "a second pass completes nothing twice"
    );
    assert!(named(&again, &green.id).is_none(), "{again}");
}

#[tokio::test]
async fn a_ready_step_that_is_not_a_marker_is_left_for_its_executor() {
    let (app, jobs) = app();
    let id = open_job(&app).await;
    let verdict = step_titled(&jobs, &id, "verdict").await;
    assert_eq!(verdict.status, StepStatus::Ready);

    let report = sweep(&app).await;

    let verdict = jobs.get_step(&verdict.id).await.unwrap().unwrap();
    assert_eq!(
        verdict.status,
        StepStatus::Ready,
        "a task is work: {report}"
    );
    assert!(verdict.completed_by.is_none());
    assert_eq!(completions(&jobs, &verdict.id), 0);
    assert!(named(&report, &verdict.id).is_none(), "{report}");
    assert_eq!(
        jobs.get_job(&id).await.unwrap().unwrap().status,
        JobStatus::Open
    );
}

#[tokio::test]
async fn a_marker_the_event_completes_inside_the_sweeps_window_is_not_completed_twice() {
    let (app, jobs) = app();
    let id = open_job(&app).await;
    record_verdict(&app, &jobs, &id).await;
    let green = step_titled(&jobs, &id, "green").await;
    assert_eq!(green.status, StepStatus::Ready);
    // The event handler's completion lands between the sweep's read of
    // the row and its write.
    jobs.change_before_next_judged_write(&green.id, |step| {
        step.status = StepStatus::Completed;
        step.completed_by = Some(ActorId::Automation(EVENT_RULE.into()));
    });

    let report = sweep(&app).await;

    let green = jobs.get_step(&green.id).await.unwrap().unwrap();
    assert_eq!(green.status, StepStatus::Completed);
    assert_eq!(
        green.completed_by,
        Some(ActorId::Automation(EVENT_RULE.into())),
        "the writer that landed first keeps the credit"
    );
    assert_eq!(
        completions(&jobs, &green.id),
        0,
        "the sweep recorded no completion of its own"
    );
    let row = named(&report, &green.id).expect("the race is on the record");
    assert_eq!(row["outcome"], "raced", "{report}");
}

#[tokio::test]
async fn the_audit_names_a_ready_marker_and_writes_nothing() {
    let (app, jobs) = app();
    let id = open_job(&app).await;
    record_verdict(&app, &jobs, &id).await;
    let green = step_titled(&jobs, &id, "green").await;
    let (status, body) = send(
        &app,
        Request::builder()
            .uri("/api/jobs/readiness")
            .header("x-boss-user", caller(SWEEP))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    let row = named(&report, &green.id).expect("the audit names the ready marker");
    assert_eq!(row["outcome"], "ready", "{report}");
    assert_eq!(
        jobs.get_step(&green.id).await.unwrap().unwrap().status,
        StepStatus::Ready,
        "an audit never writes"
    );
}
