//! `JobFilter::priority` against Postgres: a listing asked for one
//! priority answers that priority's packets, and its `total` counts
//! the same set.
//!
//! WHY IT EXISTS (backlog 74569e94, design ea906603 Q1, 2026-09-27).
//! The filter field was on the port and the in-memory adapter honoured
//! it, but the Postgres query bound no parameter for it — so a caller
//! asking for `urgent` got every packet back, and no caller had asked
//! yet, which is why nothing had noticed. The top board's `outranks`
//! read is the first caller: measured that morning, 17 of 460 open
//! packets were urgent, and an ignored filter would have drawn all 460
//! as outranking regular order. A filter the store ignores answers
//! instead of erroring; this pins the Postgres adapter to the
//! in-memory one's answer.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::{JobFilter, JobsRepository};
use boss_testing::TestDb;
use chrono::NaiveDate;

fn job(title: &str, priority: Priority) -> Job {
    Job {
        id: JobId::new(),
        kind: "backlog-item".to_string(),
        workflow_version: 1,
        subject: Subject::new("custom", "s"),
        title: title.to_string(),
        owner_id: "emp-a".into(),
        status: JobStatus::Open,
        priority,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 27).expect("day"),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_priority_filter_narrows_the_list_and_its_total_in_postgres() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    for (title, p) in [
        ("routine", Priority::Standard),
        ("also routine", Priority::Standard),
        ("hurry", Priority::Urgent),
        ("fire", Priority::Emergency),
    ] {
        repo.create_job(&job(title, p)).await.expect("create");
    }

    let urgent = JobFilter {
        status: Some(JobStatus::Open),
        priority: Some(Priority::Urgent),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&urgent, 100, 0).await.expect("list");
    let titles: Vec<&str> = rows.iter().map(|j| j.title.as_str()).collect();
    assert_eq!(titles, ["hurry"], "{rows:?}");
    assert_eq!(total, 1, "the total counts the same set the rows are");

    // No priority asked for is no filter: every open packet.
    let all = JobFilter {
        status: Some(JobStatus::Open),
        ..Default::default()
    };
    let (_, total) = repo.list_jobs(&all, 100, 0).await.expect("list");
    assert_eq!(total, 4);
}
