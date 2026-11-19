//! A step write computed from a stale read erases nothing.
//!
//! Measured on run 6b6fe011, 2026-09-25 (backlog e381689d). `boss
//! dispatch` merged `prompt_bytes` onto the run's `briefed` step through
//! the merge door: 204, and the `jobs.step.updated` it recorded at
//! 11:01:14.649Z carries the key. At 11:01:14.651Z the dispatcher's
//! assignment PUT — body `{"assignee_id": …}` and nothing else — wrote
//! the step again, with metadata `{authority_role}` only, and answered
//! 204 too. The PUT handler is read-modify-write: it had read the step a
//! moment before the merge committed, overlaid the body, and wrote the
//! WHOLE row back. The key was gone, both writers had been told success,
//! and the CLI's status-only completion was then refused
//! "prompt_bytes: required field missing".
//!
//! The first test reproduces that interleaving deterministically through
//! the real handler: the in-memory adapter lands the merge straight after
//! the handler's read (`merge_after_next_read`). The rest pin the port
//! door every read-modify-write writer now goes through, on this adapter;
//! `a_stale_step_write_erases_nothing_pg.rs` pins the same body of
//! assertions against the SQL.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::port::JobsError;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

fn dispatcher() -> User {
    User {
        id: "automation:dispatcher".to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("platform".into()),
    }
}

fn run_packet(id: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            "agent-run",
            Subject::new("custom", "run"),
            "A run whose briefed step is written twice at once",
            "emp-1",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
        )
    }
}

fn map(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("test patches are objects"),
    }
}

fn build_app() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
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
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        publisher,
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs)
}

/// The run's `briefed` step as it was materialized: ready, unassigned,
/// carrying only its `authority_role`.
async fn briefed(jobs: &InMemoryJobs, job_id: &str) -> (Job, Step) {
    let j = run_packet(job_id);
    jobs.create_job(&j).await.unwrap();
    let mut step = Step::new(j.id, "task", "Briefed", 1);
    step.status = StepStatus::Ready;
    step.metadata = serde_json::json!({ "authority_role": "platform-admin" });
    jobs.add_step(&step).await.unwrap();
    (j, step)
}

async fn put_step(app: &Router, job: &Job, step: &Step, body: &str) -> (StatusCode, String) {
    put_step_judged(app, job, step, body, None).await
}

/// [`put_step`] carrying the writer's judgement in
/// [`boss_jobs::step_metadata_write::EXPECT_HEADER`].
async fn put_step_judged(
    app: &Router,
    job: &Job,
    step: &Step,
    body: &str,
    judged: Option<&str>,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method("PUT")
        .uri(format!("/api/jobs/{}/steps/{}", job.id, step.id))
        .header("content-type", "application/json")
        .header("x-boss-user", serde_json::to_string(&dispatcher()).unwrap());
    if let Some(judged) = judged {
        request = request.header(boss_jobs::step_metadata_write::EXPECT_HEADER, judged);
    }
    let resp = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn an_assignment_put_racing_a_merge_is_refused_and_the_merged_key_stands() {
    let (app, jobs) = build_app();
    let (job, step) = briefed(&jobs, "00000000-0000-0000-0000-00000000e381").await;
    let events_before = jobs.recorded_events().len();

    // The merge door commits between the handler's read and its write.
    jobs.merge_after_next_read(
        &step.id,
        map(serde_json::json!({ "prompt_bytes": "36127" })),
    );
    let (status, body) = put_step(&app, &job, &step, r#"{"assignee_id":"agent-claude"}"#).await;

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a write computed from a read the row no longer matches must not answer success: {body}"
    );
    // The refusal names why in words, and names ITSELF by a code a
    // caller matches — never by the words, which are free to move
    // (backlog 2aa2b19e).
    let said: serde_json::Value = serde_json::from_str(&body).expect("the 409 body is JSON");
    assert_eq!(
        said["error"],
        boss_jobs::step_metadata_write::STEP_CHANGED_ERROR,
        "the refusal names why: {body}"
    );
    assert_eq!(
        said["code"],
        boss_jobs::step_metadata_write::STEP_CHANGED_CODE,
        "the refusal carries the code a resending caller matches: {body}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        stored.metadata["prompt_bytes"], "36127",
        "the merged key is still stored"
    );
    assert_eq!(stored.assignee_id, None, "the refused write wrote nothing");
    assert_eq!(
        jobs.recorded_events().len(),
        events_before,
        "and recorded nothing"
    );

    // The re-send reads the row afresh and lands, keeping the key.
    let (status, body) = put_step(&app, &job, &step, r#"{"assignee_id":"agent-claude"}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some("agent-claude"));
    assert_eq!(stored.metadata["prompt_bytes"], "36127");
}

#[tokio::test]
async fn the_port_refuses_a_write_whose_read_the_row_no_longer_holds() {
    let jobs = InMemoryJobs::new();
    let (_job, step) = briefed(&jobs, "00000000-0000-0000-0000-00000000e382").await;
    let (read, version) = jobs.get_step_versioned(&step.id).await.unwrap().unwrap();
    jobs.merge_step_metadata_at(
        &step.id,
        &map(serde_json::json!({ "prompt_bytes": "36127" })),
        &boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("test".into()),
        ),
    )
    .await
    .unwrap();

    let mut stale = read.clone();
    stale.assignee_id = Some("agent-claude".into());
    let answer = jobs
        .update_step_if_unchanged_at(&stale, version, chrono::Utc::now(), &[])
        .await;
    assert!(
        matches!(answer, Err(JobsError::StepChanged { id }) if id == step.id),
        "a stale write is refused by name, got {answer:?}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.metadata["prompt_bytes"], "36127");
    assert_eq!(stored.assignee_id, None);
}

#[tokio::test]
async fn the_port_writes_when_the_row_still_holds_the_read() {
    let jobs = InMemoryJobs::new();
    let (_job, step) = briefed(&jobs, "00000000-0000-0000-0000-00000000e383").await;
    let (read, version) = jobs.get_step_versioned(&step.id).await.unwrap().unwrap();
    let mut next = read.clone();
    next.assignee_id = Some("agent-claude".into());
    next.metadata["note"] = serde_json::json!("written");
    jobs.update_step_if_unchanged_at(&next, version, chrono::Utc::now(), &[])
        .await
        .unwrap();
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some("agent-claude"));
    assert_eq!(stored.metadata["note"], "written");
}

#[tokio::test]
async fn the_port_refuses_a_stale_write_over_a_row_that_went_terminal() {
    // This was `the_port_does_not_judge_a_terminal_row` until backlog
    // 6ec22d71: the write answered Ok, the row kept its frozen values,
    // and the event the caller passed was recorded carrying the stale
    // ones — which the rebuild replays. A row read before it finished is
    // now refused like any other moved row; the idempotent re-send the
    // dispatcher's redeliveries rely on reads afresh and still lands
    // (`a_stale_step_write_is_judged_on_the_whole_row.rs`).
    let jobs = InMemoryJobs::new();
    let (_job, step) = briefed(&jobs, "00000000-0000-0000-0000-00000000e384").await;
    let (_, version) = jobs.get_step_versioned(&step.id).await.unwrap().unwrap();
    let mut done = jobs.get_step(&step.id).await.unwrap().unwrap();
    done.status = StepStatus::Completed;
    done.metadata["prompt_bytes"] = serde_json::json!("36128");
    jobs.update_step(&done).await.unwrap();

    let mut stale = step.clone();
    stale.status = StepStatus::Completed;
    let answer = jobs
        .update_step_if_unchanged_at(&stale, version, chrono::Utc::now(), &[])
        .await;
    assert!(
        matches!(answer, Err(JobsError::StepChanged { id }) if id == step.id),
        "got {answer:?}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.metadata["prompt_bytes"], "36128", "frozen");
}

/// A STEP PUT LANDS ONLY ON THE METADATA ITS WRITER JUDGED (review
/// e296014b R2 on car c18bf126, backlog 0c55e31d). `boss dispatch
/// --started` hands a claim back READY when the worker door refuses —
/// unless a worker binding was recorded for that claim, and a concurrent
/// `--started` can record one after every read the hand-back made. The
/// binder writes the binding onto the claim's own step, and the hand-back
/// PUT carries `{worker_session: null}` in the same header the merge door
/// reads (cd1f68c6). The PUT judges it on the row it read, and its version
/// compare freezes that row through the write, so the judgement holds AT
/// the write, not at a read before it.
#[tokio::test]
async fn a_put_lands_only_on_the_metadata_its_writer_judged() {
    let (app, jobs) = build_app();
    let binding = serde_json::json!({
        "worker_session": {"run": "r", "session": "dev-build-fedcba9876543210"}
    });
    let judged = boss_jobs::step_metadata_write::expectation_header(&map(
        serde_json::json!({ "worker_session": null }),
    ));
    let body = r#"{"assignee_id":"agent-claude"}"#;

    // 1. The row records the key the writer judged absent: refused by
    //    code, and nothing is written or recorded.
    let (job, step) = briefed(&jobs, "00000000-0000-0000-0000-00000000e385").await;
    jobs.merge_step_metadata_at(
        &step.id,
        &map(binding.clone()),
        &boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("test".into()),
        ),
    )
    .await
    .unwrap();
    let events_before = jobs.recorded_events().len();
    let (status, said) = put_step_judged(&app, &job, &step, body, Some(&judged)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    let said: serde_json::Value = serde_json::from_str(&said).expect("the 409 body is JSON");
    assert_eq!(
        said["code"],
        boss_jobs::step_metadata_write::STEP_CHANGED_CODE
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id, None, "the refused write wrote nothing");
    assert_eq!(jobs.recorded_events().len(), events_before);

    // 2. The binding lands after the handler's read: the judgement passed
    //    that read, and the version compare refuses the write.
    let (job, step) = briefed(&jobs, "00000000-0000-0000-0000-00000000e386").await;
    jobs.merge_after_next_read(&step.id, map(binding.clone()));
    let (status, said) = put_step_judged(&app, &job, &step, body, Some(&judged)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id, None);
    assert_eq!(stored.metadata["worker_session"], binding["worker_session"]);

    // 3. The row still reads as judged: the write lands.
    let (job, step) = briefed(&jobs, "00000000-0000-0000-0000-00000000e387").await;
    let (status, said) = put_step_judged(&app, &job, &step, body, Some(&judged)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{said}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some("agent-claude"));

    // 4. A judgement that cannot be read is refused, never dropped into
    //    the unjudged write it asked not to be.
    let (job, step) = briefed(&jobs, "00000000-0000-0000-0000-00000000e388").await;
    let (status, said) = put_step_judged(&app, &job, &step, body, Some("not json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{said}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id, None);
}
