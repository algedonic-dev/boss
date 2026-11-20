//! The packet budget, held to one contract on both `JobsRepository`
//! adapters (backlog 4f8783fb, design 6ab85352 car 1). The HTTP suite
//! (`a_packet_keeps_a_reserve_under_its_event_bound.rs`) drives the
//! in-memory adapter through the doors; production is answered by
//! `PgJobs`, whose writes are their own SQL and whose events go through
//! the outbox — so the same cases run against each, at the port.
//!
//! What both must do, judged under the row lock before anything is
//! staged: refuse a write that grows a packet whose state event would
//! reach three quarters of the event bound, and write nothing; admit a
//! write that shrinks it; admit a close at 99% of the bound; refuse a
//! close only when its event is over the bound itself, naming the repair
//! door, and leave the packet open.

use boss_core::{
    actor::ActorId,
    event::Event,
    job::{Job, JobStatus, Priority, Step, Subject},
    publisher::EventStamp,
};
use boss_jobs::packet_budget::{BOUND, BUDGET};
use boss_jobs::port::JobsError;
use boss_jobs::{InMemoryJobs, JobsRepository};
use serde_json::{Map, Value, json};

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("test".into()))
}

fn day() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap()
}

/// Metadata carrying one evidence value of `bytes` bytes.
fn heavy(bytes: usize) -> Value {
    json!({ "evidence": "e".repeat(bytes), "kept": true })
}

fn patch(key: &str, value: Value) -> Map<String, Value> {
    let mut patch = Map::new();
    patch.insert(key.into(), value);
    patch
}

async fn open(jobs: &dyn JobsRepository, metadata: Value) -> Job {
    let mut job = Job::new(
        "user-feedback",
        Subject::new("custom", "fixture"),
        "Evidence-heavy",
        "agent-test",
        Priority::Standard,
        day(),
    );
    job.status = JobStatus::Open;
    job.metadata = metadata;
    jobs.create_job(&job).await.unwrap();
    job
}

async fn stored(jobs: &dyn JobsRepository, job: &Job) -> Job {
    jobs.get_job(&job.id).await.unwrap().unwrap()
}

fn refused_growing(result: &Result<impl std::fmt::Debug, JobsError>, packet: &Job) -> bool {
    matches!(result, Err(JobsError::OverBudget(r))
        if !r.over_bound && r.packet == packet.id.to_string() && r.event_bytes >= BUDGET)
}

async fn close(jobs: &dyn JobsRepository, job: &Job) -> Result<Option<Job>, JobsError> {
    let markers = |_: &Job| -> Vec<Event> { Vec::new() };
    jobs.close_job_at(
        &job.id,
        day(),
        &patch("outcome", json!("done")),
        &stamp(),
        &markers,
    )
    .await
}

async fn budget_contract(jobs: &dyn JobsRepository) {
    // A merge that grows a packet at its budget: refused, nothing written.
    let full = open(jobs, heavy(BUDGET)).await;
    let refused = jobs
        .merge_job_metadata_at(&full.id, &patch("late", json!("note")), None, &stamp())
        .await;
    assert!(refused_growing(&refused, &full), "{refused:?}");
    assert_eq!(stored(jobs, &full).await.metadata, full.metadata);

    // A merge that shrinks it: admitted.
    let shrunk = jobs
        .merge_job_metadata_at(
            &full.id,
            &patch("evidence", json!("e".repeat(BUDGET - 1_000))),
            None,
            &stamp(),
        )
        .await;
    assert!(shrunk.is_ok(), "a shrinking write lands: {shrunk:?}");

    // An append only ever grows: refused at the budget.
    let appended = open(jobs, heavy(BUDGET)).await;
    let refused = jobs
        .append_step_correction_at(
            &appended.id,
            &json!({ "field": "evidence", "why": "late" }),
            &stamp(),
        )
        .await;
    assert!(refused_growing(&refused, &appended), "{refused:?}");

    // A whole-row write that grows: refused. The same row closed by a
    // whole-row write: admitted, the reserve is for exactly that.
    let row = open(jobs, heavy(BUDGET)).await;
    let mut grown = row.clone();
    grown.metadata["late"] = json!("note");
    let staged = [stamp().event(
        boss_jobs::events::JOB_UPDATED,
        serde_json::to_value(&grown).unwrap(),
    )];
    let refused = jobs
        .update_job_at(&grown, JobStatus::Open, chrono::Utc::now(), &staged)
        .await;
    assert!(refused_growing(&refused, &row), "{refused:?}");
    let mut closing = row.clone();
    closing.status = JobStatus::Closed;
    closing.closed_on = Some(day());
    closing.metadata["outcome"] = json!("done");
    let staged = [stamp().event(
        boss_jobs::events::JOB_UPDATED,
        serde_json::to_value(&closing).unwrap(),
    )];
    jobs.update_job_at(&closing, JobStatus::Open, chrono::Utc::now(), &staged)
        .await
        .expect("a hand close past the budget is admitted");
    assert_eq!(stored(jobs, &row).await.status, JobStatus::Closed);

    // A close at 99% of the bound: admitted.
    let near = open(jobs, heavy(BOUND / 100 * 99)).await;
    let closed = close(jobs, &near)
        .await
        .expect("a close at 99% is admitted");
    assert_eq!(closed.map(|j| j.status), Some(JobStatus::Closed));

    // A packet whose whole row is over the bound itself — 044b189b, which
    // grew there before the budget existed: its close was refused and it
    // stood open with every step terminal. It closes now, on the bounded
    // event (backlog b96df4f8), and keeps every key it held.
    let over = open(jobs, heavy(BOUND)).await;
    let closed = close(jobs, &over)
        .await
        .expect("a packet over the bound closes")
        .expect("it was open");
    assert_eq!(closed.status, JobStatus::Closed);
    let row = stored(jobs, &over).await;
    assert_eq!(row.status, JobStatus::Closed);
    assert_eq!(row.metadata["outcome"], json!("done"));
    assert_eq!(row.metadata["evidence"], over.metadata["evidence"]);
    assert_eq!(row.metadata["kept"], json!(true));

    // A merge that would carry a packet across the bound in one write is
    // refused, naming the packet and the size, and writes nothing.
    let under = open(jobs, heavy(BOUND / 2)).await;
    let refused = jobs
        .merge_job_metadata_at(
            &under.id,
            &patch("mailbox", json!("m".repeat(BOUND / 2))),
            None,
            &stamp(),
        )
        .await;
    match &refused {
        Err(JobsError::OverBudget(r)) => {
            assert!(r.over_bound, "{r}");
            assert!(r.event_bytes > BOUND, "{r}");
            let text = r.to_string();
            assert!(
                text.contains(&format!("job {}", under.id))
                    && text.contains(&r.event_bytes.to_string()),
                "{text}"
            );
        }
        other => panic!("a merge across the bound is refused by name: {other:?}"),
    }
    assert_eq!(stored(jobs, &under).await.metadata, under.metadata);

    // A step merge that grows its step event past the budget: refused,
    // naming the packet the step belongs to.
    let small = open(jobs, json!({})).await;
    let step = Step::new(small.id, "task", "work", 0);
    jobs.add_step(&step).await.unwrap();
    jobs.merge_step_metadata_at(
        &step.id,
        &patch("transcript", json!("t".repeat(BUDGET - 20_000))),
        &stamp(),
    )
    .await
    .expect("under the budget the step takes it");
    let refused = jobs
        .merge_step_metadata_at(
            &step.id,
            &patch("appendix", json!("a".repeat(40_000))),
            &stamp(),
        )
        .await;
    assert!(refused_growing(&refused, &small), "{refused:?}");
}

#[tokio::test]
async fn memory_keeps_a_reserve_under_the_event_bound() {
    budget_contract(&InMemoryJobs::new()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_keeps_a_reserve_under_the_event_bound() {
    let db = boss_testing::TestDb::new().await;
    budget_contract(&boss_jobs::PgJobs::new(db.pool.clone())).await;
}

/// A packet recorded in 044b189b's shape: its create — its last full-row
/// state event — encodes `under` bytes short of the bound, so the log
/// carries the whole row, and a close that adds more than `under` bytes
/// cannot carry it again. `opened_at` is stamped as the admission door
/// stamps it, so the live row and its replay agree on that column.
async fn recorded_near_the_bound(jobs: &boss_jobs::PgJobs, under: usize) -> Job {
    let s = stamp();
    let mut job = Job {
        opened_at: Some(chrono::Utc::now()),
        ..open_shape(heavy(BOUND / 2))
    };
    let created = |job: &Job| {
        s.event(
            boss_jobs::events::JOB_CREATED,
            serde_json::to_value(job).unwrap(),
        )
    };
    let measured = boss_events::outbox::encoded_len(&created(&job)).unwrap();
    job.metadata = heavy(BOUND / 2 + (BOUND - under) - measured);
    let event = created(&job);
    assert_eq!(
        boss_events::outbox::encoded_len(&event).unwrap(),
        BOUND - under,
        "fixture: the create is sized by the encoding the outbox judges"
    );
    jobs.create_job_at(&job, chrono::Utc::now(), &[event])
        .await
        .expect("the create rides under the bound");
    job
}

fn open_shape(metadata: Value) -> Job {
    let mut job = Job::new(
        "user-feedback",
        Subject::new("custom", "fixture"),
        "A mailbox",
        "agent-test",
        Priority::Standard,
        day(),
    );
    job.status = JobStatus::Open;
    job.metadata = metadata;
    job
}

/// Relay the outbox into audit_log, as production does.
async fn drain(pool: &sqlx::PgPool) {
    let bus: std::sync::Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &bus, 500)
        .await
        .expect("relay drain");
}

async fn logged_kinds(pool: &sqlx::PgPool, job: &Job) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM audit_log WHERE payload->>'id' = $1 ORDER BY id")
        .bind(job.id.to_string())
        .fetch_all(pool)
        .await
        .unwrap()
}

/// THE BOUNDED CLOSE IS ON THE RECORD (backlog b96df4f8). A packet over
/// the bound closes on `jobs.job.closed_over_bound`, which carries the
/// close's fields and the sha256 of the row it left — not the row — and
/// a rebuild from the log reproduces that row exactly, digest checked.
/// A log whose recorded digest the replay cannot reproduce is counted
/// by name, never passed in silence.
#[tokio::test(flavor = "multi_thread")]
async fn a_close_over_the_bound_is_recorded_as_a_delta_and_replays_to_its_row() {
    let db = boss_testing::TestDb::new().await;
    let jobs = boss_jobs::PgJobs::new(db.pool.clone());
    let job = recorded_near_the_bound(&jobs, 40).await;

    // The close adds far more than 40 bytes: its whole row cannot ride.
    let markers = |_: &Job| -> Vec<Event> { Vec::new() };
    jobs.close_job_at(
        &job.id,
        day(),
        &patch("outcome", json!("o".repeat(200))),
        &stamp(),
        &markers,
    )
    .await
    .expect("a packet over the bound closes")
    .expect("it was open");
    let live = stored(&jobs, &job).await;
    assert_eq!(live.status, JobStatus::Closed);

    drain(&db.pool).await;
    assert_eq!(
        logged_kinds(&db.pool, &job).await,
        ["jobs.job.created", "jobs.job.closed_over_bound"],
        "the close is on the record, as a delta"
    );
    let (payload, bytes): (Value, i32) = sqlx::query_as(
        "SELECT payload, octet_length(payload::text) FROM audit_log \
         WHERE kind = 'jobs.job.closed_over_bound'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(bytes < 4_096, "the delta carries no row: {bytes} bytes");
    let (sha, size) = boss_jobs::packet_budget::row_digest(&live);
    assert_eq!(payload["row_sha256"], json!(sha), "{payload}");
    assert_eq!(payload["row_bytes"], json!(size), "{payload}");

    let report = boss_jobs::rebuild_jobs_and_steps(&db.pool)
        .await
        .expect("rebuild");
    assert_eq!(report.bounded_closes_unreproduced, 0, "{report}");
    assert_eq!(
        stored(&jobs, &job).await,
        live,
        "the replay reproduces the row the close left"
    );

    // A recorded digest no replay can reach: counted, and named.
    let mut forged = payload.clone();
    forged["row_sha256"] = json!("0".repeat(64));
    sqlx::query(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload) \
         VALUES ($1, 'jobs.job.closed_over_bound', 'jobs', now(), $2)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(forged)
    .execute(&db.pool)
    .await
    .unwrap();
    let report = boss_jobs::rebuild_jobs_and_steps(&db.pool)
        .await
        .expect("rebuild");
    assert_eq!(report.bounded_closes_unreproduced, 1, "{report}");
}
