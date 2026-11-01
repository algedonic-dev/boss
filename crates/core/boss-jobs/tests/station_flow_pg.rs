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

/// A step id in the log that is not a step's canonical id counts
/// nothing AND refuses nothing. The cube used to join on
/// `s.id::text = sid`, under which an upper-case or malformed id simply
/// matched no row; the key join (backlog 17959b76) keeps both answers —
/// a bare `sid::uuid` would turn the malformed one into a 500 for the
/// whole flow read, and would start counting the upper-case one.
#[tokio::test(flavor = "multi_thread")]
async fn a_step_id_that_is_not_canonical_counts_nothing_and_refuses_nothing() {
    // An id with hex LETTERS in it, so its upper-case spelling differs.
    const STEP_C: &str = "cccccccc-0000-4000-8000-00000000000c";
    let db = seeded().await;
    let repo = PgJobs::new(db.pool.clone());
    repo.add_step_at(&step(STEP_C, StepStatus::Ready), Utc::now(), &[])
        .await
        .unwrap();
    repo.record_events(&[
        step_event("step.ready.task", STEP_C),
        step_event("step.done.task", &STEP_C.to_uppercase()),
        step_event("step.done.task", "not-a-uuid"),
        step_event("step.ready.task", ""),
    ])
    .await
    .expect("events record");
    drain_outbox(&db.pool).await;

    let cells = repo
        .step_flow_cube(Utc::now() - Duration::hours(1))
        .await
        .expect("a malformed step id in the log must not refuse the read");
    assert_eq!(cells.len(), 1, "one shape of obligation was seeded");
    assert_eq!(cells[0].arrived, 3, "STEP_C arrived under its own id");
    assert_eq!(
        cells[0].served, 1,
        "the upper-case spelling of STEP_C is not STEP_C's id as the log writes it"
    );
}

/// The narrower window never counts what only the wider holds, and a
/// re-served step is one obligation in the wider. The windows split on
/// the log's own write instant — the newest `created_at` of the first
/// drain — so the boundary is the database's clock, not this process's.
/// (The two-window statement the IT map read, backlog 29f7e897, went
/// when the map moved to the membership basis, item ab09f1af car 2.)
#[tokio::test(flavor = "multi_thread")]
async fn a_narrower_window_counts_only_what_was_written_inside_it() {
    const STEP_C: &str = "cccccccc-0000-4000-8000-00000000000c";
    let db = seeded().await;
    let repo = PgJobs::new(db.pool.clone());
    let (boundary,): (chrono::DateTime<Utc>,) =
        sqlx::query_as("SELECT MAX(created_at) FROM audit_log")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    repo.add_step_at(&step(STEP_C, StepStatus::Completed), Utc::now(), &[])
        .await
        .unwrap();
    repo.record_events(&[
        // Served again after the boundary: one obligation in the wider
        // window, and the narrower window's only served step but one.
        step_event("step.done.task", STEP_B),
        step_event("step.ready.task", STEP_C),
        step_event("step.done.task", STEP_C),
    ])
    .await
    .expect("events record");
    drain_outbox(&db.pool).await;

    let wide = boundary - Duration::hours(1);
    let alone = [
        repo.step_flow_cube(boundary).await.unwrap(),
        repo.step_flow_cube(wide).await.unwrap(),
    ];

    let counts = |cells: &[boss_jobs::station_flow::FlowCell]| {
        assert_eq!(cells.len(), 1, "one shape of obligation: {cells:?}");
        (cells[0].arrived, cells[0].served)
    };
    assert_eq!(
        counts(&alone[0]),
        (1, 2),
        "after the boundary: C arrived; B and C served"
    );
    assert_eq!(
        counts(&alone[1]),
        (3, 2),
        "the whole log: A, B, C arrived; B, C served"
    );
    assert!(
        repo.step_flow_cube(boundary + Duration::hours(1))
            .await
            .unwrap()
            .is_empty(),
        "a window nothing was written in is no cells"
    );
}

/// The window reaches `steps` through its primary key. Measured live on
/// 2026-10-17 (backlog 17959b76): this read ran about 25 times a minute,
/// up to 51.9 s each, while job reads and COMMITs stalled beside it. The
/// join was `s.id::text = win.sid`, and no index on `steps` can answer a
/// cast of its key — so every execution read, joined and sorted EVERY
/// step and job however small the window (on a local replica at the
/// live cardinality: a parallel seq scan of 387k steps and 77k jobs,
/// ~460 MB of temp files and a JIT pass, for 28k window rows).
///
/// Hash and merge joins are switched off so the question put to the
/// planner is only whether the key CAN serve the join: under a text
/// cast it cannot, whatever the statistics, so this is not a cost
/// guess about one fixture.
#[tokio::test(flavor = "multi_thread")]
async fn the_window_reaches_steps_through_its_key() {
    let db = seeded().await;
    let mut tx = db.pool.begin().await.unwrap();
    for off in [
        "SET LOCAL enable_hashjoin = off",
        "SET LOCAL enable_mergejoin = off",
        "SET LOCAL enable_seqscan = off",
    ] {
        sqlx::query(off).execute(&mut *tx).await.unwrap();
    }
    let explain = format!(
        "EXPLAIN (FORMAT JSON) {}",
        boss_events::tail_http::STEP_FLOW_CUBE_SQL
    );
    let (plan,): (serde_json::Value,) = sqlx::query_as(&explain)
        .bind(Utc::now() - Duration::hours(1))
        .fetch_one(&mut *tx)
        .await
        .unwrap();

    fn keyed_scans(node: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
        if node.get("Relation Name").and_then(|v| v.as_str()) == Some("steps")
            && node.get("Index Name").and_then(|v| v.as_str()) == Some("steps_pkey")
            && node.get("Index Cond").is_some()
        {
            out.push(node.clone());
        }
        for child in node
            .get("Plans")
            .and_then(|p| p.as_array())
            .into_iter()
            .flatten()
        {
            keyed_scans(child, out);
        }
    }
    let mut found = Vec::new();
    keyed_scans(&plan[0]["Plan"], &mut found);
    assert!(
        !found.is_empty(),
        "steps must be probed by its key from the window, not read whole: {plan:#}"
    );
}
