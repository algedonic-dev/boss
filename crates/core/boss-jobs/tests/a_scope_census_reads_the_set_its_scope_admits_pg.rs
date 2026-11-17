//! Postgres-level pin for `JobsRepository::scope_census`, the map cache's
//! authorization binding (review run 94784c1a of car dc7a2a00, F1).
//!
//! The shared map keys a narrowed caller's cached reading on the SET of
//! packets its scope admits, so a reassignment that takes a packet out
//! lands on a new key and is built afresh. The in-memory side is pinned
//! over HTTP by yard_regions_http
//! `a_packet_reassigned_away_is_not_served_from_the_cache`; this file
//! runs the SQL spelling of the same predicate against `PgJobs`, and the
//! bound that refuses rather than truncates.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::{JobScope, JobsRepository};
use boss_testing::TestDb;
use chrono::{NaiveDate, TimeZone, Utc};
use std::collections::BTreeSet;
use uuid::Uuid;

fn job(n: u128, owner: &str, subject: Subject) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::from_u128(n)),
        status: JobStatus::Open,
        metadata: serde_json::Value::Null,
        ..Job::new(
            "backlog-item".to_string(),
            subject,
            "item",
            owner,
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 11).unwrap(),
        )
    }
}

fn ids(ns: &[u128]) -> Option<BTreeSet<String>> {
    Some(ns.iter().map(|n| Uuid::from_u128(*n).to_string()).collect())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scope_census_reads_the_set_its_scope_admits_in_postgres() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let now = Utc.with_ymd_and_hms(2026, 11, 11, 6, 0, 0).unwrap();
    for j in [
        job(1, "emp-1", Subject::new("branch", "fix/a")),
        job(2, "emp-1", Subject::new("branch", "fix/b")),
        job(3, "emp-2", Subject::new("account", "acc-1")),
        job(4, "emp-2", Subject::new("branch", "acc-1")),
    ] {
        repo.create_job_at(&j, now, &[]).await.unwrap();
    }

    let owner = JobScope::OwnerIs("emp-1".into());
    assert_eq!(repo.scope_census(&owner, 10).await.unwrap(), ids(&[1, 2]));
    assert_eq!(
        repo.scope_census(&JobScope::OwnerIn(vec!["emp-2".into()]), 10)
            .await
            .unwrap(),
        ids(&[3, 4])
    );
    assert_eq!(
        repo.scope_census(&JobScope::AccountIn(vec!["acc-1".into()]), 10)
            .await
            .unwrap(),
        ids(&[3]),
        "only an account or employee subject maps into the account list"
    );
    assert_eq!(repo.scope_census(&JobScope::All, 10).await.unwrap(), None);
    assert!(repo.scope_census(&JobScope::None, 10).await.is_err());
    assert!(
        repo.scope_census(&owner, 1).await.is_err(),
        "a set past the bound is refused, never truncated"
    );

    // A reassignment takes a packet out of the set the scope admits.
    let mut moved = repo
        .get_job(&JobId::from_uuid(Uuid::from_u128(2)))
        .await
        .unwrap()
        .unwrap();
    moved.owner_id = "emp-2".into();
    repo.update_job(&moved).await.unwrap();
    assert_eq!(repo.scope_census(&owner, 10).await.unwrap(), ids(&[1]));
}
