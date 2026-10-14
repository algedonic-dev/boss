//! A second completion of an already-completed step writes nothing.
//!
//! Measured on car 6b23d135, 2026-09-24T22:18:10Z (backlog 29a7ea09).
//! A direct PUT completed the car's `disproved` outcome terminal, and
//! `close_job_on_terminal` stamped `jobs.job.closed` with
//! `outcome=disproved` at .556307. The same terminal had gone READY a
//! moment earlier, so the dispatcher rule `complete-marker-on-step-ready`
//! PUT `{"status":"completed"}` onto it too. That second PUT read the
//! step AFTER the first had completed it — its .576623 row is a bare
//! `jobs.step.updated`, with no `jobs.step.completed` beside it — so it
//! was an idempotent re-send and changed nothing about the step. But it
//! still ran the rest of the handler: it rewrote the step row, re-ran
//! the re-evaluator, and ran the all-steps-terminal catch-all close,
//! which read the JOB before the first writer's close had committed,
//! saw it open, and wrote the whole row back as closed at .586476 —
//! with no `outcome`. Both closes are whole-row read-modify-write, so
//! the later commit erased the earlier one's key.
//!
//! A re-send that changes nothing writes nothing to the STEP: no step
//! row, no step event. On a CLOSED packet it records nothing at all.
//!
//! On a packet still OPEN it settles the packet (backlog 17bf6f3e,
//! 2026-10-10): the readiness pass runs again and a declared terminal
//! already completed closes the packet with its outcome. Rewriting the
//! row and running a catch-all that forgot the outcome was the 29a7ea09
//! defect; running NOTHING was the next one — four ops-requests were
//! left open on 2026-10-09 because the write each re-send repeated had
//! landed without its packet being settled, and the re-send was the
//! only write left that could do it.
//!
//! The first test reproduces the measured interleaving deterministically:
//! it stops the first writer between its step write and its job close
//! (the window the second writer's read fell into) by making the step
//! write through the repository, then sends the dispatcher's body.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{JobId, StepStatus};
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
use uuid::Uuid;

const KIND: &str = "a-marker-completed-twice";

/// One piece of work, then an `outcome` marker that is a declared
/// terminal — the shape of the ship-a-change `disproved` step: a
/// no-role, zero-duration marker the dispatcher auto-completes when it
/// goes ready, and which a direct writer may complete first.
fn spec() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        KIND,
        "A marker completed twice",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "work".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                title_template: "The work".into(),
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            },
            StepSpec {
                title: "disproved".into(),
                kind: "outcome".into(),
                ready_when: "steps.work.done".into(),
                title_template: "Disproved".into(),
                terminal: Some(Terminal {
                    outcome: "disproved".into(),
                }),
                ..Default::default()
            },
            // A second terminal that waits on a job-metadata key nothing
            // writes — ops-request's `nothing-to-do`. The re-evaluator
            // never skips a step whose predicate reads job metadata, so
            // only the terminal close moves it: while it is pending, the
            // all-steps-terminal catch-all cannot close the packet, and a
            // test of the terminal close is a test of that close alone.
            StepSpec {
                title: "nothing-to-do".into(),
                kind: "outcome".into(),
                ready_when: "steps.work.done AND job.metadata.plan_names_nothing".into(),
                title_template: "Nothing to do".into(),
                terminal: Some(Terminal {
                    outcome: "nothing-to-do".into(),
                }),
                ..Default::default()
            },
        ],
    );
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec
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

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-bootstrap-admin",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

struct Harness {
    app: axum::Router,
    jobs: Arc<InMemoryJobs>,
}

fn harness() -> Harness {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(spec()).expect("seed the kind");
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
    Harness {
        app: router(state),
        jobs,
    }
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn open_job(app: &axum::Router) -> JobId {
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": KIND,
                    "subject": { "subject_kind": "custom", "id": "/system/flow" },
                    "title": "A car whose marker is completed twice",
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
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    JobId::from_uuid(Uuid::parse_str(job["id"].as_str().expect("job id")).expect("uuid"))
}

async fn step(jobs: &InMemoryJobs, job_id: &JobId, slug: &str) -> boss_core::job::Step {
    jobs.list_steps(job_id)
        .await
        .expect("list steps")
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some(slug))
        .unwrap_or_else(|| panic!("no step `{slug}`"))
}

/// The dispatcher's own body (`jobs.complete_step`): the status alone.
async fn put_completed(
    app: &axum::Router,
    job_id: &JobId,
    step: &boss_core::job::Step,
) -> (StatusCode, serde_json::Value) {
    send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{}", step.id))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed" }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

fn events_for(jobs: &InMemoryJobs, job_id: &JobId) -> Vec<boss_core::event::Event> {
    let want = job_id.to_string();
    jobs.recorded_events()
        .into_iter()
        .filter(|e| {
            e.payload.get("job_id").and_then(|v| v.as_str()) == Some(want.as_str())
                || e.payload.get("id").and_then(|v| v.as_str()) == Some(want.as_str())
        })
        .collect()
}

/// The measured interleaving. The first writer has completed the
/// terminal and not yet closed the packet; the dispatcher's re-send
/// lands in that window. It writes nothing to the step — and it closes
/// the packet WITH the terminal's outcome.
///
/// This test used to assert that the re-send did not close at all,
/// because the close it ran then carried no `outcome` (29a7ea09). That
/// guarded the outcome by making the re-send inert, and an inert
/// re-send is what wedged four ops-requests on 2026-10-09 (backlog
/// 17bf6f3e): there the "first writer" never closed — its tail stopped
/// after the terminal close's skips — and the dispatcher's retry, the
/// one write that could still finish the job, answered 204 and did
/// nothing. The outcome is now kept by naming it: the re-send closes
/// through the declared-terminal close, and if the first writer's close
/// does arrive it finds the packet closed and no-ops (both name the
/// same outcome, through the same owned-keys close door).
#[tokio::test]
async fn a_resend_between_the_terminal_write_and_its_close_closes_the_packet_with_its_outcome() {
    let h = harness();
    let job_id = open_job(&h.app).await;

    let work = step(&h.jobs, &job_id, "work").await;
    let (status, body) = put_completed(&h.app, &job_id, &work).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "work completes: {body}");
    let marker = step(&h.jobs, &job_id, "disproved").await;
    assert_eq!(
        marker.status,
        StepStatus::Ready,
        "precondition: the marker goes ready behind the work"
    );

    // The first writer, stopped after its step write: the terminal is
    // completed on the row, the packet is not yet closed.
    let mut first = marker.clone();
    first.status = StepStatus::Completed;
    first.completed_on = Some(chrono::Utc::now().date_naive());
    h.jobs
        .update_step(&first)
        .await
        .expect("first writer's step write");

    let stored_marker = step(&h.jobs, &job_id, "disproved").await;

    // The dispatcher's re-send.
    let (status, body) = put_completed(&h.app, &job_id, &marker).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an idempotent re-send still answers 204: {body}"
    );

    let job_after = h.jobs.get_job(&job_id).await.unwrap().expect("job");
    assert_eq!(
        job_after.status,
        boss_core::job::JobStatus::Closed,
        "the re-send of a completed terminal on an open packet closes it \
         (metadata after: {})",
        job_after.metadata
    );
    assert_eq!(
        job_after.metadata["outcome"], "disproved",
        "the close names the terminal's outcome"
    );
    assert_eq!(
        step(&h.jobs, &job_id, "disproved").await,
        stored_marker,
        "the re-sent step itself is not rewritten"
    );
    assert_eq!(
        step(&h.jobs, &job_id, "nothing-to-do").await.status,
        StepStatus::Skipped,
        "the terminal close skips what was still open"
    );
}

/// SHAPE ONE OF BACKLOG 17bf6f3e: a step completed, and the readiness
/// pass that should have followed it never ran. ops-request e8dd5479's
/// `execute` completed at 2026-10-09T04:01:20Z with disposition
/// `answered`, and its log ends there — no promotion of `answered`, ever
/// — so the packet stayed open, and the hourly probe recheck, which
/// reads any open request as "already asked", stopped rechecking its
/// car for good. The first writer is stopped here exactly where that
/// log stops: after its step write, before any readiness pass. A
/// re-send of the same completion — the runner retries a write the
/// system of record did not answer — must run the pass it missed.
#[tokio::test]
async fn a_resend_of_a_completion_whose_readiness_pass_never_ran_opens_what_it_unblocks() {
    let h = harness();
    let job_id = open_job(&h.app).await;

    let work = step(&h.jobs, &job_id, "work").await;
    let mut first = work.clone();
    first.status = StepStatus::Completed;
    first.completed_on = Some(chrono::Utc::now().date_naive());
    h.jobs
        .update_step(&first)
        .await
        .expect("first writer's step write");
    assert_eq!(
        step(&h.jobs, &job_id, "disproved").await.status,
        StepStatus::Pending,
        "precondition: no readiness pass has run"
    );
    let stored_work = step(&h.jobs, &job_id, "work").await;

    let (status, body) = put_completed(&h.app, &job_id, &work).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the re-send answers 204: {body}"
    );

    assert_eq!(
        step(&h.jobs, &job_id, "disproved").await.status,
        StepStatus::Ready,
        "the re-send ran the readiness pass the first write missed"
    );
    assert_eq!(
        step(&h.jobs, &job_id, "work").await,
        stored_work,
        "the re-sent step itself is not rewritten"
    );
}

/// SHAPE TWO OF BACKLOG 17bf6f3e: a declared terminal completed and its
/// close never landed. ops-request 4a5ac945's `answered` completed at
/// 2026-10-09T20:00:43.039Z and the terminal close's skip loop ran at
/// .129 — and stopped short of `nothing-to-do`, with no
/// `jobs.job.closed` after it. The healing re-send does not have to
/// land on the terminal itself: the hourly recheck re-sends the
/// request's `execute`, the one step every request completes, so a
/// re-send of ANY completed step closes a packet whose declared terminal
/// has completed, with that terminal's outcome.
#[tokio::test]
async fn a_resend_of_any_completed_step_closes_a_packet_whose_terminal_completed() {
    let h = harness();
    let job_id = open_job(&h.app).await;

    let work = step(&h.jobs, &job_id, "work").await;
    let (status, body) = put_completed(&h.app, &job_id, &work).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "work completes: {body}");
    let marker = step(&h.jobs, &job_id, "disproved").await;
    let mut first = marker.clone();
    first.status = StepStatus::Completed;
    first.completed_on = Some(chrono::Utc::now().date_naive());
    h.jobs
        .update_step(&first)
        .await
        .expect("the terminal's step write, its close never run");

    let (status, body) = put_completed(&h.app, &job_id, &work).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the re-send answers 204: {body}"
    );

    let job = h.jobs.get_job(&job_id).await.unwrap().expect("job");
    assert_eq!(
        job.status,
        boss_core::job::JobStatus::Closed,
        "a packet whose terminal completed closes on a re-send (metadata: {})",
        job.metadata
    );
    assert_eq!(job.metadata["outcome"], "disproved");
}

/// A readiness pass that cannot read the packet's steps is not a
/// success. `reevaluate_pass` answered a failed steps read with a bare
/// `return false` — no log line, no error — so the step PUT said 204
/// over a packet whose readiness had not been evaluated, and the writer
/// had no reason to send it again. Now it answers 500, naming the
/// packet and that the step itself was written, so the writer retries,
/// and the retry is a re-send that runs the pass again.
#[tokio::test]
async fn a_readiness_pass_that_cannot_read_the_steps_is_not_a_success() {
    let h = harness();
    let job_id = open_job(&h.app).await;

    let work = step(&h.jobs, &job_id, "work").await;
    let mut first = work.clone();
    first.status = StepStatus::Completed;
    first.completed_on = Some(chrono::Utc::now().date_naive());
    h.jobs
        .update_step(&first)
        .await
        .expect("first writer's step write");
    h.jobs.fail_steps_read(&job_id);

    let (status, body) = put_completed(&h.app, &job_id, &work).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a pass that could not read is not a 204: {body}"
    );
    let words = body.as_str().unwrap_or_default();
    assert!(
        words.contains(&job_id.to_string()) && words.contains("send the same request again"),
        "the refusal names the packet and the repair: {words}"
    );
}

/// After the terminal's own close, the re-send is still a no-op: the
/// outcome stays, and nothing is written — not even the bare
/// `jobs.step.updated` the measured re-send left at .576623.
#[tokio::test]
async fn a_resend_after_the_close_records_nothing_and_keeps_the_outcome() {
    let h = harness();
    let job_id = open_job(&h.app).await;

    let work = step(&h.jobs, &job_id, "work").await;
    let (status, body) = put_completed(&h.app, &job_id, &work).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "work completes: {body}");
    let marker = step(&h.jobs, &job_id, "disproved").await;
    let (status, body) = put_completed(&h.app, &job_id, &marker).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the terminal completes: {body}"
    );

    let closed = h.jobs.get_job(&job_id).await.unwrap().expect("job");
    assert_eq!(closed.metadata["outcome"], "disproved", "precondition");
    let events_before = events_for(&h.jobs, &job_id).len();

    let (status, body) = put_completed(&h.app, &job_id, &marker).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the re-send answers 204: {body}"
    );

    let written: Vec<String> = events_for(&h.jobs, &job_id)
        .into_iter()
        .skip(events_before)
        .map(|e| e.kind)
        .collect();
    assert!(
        written.is_empty(),
        "a re-send that changes nothing must record nothing; it recorded {written:?}"
    );
    let after = h.jobs.get_job(&job_id).await.unwrap().expect("job");
    assert_eq!(after.metadata["outcome"], "disproved");
    assert_eq!(after, closed, "the packet row is untouched");
}
