#![cfg(feature = "postgres")]

use boss_core::job::{Job, JobId, Priority, Step, Subject};
use boss_jobs::{InMemoryJobs, JobsRepository, PgJobs};
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

async fn seed(repo: &impl JobsRepository) -> (Vec<JobId>, Vec<Step>) {
    let mut ids = Vec::new();
    let mut expected = Vec::new();
    for n in [2, 1, 3] {
        let job = Job {
            id: JobId::from_uuid(Uuid::from_u128(n)),
            ..Job::new(
                "ship-a-change",
                Subject::new("custom", "batch"),
                "car",
                "emp-1",
                Priority::Standard,
                NaiveDate::from_ymd_opt(2026, 10, 23).unwrap(),
            )
        };
        repo.create_job_at(&job, chrono::Utc::now(), &[])
            .await
            .unwrap();
        ids.push(job.id);
        for order in [7, 1, 4] {
            let mut step = Step::new(job.id, "task", format!("step {order}"), order);
            step.spec_slug = Some(format!("step-{order}"));
            step.metadata = serde_json::json!({"original": n, "order": order});
            repo.add_step_at(&step, chrono::Utc::now(), &[])
                .await
                .unwrap();
            expected.push(step);
        }
    }
    ids.push(ids[0]);
    ids.push(JobId::from_uuid(Uuid::from_u128(99)));
    expected.sort_by_key(|s| (*s.job_id.inner().as_uuid(), s.sort_order));
    (ids, expected)
}

#[tokio::test]
async fn default_batch_preserves_all_rows_and_order_without_duplicate_jobs() {
    let repo = InMemoryJobs::new();
    let (ids, expected) = seed(&repo).await;
    assert_eq!(repo.list_steps_for_jobs(&ids).await.unwrap(), expected);
    assert!(repo.list_steps_for_jobs(&[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn postgres_batch_is_one_statement_and_preserves_the_port_rows() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let (ids, expected) = seed(&repo).await;
    // An initplan in a test-owned view records one evaluation per SELECT,
    // independently of row count. A per-job loop reads it three times.
    sqlx::raw_sql("CREATE SEQUENCE step_read_statements; ALTER TABLE steps RENAME TO batch_steps; CREATE VIEW steps AS SELECT batch_steps.*, xmin FROM batch_steps WHERE (SELECT nextval('step_read_statements')) > 0")
        .execute(&db.pool).await.unwrap();
    assert_eq!(repo.list_steps_for_jobs(&ids).await.unwrap(), expected);
    let count: i64 = sqlx::query_scalar("SELECT last_value FROM step_read_statements")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "all car steps must be read in one statement, not one per car"
    );
    assert!(repo.list_steps_for_jobs(&[]).await.unwrap().is_empty());
    // Storage failure is an error, never an empty car-step population.
    sqlx::raw_sql("DROP VIEW steps")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(repo.list_steps_for_jobs(&ids).await.is_err());
}
