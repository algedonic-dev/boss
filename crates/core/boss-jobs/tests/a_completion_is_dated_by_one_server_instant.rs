//! A completion is dated by the server, from ONE instant.
//!
//! THE DEFECT (backlog f3e78bdf, split out of car 8df80582 on its
//! review, 2026-09-25; paired with part (1) of a5956b1a):
//!
//! - A completing PUT kept a body `completed_on` (`if is_flipping_to_done
//!   && step.completed_on.is_none()`), so a caller chose its own
//!   completion date — and a job's `closed_on`, and every ledger date a
//!   dispatcher handler reads off the step, followed it.
//! - When the server did stamp it, the date came from one clock read and
//!   `completed_at` from a second, ~400 lines later. Across midnight the
//!   two disagree, and the row says it completed on one day at an instant
//!   on the next.
//! - `POST /api/jobs/{id}/steps` kept a body `completed_on` on a step
//!   born completed (a5956b1a (1)), and stamped `completed_at` from the
//!   record stamp (wall time) rather than the clock that dates the day.
//!
//! THE RULE: the completion's date and instant are the server's, read
//! from the clock port ONCE (the sim clock on an instance running
//! simulated time) — `completed_on` is `completed_at`'s date. A body
//! that names a `completed_on` other than the stored one is REFUSED,
//! naming the field, on both doors and on every partition; a re-send of
//! the stored value (a read-merge-write body) is not a date and passes.
//! Refused rather than ignored so the caller learns the rule at the
//! call: a silently dropped date is a write that half-landed. The only
//! caller that ever sent one — the simulator's live step-update flush —
//! was deleted in train #728.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use tower::ServiceExt;
use uuid::Uuid;

const JOB: &str = "00000000-0000-0000-0000-00000000d001";
const OPEN: &str = "00000000-0000-0000-0000-00000000d101";
const LATER: &str = "00000000-0000-0000-0000-00000000d102";

/// A clock whose every read lands on a DIFFERENT day — each read is one
/// day later than the last, starting a second before midnight. Two
/// reads can never agree on a date, so a completion whose `completed_on`
/// equals its `completed_at`'s date was dated from one read. Set years
/// away from the wall clock, so a stamp from the wall is told apart too.
struct EveryReadIsTomorrow {
    reads: AtomicI64,
}

fn first_read() -> DateTime<Utc> {
    NaiveDate::from_ymd_opt(2031, 3, 14)
        .unwrap()
        .and_hms_opt(23, 59, 59)
        .unwrap()
        .and_utc()
}

#[async_trait]
impl ClockClient for EveryReadIsTomorrow {
    async fn now(&self) -> ClockNow {
        let n = self.reads.fetch_add(1, Ordering::SeqCst);
        ClockNow {
            now: first_read() + chrono::Duration::days(n),
            simulated: true,
            epoch_start: None,
            epoch_end: None,
            paused: false,
            restart_in_progress: false,
            warp_factor: None,
        }
    }
}

fn operator() -> User {
    User {
        id: "emp-david".to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn policy() -> Arc<dyn PolicyClient> {
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

fn job_id() -> JobId {
    JobId::from_uuid(Uuid::parse_str(JOB).unwrap())
}

fn step_id(id: &str) -> StepId {
    StepId::from_uuid(Uuid::parse_str(id).unwrap())
}

fn open_step(id: &str, sort_order: i32) -> Step {
    let mut s = Step::new(job_id(), "generic", "Do the work", sort_order);
    s.id = step_id(id);
    s.status = StepStatus::Ready;
    s
}

/// A packet of a kind no registry describes (the only kind the step
/// POST accepts), with one open step, plus a second that keeps it open
/// until a test wants it closed.
async fn seed(steps: &[Step]) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy(),
        Arc::new(EveryReadIsTomorrow {
            reads: AtomicI64::new(0),
        }),
    );
    jobs.create_job(&Job {
        id: job_id(),
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            "unregistered-chore",
            Subject::new("custom", "chore"),
            "a chore",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2031, 3, 1).unwrap(),
        )
    })
    .await
    .unwrap();
    for s in steps {
        jobs.add_step(s).await.unwrap();
    }
    (router(state), jobs)
}

async fn send(
    app: &Router,
    method: &str,
    uri: String,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&operator()).unwrap())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn put(app: &Router, id: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    send(app, "PUT", format!("/api/jobs/{JOB}/steps/{id}"), body).await
}

async fn post(app: &Router, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    send(app, "POST", format!("/api/jobs/{JOB}/steps"), body).await
}

async fn stored(jobs: &InMemoryJobs, id: &str) -> Step {
    jobs.get_step(&step_id(id)).await.unwrap().expect("step")
}

fn refused(body: &serde_json::Value) -> Vec<String> {
    body["refused_fields"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The day and the instant are one read: `completed_on` is
/// `completed_at`'s date, both from the clock port, and the packet's
/// `closed_on` inherits that same day.
#[tokio::test]
async fn a_completing_put_dates_the_step_from_one_clock_read() {
    let (app, jobs) = seed(&[open_step(OPEN, 0)]).await;

    let (status, body) = put(&app, OPEN, serde_json::json!({ "status": "completed" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let done = stored(&jobs, OPEN).await;
    let at = done.completed_at.expect("the flip stamps completed_at");
    assert_eq!(
        at.year(),
        2031,
        "completed_at is read from the clock port (the sim clock on a simulated \
         instance), not from the wall: {at}"
    );
    assert_eq!(
        done.completed_on,
        Some(at.date_naive()),
        "completed_on is completed_at's date — one read, not two that can straddle \
         midnight (completed_at {at})"
    );
    let job = jobs.get_job(&job_id()).await.unwrap().expect("job");
    assert_eq!(
        job.status,
        JobStatus::Closed,
        "the only step closed the packet"
    );
    assert_eq!(
        job.closed_on, done.completed_on,
        "closed_on inherits the completion's day"
    );
}

/// A step born completed is a completion, dated the same way — not from
/// the body, and not from the wall-clock record stamp (a5956b1a (1)).
#[tokio::test]
async fn a_step_born_completed_is_dated_from_one_clock_read() {
    let (app, jobs) = seed(&[open_step(OPEN, 0)]).await;
    let (status, body) = post(
        &app,
        serde_json::json!({
            "id": LATER,
            "job_id": JOB,
            "kind": "generic",
            "title": "Done on arrival",
            "status": "completed",
            "sort_order": 1,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let born = stored(&jobs, LATER).await;
    let at = born.completed_at.expect("a step born completed is stamped");
    assert_eq!(at.year(), 2031, "from the clock port, not the wall: {at}");
    assert_eq!(
        born.completed_on,
        Some(at.date_naive()),
        "the server dates it, from the same read (completed_at {at})"
    );
}

/// A completing PUT that names its own day is refused, naming the field;
/// nothing is written. So is a PUT that sets one without completing —
/// an open step has no completion date to set.
#[tokio::test]
async fn a_put_that_names_its_own_completion_date_is_refused() {
    let (app, jobs) = seed(&[open_step(OPEN, 0)]).await;
    for body in [
        serde_json::json!({ "status": "completed", "completed_on": "2020-01-01" }),
        serde_json::json!({ "completed_on": "2020-01-01" }),
    ] {
        let (status, resp) = put(&app, OPEN, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{body}: a caller does not choose its completion's date: {resp}"
        );
        assert_eq!(refused(&resp), vec!["completed_on"], "{body}: {resp}");
        let after = stored(&jobs, OPEN).await;
        assert_eq!(after.status, StepStatus::Ready, "{body}: nothing completed");
        assert_eq!(after.completed_on, None, "{body}: nothing dated");
    }
}

/// A read-merge-write body sends back what it read — `completed_on:
/// null` on an open step. That is not a date, and it still completes.
/// The row goes back without its `metadata`: the step PUT refuses any
/// metadata body since e39a9d2a, and a whole-row write-back is not a
/// metadata write.
#[tokio::test]
async fn a_re_send_of_the_stored_completion_date_is_not_a_date() {
    let (app, jobs) = seed(&[open_step(OPEN, 0), open_step(LATER, 1)]).await;
    let read = serde_json::to_value(stored(&jobs, OPEN).await).unwrap();
    let mut body = read.as_object().unwrap().clone();
    body.remove("metadata");
    body.insert("status".into(), serde_json::json!("completed"));
    let (status, resp) = put(&app, OPEN, serde_json::Value::Object(body)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{resp}");
    let done = stored(&jobs, OPEN).await;
    let at = done.completed_at.expect("stamped");
    assert_eq!(done.completed_on, Some(at.date_naive()));
}

/// A posted step that names its own completion date is refused, naming
/// the field, whatever status it is born with; nothing is stored.
#[tokio::test]
async fn a_posted_step_that_names_its_own_completion_date_is_refused() {
    let (app, jobs) = seed(&[open_step(OPEN, 0)]).await;
    for status_word in ["completed", "ready"] {
        let (status, resp) = post(
            &app,
            serde_json::json!({
                "id": LATER,
                "job_id": JOB,
                "kind": "generic",
                "title": "Backdated",
                "status": status_word,
                "sort_order": 1,
                "completed_on": "2020-01-01",
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{status_word}: a posted step does not choose its completion's date: {resp}"
        );
        assert_eq!(
            refused(&resp),
            vec!["completed_on"],
            "{status_word}: {resp}"
        );
        assert!(
            jobs.get_step(&step_id(LATER)).await.unwrap().is_none(),
            "{status_word}: nothing stored"
        );
    }
}
