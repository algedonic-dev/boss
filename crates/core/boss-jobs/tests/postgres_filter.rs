//! Postgres-level regression test for the jobs list filter.
//!
//! Guards a `subject_id` filter bug class with two failure modes:
//!   1. The client passes `?subject_id=` but the HTTP handler reads a
//!      different param name, so the filter silently falls through and
//!      the call returns every job system-wide.
//!   2. The handler translates the query param into `filter.subject_id`
//!      correctly, but the Postgres `list_jobs` SQL has no
//!      `subject_id = $X` predicate and ignores the filter, so the
//!      call returns an empty set.
//!
//! The in-memory adapter honors `filter.subject_id`, so the sibling
//! filter test in `policy_gated_handlers.rs` (which runs against
//! `InMemoryJobs`) wouldn't catch a Postgres-only gap. This file runs
//! the same shape against `PgJobs`.

#![cfg(feature = "postgres")]

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::{JobFilter, JobScope, JobsRepository};
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn job(id: &str, kind: &str, subject: Subject) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: kind.to_string(),
        job_kind_version: 1,
        subject,
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn list_jobs_filters_by_subject_id_in_postgres() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let account_a = Subject::new("account", "prac-A");
    let account_b = Subject::new("account", "prac-B");
    let system_a = Subject::new("asset", "SYS-A");

    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000001",
        "sale",
        account_a.clone(),
    ))
    .await
    .unwrap();
    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000002",
        "sale",
        account_a.clone(),
    ))
    .await
    .unwrap();
    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000003",
        "sale",
        account_b,
    ))
    .await
    .unwrap();
    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000004",
        "field-service",
        system_a,
    ))
    .await
    .unwrap();

    // No filter → all 4.
    let (_, total) = repo
        .list_jobs(
            &JobFilter {
                scope: JobScope::All,
                ..Default::default()
            },
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(total, 4);

    // subject_id = prac-A → exactly 2 sale jobs.
    let (rows, total) = repo
        .list_jobs(
            &JobFilter {
                scope: JobScope::All,
                subject_id: Some("prac-A".into()),
                ..Default::default()
            },
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(total, 2, "prac-A should have 2 jobs");
    assert_eq!(rows.len(), 2);

    // subject_id = SYS-A → exactly 1 field-service job.
    let (rows, total) = repo
        .list_jobs(
            &JobFilter {
                scope: JobScope::All,
                subject_id: Some("SYS-A".into()),
                ..Default::default()
            },
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(total, 1, "SYS-A should have 1 job");
    assert_eq!(rows.len(), 1);

    // subject_id = unknown → zero, not "everything".
    let (_, total) = repo
        .list_jobs(
            &JobFilter {
                scope: JobScope::All,
                subject_id: Some("does-not-exist".into()),
                ..Default::default()
            },
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(total, 0);
}
