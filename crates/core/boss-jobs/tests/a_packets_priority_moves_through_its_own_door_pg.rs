//! The priority door's write, held to one contract on both
//! `JobsRepository` adapters (backlog eb26059d): the HTTP suite
//! (`a_packets_priority_moves_through_its_own_door.rs`) drives the
//! in-memory adapter, and production is answered by `PgJobs`, whose
//! write is its own SQL — so the same cases run against each.
//!
//! What both must do: set the column and append the record in one
//! write, `from` read off the row at write time and `by` off the stamp;
//! keep every other metadata key; write nothing for the priority the
//! row already has; refuse a finished row; refuse to append to a stored
//! value that is not a list rather than overwrite it.

use boss_core::{
    actor::ActorId,
    job::{Job, JobStatus, Priority, Subject},
    publisher::EventStamp,
};
use boss_jobs::port::JobsError;
use boss_jobs::{InMemoryJobs, JobsRepository};
use serde_json::json;

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("test".into()))
}

async fn open(jobs: &dyn JobsRepository, metadata: serde_json::Value) -> Job {
    let mut job = Job::new(
        "user-feedback",
        Subject::new("custom", "fixture"),
        "Priority",
        "agent-test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 18).unwrap(),
    );
    job.status = JobStatus::Open;
    job.metadata = metadata;
    jobs.create_job(&job).await.unwrap();
    job
}

async fn priority_contract(jobs: &dyn JobsRepository) {
    let job = open(jobs, json!({"kept": true})).await;
    let moved = jobs
        .set_priority_at(&job.id, Priority::Urgent, "blocked", &stamp())
        .await
        .unwrap();
    assert!(moved.changed);
    assert_eq!(moved.from, Priority::Standard);
    let stored = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(stored.priority, Priority::Urgent);
    assert_eq!(stored, moved.job, "the answer is the row");
    assert_eq!(stored.metadata["kept"], true, "no other key moves");
    let list = stored.metadata["priority_changes"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["from"], "standard");
    assert_eq!(list[0]["to"], "urgent");
    assert_eq!(list[0]["by"], stamp().actor().to_string());
    assert_eq!(list[0]["why"], "blocked");

    // The priority it already has: nothing written.
    let same = jobs
        .set_priority_at(&job.id, Priority::Urgent, "again", &stamp())
        .await
        .unwrap();
    assert!(!same.changed);
    assert_eq!(jobs.get_job(&job.id).await.unwrap().unwrap(), stored);

    // A finished packet: refused, nothing written.
    let mut closed = stored.clone();
    closed.status = JobStatus::Closed;
    jobs.update_job_at(&closed, JobStatus::Open, chrono::Utc::now(), &[])
        .await
        .unwrap();
    let before = jobs.get_job(&job.id).await.unwrap().unwrap();
    let refused = jobs
        .set_priority_at(&job.id, Priority::Emergency, "late", &stamp())
        .await;
    assert!(
        matches!(&refused, Err(JobsError::TerminalJob { status, .. }) if status == "closed"),
        "{refused:?}"
    );
    assert_eq!(jobs.get_job(&job.id).await.unwrap().unwrap(), before);

    // A stored value the door did not write is not overwritten.
    let odd = open(jobs, json!({"priority_changes": "hand-typed"})).await;
    assert!(
        jobs.set_priority_at(&odd.id, Priority::Urgent, "w", &stamp())
            .await
            .is_err()
    );
    let kept = jobs.get_job(&odd.id).await.unwrap().unwrap();
    assert_eq!(kept.priority, Priority::Standard);
    assert_eq!(kept.metadata["priority_changes"], "hand-typed");
}

#[tokio::test]
async fn memory_moves_a_priority_on_the_record() {
    priority_contract(&InMemoryJobs::new()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_moves_a_priority_on_the_record() {
    let db = boss_testing::TestDb::new().await;
    priority_contract(&boss_jobs::PgJobs::new(db.pool.clone())).await;
}
