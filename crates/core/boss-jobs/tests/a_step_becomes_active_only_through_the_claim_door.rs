//! A step becomes Active only through the claim door.
//!
//! THE DEFECT (backlog 6ef4a36b, finding (2) of the review of car
//! 781b9209). An active step keeps its holder until it is released
//! (`active_holder`), and a release makes it Ready — but a step PUT
//! could still take a Ready step to Active naming ANYONE:
//! `{"status":"active","assignee_id":"emp-x"}` installed a holder who
//! never claimed it, 204, outside the compare-and-set that decides who
//! wins a step, outside the claim-for rule (design 611fbffd: only the
//! declared executor or a `step-assign` holder may start a step for
//! someone else), and outside the station and budget gates the claim
//! door judges. "Release, then claim" was a sentence in a hint that
//! nothing enforced.
//!
//! Design 611fbffd clause (b) moved every caller to the claim door
//! first — the web's Start (car fix/every-start-goes-through-the-claim-
//! door), the sim's workforce, the ops runner — so the PUT's refusal is
//! the last half: Ready→Active and Pending→Active by PUT answer 409,
//! naming `POST /api/jobs/{id}/steps/{step_id}/claim`, and nothing is
//! written. An Active step's own writes (its holder's notes, its
//! completion) are untouched: they do not open it.

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
use tower::ServiceExt;
use uuid::Uuid;

const JOB: &str = "00000000-0000-0000-0000-00000000d0a1";
const STEP: &str = "00000000-0000-0000-0000-00000000d0b1";

fn user(id: &str) -> User {
    User {
        id: id.to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn allow_update() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .build(),
    )
}

async fn seed(status: StepStatus, assignee: Option<&str>) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        publisher,
        allow_update(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    let job_id = JobId::from_uuid(Uuid::parse_str(JOB).unwrap());
    let job = Job {
        id: job_id,
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            "backlog-item",
            Subject::new("custom", "bosspipeline"),
            "A step someone may start",
            "emp-owner",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
        )
    };
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job_id, "task", "Build the change", 1);
    step.id = StepId::from_uuid(Uuid::parse_str(STEP).unwrap());
    step.spec_slug = Some("build".into());
    step.status = status;
    step.assignee_id = assignee.map(str::to_string);
    step.metadata = serde_json::json!({ "authority_role": "platform-admin" });
    jobs.add_step(&step).await.unwrap();
    (router(state), jobs)
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    as_user: &str,
    body: &str,
) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header(
                    "x-boss-user",
                    serde_json::to_string(&user(as_user)).unwrap(),
                )
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn put_step(app: &Router, as_user: &str, body: &str) -> (StatusCode, String) {
    send(
        app,
        "PUT",
        &format!("/api/jobs/{JOB}/steps/{STEP}"),
        as_user,
        body,
    )
    .await
}

async fn claim(app: &Router, as_user: &str) -> (StatusCode, String) {
    send(
        app,
        "POST",
        &format!("/api/jobs/{JOB}/steps/{STEP}/claim"),
        as_user,
        "{}",
    )
    .await
}

async fn stored(jobs: &InMemoryJobs) -> Step {
    jobs.get_step(&StepId::from_uuid(Uuid::parse_str(STEP).unwrap()))
        .await
        .unwrap()
        .expect("step exists")
}

/// The refusal names the door, on the wire, as the builder spells it —
/// one shape, read by every caller that meets it (CLAUDE.md §9a).
fn assert_names_the_claim_door(body: &str, from: &str) {
    let v: serde_json::Value = serde_json::from_str(body).expect("the refusal is JSON");
    assert_eq!(
        v,
        boss_jobs::active_holder::opening_refusal_body(JOB, STEP, from),
        "the refusal on the wire is the builder's: {body}"
    );
    assert_eq!(
        v["claim_door"],
        format!("/api/jobs/{JOB}/steps/{STEP}/claim"),
        "names the door by its path: {body}"
    );
    assert_eq!(
        v["step_status"], from,
        "names the status it stands in: {body}"
    );
}

/// THE BUG, in the shape the review probed: a Ready step — a released
/// one, or one never claimed — taken to Active by PUT naming a holder
/// who never claimed it.
#[tokio::test]
async fn a_put_does_not_start_a_ready_step_for_anyone() {
    for body in [
        r#"{"status":"active","assignee_id":"emp-never-claimed"}"#,
        r#"{"status":"active"}"#,
    ] {
        let (app, jobs) = seed(StepStatus::Ready, None).await;

        let (status, answer) = put_step(&app, "emp-op", body).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "Ready->Active by PUT must be refused — it installed a holder outside the \
             claim (6ef4a36b).\nsent {body}\nanswer: {answer}"
        );
        assert_names_the_claim_door(&answer, "ready");

        let after = stored(&jobs).await;
        assert_eq!(after.status, StepStatus::Ready, "nothing written: {body}");
        assert_eq!(after.assignee_id, None, "nobody installed: {body}");
    }
}

/// The three-write variant the review named — release, nominate, start
/// — ends at the same refusal: a nomination is not a claim.
#[tokio::test]
async fn release_then_nominate_then_start_is_refused_at_the_start() {
    let (app, jobs) = seed(StepStatus::Ready, None).await;
    let (status, answer) = claim(&app, "emp-claimant").await;
    assert_eq!(status, StatusCode::OK, "{answer}");

    let (status, answer) =
        put_step(&app, "emp-op", r#"{"status":"ready","assignee_id":null}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the release: {answer}");
    let (status, answer) = put_step(&app, "emp-op", r#"{"assignee_id":"emp-other"}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the nomination: {answer}");

    let (status, answer) = put_step(&app, "emp-op", r#"{"status":"active"}"#).await;
    assert_eq!(status, StatusCode::CONFLICT, "the start: {answer}");
    assert_names_the_claim_door(&answer, "ready");
    assert_eq!(stored(&jobs).await.status, StepStatus::Ready);
}

/// A Pending step is refused the same way, even with nothing blocking
/// it: it was judged by the blocker and predicate gates and admitted
/// (36352452), but opening it straight to Active is still a start
/// outside the claim.
#[tokio::test]
async fn a_put_does_not_start_a_pending_step() {
    let (app, jobs) = seed(StepStatus::Pending, None).await;

    let (status, answer) = put_step(
        &app,
        "emp-op",
        r#"{"status":"active","assignee_id":"emp-op"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_names_the_claim_door(&answer, "pending");
    let after = stored(&jobs).await;
    assert_eq!(after.status, StepStatus::Pending);
    assert_eq!(after.assignee_id, None);
}

/// The door it names works, and the holder's own writes on the started
/// step — a re-send of `active` in a read-merge-write, notes, the
/// completion — are not openings and pass.
#[tokio::test]
async fn the_claim_door_starts_it_and_the_holders_writes_still_land() {
    let (app, jobs) = seed(StepStatus::Ready, None).await;
    let (status, answer) = claim(&app, "emp-claimant").await;
    assert_eq!(status, StatusCode::OK, "{answer}");

    let (status, answer) = put_step(
        &app,
        "emp-claimant",
        r#"{"status":"active","assignee_id":"emp-claimant","notes":"on it"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "a re-send: {answer}");
    let (status, answer) = put_step(&app, "emp-claimant", r#"{"status":"completed"}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the completion: {answer}");

    let after = stored(&jobs).await;
    assert_eq!(after.status, StepStatus::Completed);
    assert_eq!(after.assignee_id.as_deref(), Some("emp-claimant"));
    assert_eq!(after.notes.as_deref(), Some("on it"));
}
