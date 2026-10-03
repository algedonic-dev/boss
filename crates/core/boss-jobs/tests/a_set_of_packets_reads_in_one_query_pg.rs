//! `JobsRepository::get_jobs` against Postgres: the packets a set of ids
//! names, read in one query.
//!
//! WHY IT EXISTS (review of backlog 046832d3's first cut, 2026-09-25,
//! finding #6). A collection read cut to the caller's scope — the
//! assignments queue, the refused-write table — judged each row's
//! packet with one `get_job` per distinct packet, and `all_assigned`
//! answers up to 50,000 rows. The scope cut now reads the set at once;
//! this pins the Postgres override to the default's answer: every id
//! that names a packet comes back, an id that names none is simply
//! absent, and a duplicate id is one packet.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::JobsRepository;
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn id(s: &str) -> JobId {
    JobId::from_uuid(Uuid::parse_str(s).expect("uuid"))
}

fn job(job_id: &str, owner: &str) -> Job {
    Job {
        id: id(job_id),
        status: JobStatus::Open,
        metadata: serde_json::Value::Null,
        ..Job::new(
            "brew-day".to_string(),
            Subject::new("asset", "FV-1"),
            format!("packet {job_id}"),
            owner,
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 25).expect("day"),
        )
    }
}

const A: &str = "0f000001-0000-0000-0000-000000000000";
const B: &str = "0f000002-0000-0000-0000-000000000000";
const C: &str = "0f000003-0000-0000-0000-000000000000";
const ABSENT: &str = "0f0000ff-0000-0000-0000-000000000000";

#[tokio::test(flavor = "multi_thread")]
async fn a_set_of_packets_reads_in_one_query_in_postgres() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    for (j, owner) in [(A, "emp-a"), (B, "emp-b"), (C, "emp-c")] {
        repo.create_job(&job(j, owner)).await.expect("create");
    }

    let mut got = repo
        .get_jobs(&[id(A), id(C), id(ABSENT), id(A)])
        .await
        .expect("read");
    got.sort_by_key(|j| j.id.to_string());
    let owners: Vec<&str> = got.iter().map(|j| j.owner_id.as_str()).collect();
    assert_eq!(owners, ["emp-a", "emp-c"], "{got:?}");

    // The whole row, the same as the single read hands back.
    let single = repo.get_job(&id(A)).await.expect("read").expect("A");
    assert_eq!(
        serde_json::to_value(&got[0]).expect("json"),
        serde_json::to_value(&single).expect("json"),
    );

    assert!(repo.get_jobs(&[]).await.expect("empty").is_empty());
}
