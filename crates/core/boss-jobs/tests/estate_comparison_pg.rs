//! PostgreSQL persistence controls share the HTTP fixture with the default,
//! database-free target; requiring postgres must not hide those default tests.
use axum::http::StatusCode;
use std::sync::Arc;
#[path = "estate_comparison_http/common.rs"]
mod common;
use common::{
    app_with, comparison_body, exercise_observation_identity_boundary, post_comparison, user_header,
};

#[tokio::test]
async fn pg_replay_of_one_observation_atomically_records_one_comparison() {
    let db = boss_testing::TestDb::new().await;
    let jobs = Arc::new(boss_jobs::PgJobs::new(db.pool.clone()));
    let app = app_with(jobs);
    let user = user_header("rule:estate-compare-on-observation", "operator");
    let mut body: serde_json::Value = serde_json::from_str(&comparison_body()).unwrap();
    body["observation_id"] = serde_json::json!("a56cb17b-00ba-4f61-9dc3-01c48871968a");
    let body = body.to_string();
    let (a, b) = tokio::join!(
        post_comparison(&app, Some(&user), &body),
        post_comparison(&app, Some(&user), &body)
    );
    assert_eq!((a, b), (StatusCode::ACCEPTED, StatusCode::ACCEPTED));
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind = 'jobs.estate.compared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        count, 1,
        "a replay cannot manufacture another persistence sample in PostgreSQL"
    );
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM estate_comparison_receipts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(receipts, 1);
}

#[tokio::test]
async fn canonical_identity_deduplicates_one_fact_but_preserves_distinct_facts_in_pg() {
    let db = boss_testing::TestDb::new().await;
    let jobs = Arc::new(boss_jobs::PgJobs::new(db.pool.clone()));
    exercise_observation_identity_boundary(&app_with(jobs)).await;
    let events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind = 'jobs.estate.compared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM estate_comparison_receipts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!((events, receipts), (2, 2));
}
