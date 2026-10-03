//! Postgres contract for `step_flow_cube` — the storage half of the
//! station drain rate.
//!
//! Two implementations of one rule (the in-memory HTTP test carries
//! the other), so what is pinned here is the SQL: the join that
//! recovers the two coordinates the log's step payloads do not carry,
//! the wall-clock window, and the DISTINCT that keeps a re-promoted
//! step one obligation.
//!
//! The write path stages on the outbox and the relay drain moves rows
//! into `audit_log`, so this exercises the same two hops production
//! does. A read that skipped the drain would assert against rows
//! production does not have yet.

use std::sync::Arc;

use boss_core::event::Event;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_jobs::PgJobs;
use boss_jobs::port::JobsRepository;
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{Duration, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

const JOB: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const STEP_A: &str = "11111111-0000-4000-8000-000000000001";
const STEP_B: &str = "22222222-0000-4000-8000-000000000002";

async fn drain_outbox(pool: &PgPool) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 200)
        .await
        .expect("relay drain");
}

fn packet() -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            "backlog-item",
            Subject::new("custom", "bosspipeline"),
            "a packet with obligations",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        )
    }
}

fn step(id: &str, status: StepStatus) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        spec_slug: Some("triage".into()),
        status,
        metadata: serde_json::json!({ "authority_role": "platform-admin" }),
        ..Step::new(
            JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
            "task",
            "Measure the claim",
            0,
        )
    }
}

fn step_event(kind: &str, step_id: &str) -> Event {
    Event::new(
        "jobs",
        kind,
        serde_json::json!({ "job_id": JOB, "step_id": step_id, "kind": "task" }),
        Utc::now(),
    )
}

async fn seeded() -> TestDb {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let now = Utc::now();
    repo.create_job_at(&packet(), now, &[]).await.unwrap();
    repo.add_step_at(&step(STEP_A, StepStatus::Ready), now, &[])
        .await
        .unwrap();
    repo.add_step_at(&step(STEP_B, StepStatus::Completed), now, &[])
        .await
        .unwrap();
    repo.record_events(&[
        step_event("step.ready.task", STEP_A),
        // Re-promoted: a SECOND ready event on the same step. One
        // obligation, not two — the DISTINCT this test exists for.
        step_event("step.ready.task", STEP_A),
        step_event("step.ready.task", STEP_B),
        step_event("step.done.task", STEP_B),
        // Not a step transition: must not enter the cube at all.
        Event::new(
            "jobs",
            "jobs.job.updated",
            serde_json::json!({ "id": JOB }),
            Utc::now(),
        ),
    ])
    .await
    .expect("events record");
    drain_outbox(&db.pool).await;
    db
}

/// The join recovers the Job kind and the step's slug — the two
/// coordinates the log's `step.ready` payload does not carry — and a
/// step counted twice is still one obligation.
#[tokio::test(flavor = "multi_thread")]
async fn the_cube_carries_the_coordinates_the_payload_lacks() {
    let db = seeded().await;
    let repo = PgJobs::new(db.pool.clone());

    let cells = repo
        .step_flow_cube(Utc::now() - Duration::hours(1))
        .await
        .expect("cube reads");
    assert_eq!(cells.len(), 1, "one shape of obligation was seeded");
    let cell = &cells[0];
    assert_eq!(cell.job_kind, "backlog-item", "recovered from `jobs`");
    assert_eq!(cell.step_kind, "task");
    assert_eq!(cell.spec_slug, "triage", "recovered from `steps`");
    assert_eq!(cell.authority_role, "platform-admin");
    assert_eq!(cell.arrived, 2, "two steps arrived; one arrived twice");
    assert_eq!(cell.served, 1);
}

/// The window is a filter on the WALL-CLOCK write instant, not a hint:
/// a window that opens after the rows were written counts nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_opens_later_counts_nothing() {
    let db = seeded().await;
    let repo = PgJobs::new(db.pool.clone());

    let cells = repo
        .step_flow_cube(Utc::now() + Duration::hours(1))
        .await
        .expect("cube reads");
    assert!(cells.is_empty(), "nothing was written after the window");
}
