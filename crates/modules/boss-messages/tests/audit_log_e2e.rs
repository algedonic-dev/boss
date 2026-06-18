//! End-to-end test for the messages → audit_log chain.

#![cfg(feature = "postgres")]

mod common;

use axum::http::StatusCode;
use boss_testing::{TestDb, TestRequest};
use common::MessageTestApp;

#[tokio::test(flavor = "multi_thread")]
async fn send_message_lands_in_audit_log() {
    let db = TestDb::new().await;
    let app = MessageTestApp::with_audit_pool(db.pool.clone());

    let body = serde_json::json!({
        "sender_id": "emp-sender",
        "recipient_id": "emp-recipient",
        "subject": "audit log test",
        "body": "hello",
        "kind": "direct",
    });
    TestRequest::post("/api/messages/send")
        .json(&body)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let row: (String, String) = sqlx::query_as(
        "SELECT source, kind FROM audit_log \
         WHERE kind = 'messages.message.sent' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST");

    assert_eq!(row.0, "messages");
    assert_eq!(row.1, "messages.message.sent");
}
