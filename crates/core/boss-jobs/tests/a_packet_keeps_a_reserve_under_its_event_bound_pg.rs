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

    // A close whose event is over the bound itself: refused, naming the
    // repair door, and the packet stays open.
    let over = open(jobs, heavy(BOUND)).await;
    let refused = close(jobs, &over).await;
    match &refused {
        Err(JobsError::OverBudget(r)) => {
            assert!(r.over_bound, "{r}");
            assert!(
                r.to_string()
                    .contains(&format!("boss job externalise {}", over.id)),
                "{r}"
            );
        }
        other => panic!("a close over the bound is refused by name: {other:?}"),
    }
    assert_eq!(stored(jobs, &over).await.status, JobStatus::Open);

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
