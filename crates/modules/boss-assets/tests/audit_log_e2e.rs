//! End-to-end test for the assets → audit_log chain.

#![cfg(feature = "postgres")]

mod common;

use axum::http::StatusCode;
use boss_testing::{TestDb, TestRequest};
use common::{AssetsTestApp, received_event};

#[tokio::test(flavor = "multi_thread")]
async fn post_event_lands_in_audit_log() {
    let db = TestDb::new().await;
    let app = AssetsTestApp::with_audit_pool(db.pool.clone()).await;

    let event = received_event("evt-audit-1", "SN-AUDIT-1");
    TestRequest::post("/api/assets/events")
        .json(&event)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let row: (String, String) = sqlx::query_as(
        "SELECT source, kind FROM audit_log \
         WHERE kind = 'asset.received' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST");

    assert_eq!(row.0, "assets");
    assert_eq!(row.1, "asset.received");
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_events_each_land_in_audit_log() {
    // The bulk path also has to publish through the same publisher
    // so audit_log captures every event in the batch.
    let db = TestDb::new().await;
    let app = AssetsTestApp::with_audit_pool(db.pool.clone()).await;

    let events = vec![
        received_event("evt-batch-a-1", "SN-BATCH-A-1"),
        received_event("evt-batch-a-2", "SN-BATCH-A-2"),
        received_event("evt-batch-a-3", "SN-BATCH-A-3"),
    ];

    TestRequest::post("/api/assets/events/batch")
        .json(&events)
        .send(&app.router)
        .await
        .assert_status(StatusCode::OK);

    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM audit_log WHERE kind = 'asset.received'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 3, "every event in the batch must land in audit_log");
}
