//! The step-metadata merge takes the WRITER'S judgement (backlog
//! cd1f68c6, from Codex review 5d605949 R1 and the strike-release repair
//! run c76fff92).
//!
//! `PATCH /api/jobs/{id}/steps/{step_id}/metadata` judged a precondition
//! under the write's row lock only when the release door called it
//! in-process. On the wire it passed none, so a client that read a
//! review hold, decided on it, and wrote many calls later — `boss rerail
//! --finish` and the auto-park refresh moving `hold_sha` to a new head,
//! the strike release lifting a red-train hold — wrote over whatever
//! hold landed in between: a release left `hold_sha` beside no hold, and
//! a person's hold became bound to a head a reviewer could release.
//!
//! The writer now sends the keys it judged, as it read them, in
//! `boss_jobs::step_metadata_write::EXPECT_HEADER`. Pinned here through
//! the real router over BOTH adapters, which must agree: a judgement the
//! row no longer holds is 409 `step_changed` and writes neither the row
//! nor an event; one it holds lands; a merge with no judgement is exactly
//! today's; a judgement that cannot be read is refused, never dropped;
//! and the writer's judgement joins the door's own fence rather than
//! replacing it. The last test holds the Postgres row lock across the
//! handler's own read, so the judgement passes that read and is refused
//! only UNDER the lock — the case a re-read before the write cannot close.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use boss_core::{
    job::{Job, Priority, Step, StepStatus, Subject},
    port::EventBus,
    publisher::DomainPublisher,
};
use boss_jobs::step_metadata_write::{EXPECT_HEADER, STEP_CHANGED_CODE, expectation_header};
use boss_jobs::{
    InMemoryJobs, JobsRepository, PgJobs,
    http::{JobsApiState, router},
};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};
use tower::ServiceExt;

/// The hold the conductor's re-judge writes, prose and all: the header
/// must carry an em-dash intact.
const REVIEW_HOLD: &str =
    "conductor re-judge at 29943365 — waits for a review of this head (backlog b7b02024)";

fn app<R: JobsRepository + 'static>(jobs: Arc<R>) -> Router {
    let bus = RecordingEventBus::new();
    let publisher = DomainPublisher::new(bus.clone() as Arc<dyn EventBus>, "jobs");
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
    router(JobsApiState::minimal(
        jobs,
        bus,
        publisher,
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    ))
}

fn map(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => unreachable!("test judgements are objects"),
    }
}

/// A car at the dock: a ship-a-change packet whose review step holds
/// `review_md`.
async fn car<R: JobsRepository>(jobs: &R, review_md: Value) -> (Job, Step) {
    let job = Job::new(
        "ship-a-change",
        Subject::new("custom", "fix/x"),
        "A held car",
        "emp-owner",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
    );
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Open for review", 4);
    step.spec_slug = Some("review".into());
    step.status = StepStatus::Ready;
    step.metadata = review_md;
    jobs.add_step(&step).await.unwrap();
    (job, step)
}

fn held_at_a() -> Value {
    json!({"hold": REVIEW_HOLD, "hold_sha": "29943365", "agent_run": "7cb2438c"})
}

async fn patch(
    app: Router,
    job: &Job,
    step: &Step,
    body: Value,
    judged: Option<String>,
) -> (StatusCode, Value) {
    let user = User {
        id: "agent-claude-b".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    };
    let mut request = Request::builder()
        .method("PATCH")
        .uri(format!("/api/jobs/{}/steps/{}/metadata", job.id, step.id))
        .header("content-type", "application/json")
        .header("x-boss-user", serde_json::to_string(&user).unwrap());
    if let Some(judged) = judged {
        request = request.header(EXPECT_HEADER, judged);
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"text": String::from_utf8_lossy(&bytes)})),
    )
}

async fn events<R: JobsRepository>(jobs: &R, job: &Job) -> usize {
    jobs.events_for_job(&job.id, 1000).await.unwrap().len()
}

async fn metadata<R: JobsRepository>(jobs: &R, step: &Step) -> Value {
    jobs.get_step(&step.id).await.unwrap().unwrap().metadata
}

/// Every wire rule this door now has, against one adapter. Returns what
/// each case answered, so the two adapters are compared as a whole.
async fn the_rules<R: JobsRepository + 'static>(jobs: Arc<R>) -> Vec<String> {
    let mut answers = Vec::new();
    let refresh = json!({"hold_sha": "df139b70"});

    // 1. The refresh judged the hold at A, and a person's hold replaced
    //    it before the write: refused by name, and NOTHING is written.
    let (job, step) = car(jobs.as_ref(), held_at_a()).await;
    let judged = map(json!({"hold": "an older review hold", "hold_sha": "29943365"}));
    let before = (
        metadata(jobs.as_ref(), &step).await,
        events(jobs.as_ref(), &job).await,
    );
    let (status, body) = patch(
        app(jobs.clone()),
        &job,
        &step,
        refresh.clone(),
        Some(expectation_header(&judged)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], STEP_CHANGED_CODE, "{body}");
    assert_eq!(
        (
            metadata(jobs.as_ref(), &step).await,
            events(jobs.as_ref(), &job).await
        ),
        before,
        "a refused judgement writes neither the row nor an event"
    );
    answers.push(format!("stale:{status}"));

    // 2. A release lifted the hold in the window, leaving hold_sha: the
    //    refresh judged a hold that is gone, so it does not rebind one.
    let (job, step) = car(jobs.as_ref(), json!({"hold_sha": "29943365"})).await;
    let judged = map(json!({"hold": REVIEW_HOLD, "hold_sha": "29943365"}));
    let (status, body) = patch(
        app(jobs.clone()),
        &job,
        &step,
        refresh.clone(),
        Some(expectation_header(&judged)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        metadata(jobs.as_ref(), &step).await,
        json!({"hold_sha": "29943365"})
    );
    answers.push(format!("lifted:{status}"));

    // 3. The row still reads as judged — an unrelated key written since
    //    changes nothing the write rests on — and the write lands.
    let (job, step) = car(jobs.as_ref(), held_at_a()).await;
    let judged = map(json!({"hold": REVIEW_HOLD, "hold_sha": "29943365"}));
    let (status, body) = patch(
        app(jobs.clone()),
        &job,
        &step,
        refresh.clone(),
        Some(expectation_header(&judged)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let after = metadata(jobs.as_ref(), &step).await;
    assert_eq!(after["hold_sha"], "df139b70");
    assert_eq!(after["hold"], REVIEW_HOLD, "the reason stands");
    answers.push(format!("judged:{status}"));

    // 4. No judgement: exactly today's merge, whatever the row holds.
    let (job, step) = car(jobs.as_ref(), held_at_a()).await;
    let (status, body) = patch(app(jobs.clone()), &job, &step, refresh.clone(), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(metadata(jobs.as_ref(), &step).await["hold_sha"], "df139b70");
    answers.push(format!("unjudged:{status}"));

    // 5. A judgement that cannot be read is refused before anything is
    //    written — never dropped into an unjudged write.
    let (job, step) = car(jobs.as_ref(), held_at_a()).await;
    for unreadable in ["[\"hold\"]", "{\"hold\":", "null"] {
        let (status, body) = patch(
            app(jobs.clone()),
            &job,
            &step,
            refresh.clone(),
            Some(unreadable.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{unreadable}: {body}");
        answers.push(format!("unreadable:{status}"));
    }
    assert_eq!(metadata(jobs.as_ref(), &step).await, held_at_a());

    // 6. The writer's judgement JOINS the door's own fence. A reservation
    //    write on a review step is fenced by the server's read of the
    //    hold, hold_sha and releaser; a writer that judged hold_sha at
    //    another value names a row that cannot exist, and is refused.
    let (job, step) = car(jobs.as_ref(), json!({"hold_sha": "29943365"})).await;
    let (status, body) = patch(
        app(jobs.clone()),
        &job,
        &step,
        json!({"releaser": "automation:train-conductor"}),
        Some(expectation_header(&map(json!({"hold_sha": "df139b70"})))),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], STEP_CHANGED_CODE, "{body}");
    assert_eq!(
        metadata(jobs.as_ref(), &step).await,
        json!({"hold_sha": "29943365"})
    );
    answers.push(format!("joined:{status}"));
    answers
}

#[tokio::test(flavor = "multi_thread")]
async fn in_memory_and_postgres_judge_the_writers_judgement_alike() {
    let db = TestDb::new().await;
    let memory = the_rules(Arc::new(InMemoryJobs::new())).await;
    let postgres = the_rules(Arc::new(PgJobs::new(db.pool.clone()))).await;
    assert_eq!(memory, postgres);
}

/// UNDER THE LOCK, not at the handler's read. The judgement matches the
/// row when the handler reads it; a concurrent writer holding the row
/// lock then replaces the hold and commits; the write, which was waiting
/// on that lock, must see the new hold and refuse. A client re-read
/// before its write — the strike release's repair at 4dccbb95 — narrows
/// this window to one round trip and cannot close it; this does.
#[tokio::test(flavor = "multi_thread")]
async fn a_hold_replaced_while_the_write_waits_on_the_row_lock_is_not_rebound() {
    let db = TestDb::new().await;
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    let (job, step) = car(jobs.as_ref(), held_at_a()).await;
    let mut blocking = db.pool.begin().await.unwrap();
    let (pid,): (i32,) = sqlx::query_as("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocking)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM steps WHERE id = $1 FOR UPDATE")
        .bind(*step.id.inner().as_uuid())
        .fetch_one(&mut *blocking)
        .await
        .unwrap();
    let judged = map(json!({"hold": REVIEW_HOLD, "hold_sha": "29943365"}));
    let (request_app, request_job, request_step) = (app(jobs.clone()), job.clone(), step.clone());
    let pending = tokio::spawn(async move {
        patch(
            request_app,
            &request_job,
            &request_step,
            json!({"hold_sha": "df139b70"}),
            Some(expectation_header(&judged)),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (blocked,): (bool,) = sqlx::query_as(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                 WHERE $1 = ANY(pg_blocking_pids(pid)))",
            )
            .bind(pid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the judged write reached the row lock");
    // A person's hold, unbound, replaces the review hold meanwhile.
    sqlx::query(
        "UPDATE steps SET metadata = jsonb_set(metadata, '{hold}', '\"David reads it first\"') \
         WHERE id = $1",
    )
    .bind(*step.id.inner().as_uuid())
    .execute(&mut *blocking)
    .await
    .unwrap();
    blocking.commit().await.unwrap();
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], STEP_CHANGED_CODE, "{body}");
    let stored = metadata(jobs.as_ref(), &step).await;
    assert_eq!(stored["hold"], "David reads it first");
    assert_eq!(
        stored["hold_sha"], "29943365",
        "a person's hold is never bound to a head a reviewer could release"
    );
}
