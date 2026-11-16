//! A step write whose settle could not read the packet's steps is not a
//! 204 (backlog de9b48de).
//!
//! WHY THIS FILE EXISTS. After a step write commits, the settle runs the
//! all-steps-terminal catch-all close: it reads the packet's steps and
//! closes the packet when every one is terminal. It read them with
//! `if let Ok(steps) = list_steps(..)`, so a read the store could not
//! answer was taken as "nothing to close" and the write answered 204
//! over a packet left open with every step finished — no refusal, no
//! retry hint, nothing recorded. Builder run fad7ae92 found it while
//! fixing the same silent 204 on a re-sent skip (backlog 657c9200).
//!
//! The packet here has NO protocol (no workflow registry), so no
//! readiness pass reads the steps first and the catch-all's read is the
//! one the injected failure lands on. The answer is a 500 naming the
//! packet and the read, and the re-send it asks for settles the packet
//! once the read recovers.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

const PACKET: &str = "00000000-0000-0000-0000-0000000de9b4";

fn id() -> JobId {
    JobId::from_uuid(Uuid::parse_str(PACKET).expect("uuid"))
}

fn ceo() -> String {
    serde_json::to_string(&User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    })
    .expect("a User always serialises")
}

/// An open packet of a kind no registry knows, with one ready step.
async fn seed() -> (Router, Arc<InMemoryJobs>, StepId) {
    let jobs = Arc::new(InMemoryJobs::new());
    let job = Job {
        id: id(),
        status: JobStatus::Open,
        ..Job::new(
            "no-protocol",
            Subject::new("custom", "/system/flow"),
            "a packet with no protocol",
            "emp-ceo",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 20).expect("day"),
        )
    };
    jobs.create_job(&job).await.expect("job");
    let mut step = Step::new(id(), "task", "the only work", 1);
    step.id = StepId::new();
    step.status = StepStatus::Ready;
    jobs.add_step(&step).await.expect("step");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs, step.id)
}

async fn complete(app: &Router, step: StepId) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{PACKET}/steps/{step}"))
                .header("content-type", "application/json")
                .header("x-boss-user", ceo())
                .body(Body::from(
                    serde_json::json!({ "status": "completed" }).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn packet_status(jobs: &InMemoryJobs) -> JobStatus {
    jobs.get_job(&id())
        .await
        .expect("job read")
        .expect("job exists")
        .status
}

#[tokio::test]
async fn control_a_readable_packet_closes_on_its_last_step() {
    let (app, jobs, step) = seed().await;
    let (status, body) = complete(&app, step).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "control write: {body}");
    assert_eq!(
        packet_status(&jobs).await,
        JobStatus::Closed,
        "control: the catch-all closes a packet whose every step is terminal"
    );
}

#[tokio::test]
async fn a_settle_whose_steps_read_failed_answers_500_naming_the_read() {
    let (app, jobs, step) = seed().await;
    jobs.fail_steps_read(&id());
    let (status, body) = complete(&app, step).await;
    assert_eq!(
        packet_status(&jobs).await,
        JobStatus::Open,
        "precondition: the catch-all could not read the steps, so nothing closed"
    );
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a write whose settle could not read the steps must not answer success: {body}"
    );
    assert!(
        body.contains(PACKET) && body.contains("steps") && body.contains("send the same request"),
        "the answer names the packet, the read that failed, and the retry: {body}"
    );
}

#[tokio::test]
async fn the_resend_it_asks_for_settles_the_packet() {
    let (app, jobs, step) = seed().await;
    jobs.fail_steps_read(&id());
    let (status, body) = complete(&app, step).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "first write: {body}"
    );

    jobs.restore_steps_read(&id());
    let (status, body) = complete(&app, step).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the re-send: {body}");
    assert_eq!(
        packet_status(&jobs).await,
        JobStatus::Closed,
        "the re-send ran the catch-all the first write could not"
    );
}
