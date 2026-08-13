//! Postgres-level contract for the Job's admission-fixed `simulated`
//! flag: the column round-trips through the adapter, and the UPDATE
//! path cannot move it — the storage enforces the immutability rather
//! than trusting every caller to (the epoch trim in 03-jobs.sql
//! depends on a Job's rows all sharing one fate).

#![cfg(feature = "postgres")]

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::JobsRepository;
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn job(id: &str, simulated: bool) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "sale".to_string(),
        workflow_version: 1,
        subject: Subject::new("account", "prac-A"),
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        simulated,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn simulated_round_trips_and_update_cannot_flip_it() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let sim = job("00000000-0000-0000-0000-00000000a001", true);
    let real = job("00000000-0000-0000-0000-00000000a002", false);
    repo.create_job(&sim).await.unwrap();
    repo.create_job(&real).await.unwrap();

    let got_sim = repo.get_job(&sim.id).await.unwrap().unwrap();
    assert!(got_sim.simulated, "column round-trips true");
    let got_real = repo.get_job(&real.id).await.unwrap().unwrap();
    assert!(!got_real.simulated, "column round-trips false");

    // An update carrying a flipped flag must not move the column —
    // `simulated` is absent from the UPDATE's SET list by design.
    let mut flipped = got_sim.clone();
    flipped.simulated = false;
    flipped.title = "renamed".into();
    repo.update_job(&flipped).await.unwrap();

    let after = repo.get_job(&sim.id).await.unwrap().unwrap();
    assert_eq!(after.title, "renamed", "the update itself applied");
    assert!(
        after.simulated,
        "simulated is immutable at the storage layer"
    );
}
