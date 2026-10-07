//! Real database parity, commit races and row/outbox rollback for the
//! conditional completion door. Every database is TestDb's isolated schema.
#![cfg(feature = "postgres")]

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
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

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

async fn seed<R: JobsRepository>(jobs: &R) -> (Job, Step) {
    let job = Job::new(
        "conditional-test",
        Subject::new("custom", "alarm"),
        "Triage",
        "emp-owner",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(),
    );
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Triage", 0);
    step.status = StepStatus::Ready;
    step.metadata = json!({"retained": "untouched"});
    jobs.add_step(&step).await.unwrap();
    (job, step)
}

fn body() -> Value {
    json!({"operation_id": "733bdce8-d878-44d6-8e4f-ea3d0c1889af",
        "expected": {"status": "ready", "assignee_id": null},
        "evidence": {"finding": "Recovered"}})
}

async fn post(app: Router, job: &Job, step: &Step, body: Value) -> (StatusCode, Value) {
    let user = User {
        id: "automation:observer".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    };
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/jobs/{}/steps/{}/complete-if",
                    job.id, step.id
                ))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&user).unwrap())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
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

async fn parity<R: JobsRepository + 'static>(jobs: Arc<R>) -> Vec<String> {
    let (job, step) = seed(jobs.as_ref()).await;
    let app = app(jobs.clone());
    let (status, completed) = post(app.clone(), &job, &step, body()).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["outcome"], "completed");
    let event_id = serde_json::from_value(completed["receipt"]["event_id"].clone()).unwrap();
    let event = jobs.recorded_event(event_id).await.unwrap().unwrap();
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    let record: boss_jobs::conditional_completion::RecordedCompletion =
        serde_json::from_value(event.payload["_conditional_completion"].clone()).unwrap();
    assert!(
        record.authenticated_by(&event, &stored),
        "receipt={record:?}, event={event:?}, stored={stored:?}"
    );
    assert_eq!(stored.metadata["retained"], "untouched");
    assert_eq!(stored.metadata["finding"], "Recovered");
    assert!(stored.metadata.get("_conditional_completion").is_none());
    let (status, replay) = post(app.clone(), &job, &step, body()).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["outcome"], "replayed");
    assert_eq!(completed["receipt"], replay["receipt"]);
    assert_eq!(
        jobs.recorded_event(event_id)
            .await
            .unwrap()
            .unwrap()
            .payload,
        event.payload
    );
    let mut conflict = body();
    conflict["evidence"]["finding"] = json!("Replacement");
    let (status, conflict) = post(app, &job, &step, conflict).await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["outcome"], "terminal_conflict");
    assert_eq!(
        jobs.recorded_event(event_id)
            .await
            .unwrap()
            .unwrap()
            .payload,
        event.payload
    );
    vec![
        completed["outcome"].to_string(),
        replay["outcome"].to_string(),
        conflict["outcome"].to_string(),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn in_memory_and_postgres_return_the_same_completion_and_receipt_contract() {
    let db = TestDb::new().await;
    let memory = parity(Arc::new(InMemoryJobs::new())).await;
    let postgres = parity(Arc::new(PgJobs::new(db.pool.clone()))).await;
    assert_eq!(memory, postgres);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_postgres_claim_inside_the_commit_window_refuses_evidence_and_events() {
    let db = TestDb::new().await;
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    let (job, step) = seed(jobs.as_ref()).await;
    let before = jobs.events_for_job(&job.id, 100).await.unwrap().len();
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
    let request_app = app(jobs.clone());
    let request_job = job.clone();
    let request_step = step.clone();
    let pending =
        tokio::spawn(async move { post(request_app, &request_job, &request_step, body()).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (blocked,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))")
                .bind(pid).fetch_one(&db.pool).await.unwrap();
            if blocked { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("conditional writer reached the actual row lock");
    sqlx::query("UPDATE steps SET status = 'active', assignee_id = 'emp-reviewer' WHERE id = $1")
        .bind(*step.id.inner().as_uuid())
        .execute(&mut *blocking)
        .await
        .unwrap();
    blocking.commit().await.unwrap();
    let (status, answer) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["outcome"], "precondition_failed");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);
    assert_eq!(stored.assignee_id.as_deref(), Some("emp-reviewer"));
    assert_eq!(stored.metadata, step.metadata);
    assert_eq!(
        jobs.events_for_job(&job.id, 100).await.unwrap().len(),
        before
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn outbox_failure_rolls_back_completion_evidence_and_receipt_together() {
    let db = TestDb::new().await;
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    let (job, step) = seed(jobs.as_ref()).await;
    sqlx::query("CREATE FUNCTION refuse_conditional_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'isolated causal outbox failure'; END $$")
        .execute(&db.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER refuse_conditional_outbox BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION refuse_conditional_outbox()")
        .execute(&db.pool).await.unwrap();
    let (status, answer) = post(app(jobs.clone()), &job, &step, body()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{answer}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Ready);
    assert_eq!(stored.metadata, step.metadata);
    assert!(jobs.events_for_job(&job.id, 100).await.unwrap().is_empty());
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM event_outbox WHERE payload->>'job_id' = $1 OR payload->>'id' = $1",
    )
    .bind(job.id.to_string())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
    sqlx::query("DROP TRIGGER refuse_conditional_outbox ON event_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    let (status, completed) = post(app(jobs.clone()), &job, &step, body()).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["outcome"], "completed");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_equal_competing_operations_return_one_original_receipt() {
    let db = TestDb::new().await;
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    let (job, step) = seed(jobs.as_ref()).await;
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
    let mut pending = vec![];
    for _ in 0..2 {
        let app = app(jobs.clone());
        let job = job.clone();
        let step = step.clone();
        pending.push(tokio::spawn(
            async move { post(app, &job, &step, body()).await },
        ));
    }
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (count,): (i64,) = sqlx::query_as("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) UNION SELECT activity.pid FROM pg_stat_activity activity JOIN blocked ON blocked.pid = ANY(pg_blocking_pids(activity.pid))) SELECT count(*) FROM blocked")
                .bind(pid).fetch_one(&db.pool).await.unwrap();
            if count == 2 { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("both conditional writers reached the actual row lock");
    blocking.commit().await.unwrap();
    let mut replies = vec![];
    for pending in pending {
        replies.push(pending.await.unwrap());
    }
    for (status, answer) in &replies {
        assert_eq!(*status, StatusCode::OK, "{answer}");
    }
    let mut outcomes = replies
        .iter()
        .map(|(_, answer)| answer["outcome"].as_str().unwrap())
        .collect::<Vec<_>>();
    outcomes.sort();
    assert_eq!(outcomes, ["completed", "replayed"]);
    assert_eq!(replies[0].1["receipt"], replies[1].1["receipt"]);
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM event_outbox WHERE kind = 'jobs.step.updated' AND payload->>'step_id' = $1")
        .bind(step.id.to_string()).fetch_one(&db.pool).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn receipt_survives_relay_and_outbox_retention_without_recreating_events() {
    let db = TestDb::new().await;
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    let (job, step) = seed(jobs.as_ref()).await;
    let app = app(jobs.clone());
    let (status, first) = post(app.clone(), &job, &step, body()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let id: uuid::Uuid = serde_json::from_value(first["receipt"]["event_id"].clone()).unwrap();
    let staged = jobs.recorded_event(id).await.unwrap().unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    let drained = boss_events::outbox::drain_outbox_once(&db.pool, &bus, 100)
        .await
        .unwrap();
    assert!(drained.delivered > 0 && drained.delivered < 100);
    assert_eq!(
        jobs.recorded_event(id).await.unwrap().unwrap().payload,
        staged.payload
    );
    sqlx::query("DELETE FROM event_outbox WHERE delivered_at IS NOT NULL")
        .execute(&db.pool)
        .await
        .unwrap();
    let before = jobs.events_for_job(&job.id, 100).await.unwrap();
    assert!(!before.is_empty() && before.len() < 100);
    let (status, replay) = post(app, &job, &step, body()).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(first["receipt"], replay["receipt"]);
    assert_eq!(
        jobs.events_for_job(&job.id, 100).await.unwrap().len(),
        before.len()
    );
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicting_committed_receipt_copies_are_unavailable_not_arbitrarily_chosen() {
    let db = TestDb::new().await;
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    let (job, step) = seed(jobs.as_ref()).await;
    let app = app(jobs.clone());
    let (status, first) = post(app.clone(), &job, &step, body()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 100)
        .await
        .unwrap();
    let id: uuid::Uuid = serde_json::from_value(first["receipt"]["event_id"].clone()).unwrap();
    // A corruption fixture in this isolated database, not a production write.
    sqlx::query("UPDATE event_outbox SET payload = payload || '{\"diagnostic_corruption\":true}'::jsonb WHERE event_id = $1")
        .bind(id).execute(&db.pool).await.unwrap();
    assert!(jobs.recorded_event(id).await.is_err());
    let before = jobs.get_step(&step.id).await.unwrap().unwrap();
    let (status, answer) = post(app, &job, &step, body()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert!(!answer.to_string().contains("diagnostic_corruption"));
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata,
        before.metadata
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuilt_completion_keeps_the_same_immutable_receipt_and_provenance() {
    let db = TestDb::new().await;
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    let (job, step) = seed(jobs.as_ref()).await;
    let now = chrono::Utc::now();
    jobs.record_events(&[
        boss_core::event::Event::new(
            "jobs",
            boss_jobs::events::JOB_CREATED,
            serde_json::to_value(&job).unwrap(),
            now,
        ),
        boss_core::event::Event::new(
            "jobs",
            boss_jobs::events::STEP_CREATED,
            serde_json::to_value(&step).unwrap(),
            now,
        ),
    ])
    .await
    .unwrap();
    let app = app(jobs.clone());
    let (status, first) = post(app.clone(), &job, &step, body()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    let drained = boss_events::outbox::drain_outbox_once(&db.pool, &bus, 100)
        .await
        .unwrap();
    assert!(drained.delivered > 0 && drained.delivered < 100);
    let report = boss_jobs::rebuild::rebuild_jobs_and_steps(&db.pool)
        .await
        .unwrap();
    // The completion topic is a marker; STEP_UPDATED is its whole
    // projection state, so the native rebuilder deliberately skips it.
    assert_eq!(report.events_skipped, 1, "{report}");
    assert_eq!(report.steps_inserted, 1, "{report}");
    assert_eq!(report.steps_updated, 1, "{report}");
    let rebuilt = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&rebuilt).unwrap(),
        serde_json::to_value(&stored).unwrap()
    );
    let before = jobs.events_for_job(&job.id, 100).await.unwrap();
    let (status, replay) = post(app, &job, &step, body()).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(first["receipt"], replay["receipt"]);
    assert_eq!(
        jobs.events_for_job(&job.id, 100).await.unwrap().len(),
        before.len()
    );
}
