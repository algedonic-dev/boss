//! Audit-chain test for the batch-messages path.
//!
//! `POST /api/messages/batch` must emit MESSAGE_SENT for each row,
//! the same as the single-message POST handler — otherwise the
//! batch path used by the sim and bulk imports is a silent bypass
//! and every batch-imported row vanishes on `boss-rebuild-all`.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_messages::PgMessages;
use boss_messages::http::{MessageApiState, router};
use boss_messages::rebuild_messages;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use serde_json::json;
use sqlx::PgPool;

fn build_pg_app(pool: PgPool) -> Router {
    let publisher = DomainPublisher::new(RecordingEventBus::new(), "messages")
        .with_audit(Arc::new(PgAuditWriter::new(pool.clone())));
    router(MessageApiState {
        messages: Arc::new(PgMessages::new(pool)),
        publisher: Some(publisher),
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    })
}

async fn count(pool: &PgPool) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(pool)
        .await
        .unwrap();
    row.0
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_messages_survive_rebuild() {
    let db = TestDb::new().await;
    let app = build_pg_app(db.pool.clone());

    let batch = json!([
        {
            "id": "msg-batch-001",
            "sender_id": "emp-sim",
            "recipient_id": "emp-recipient-1",
            "subject": "Wholesale dispatch — 2026-05-04",
            "body": "Driver loaded 84 kegs.",
            "kind": "signal",
            "sent_at": "2026-05-04T09:15:00Z",
        },
        {
            "id": "msg-batch-002",
            "sender_id": "emp-sim",
            "recipient_id": "emp-recipient-2",
            "subject": "Hop shipment received",
            "body": "Pallet from Cascade landed.",
            "kind": "signal",
            "sent_at": "2026-05-04T11:30:00Z",
        },
    ]);

    TestRequest::post("/api/messages/batch")
        .json(&batch)
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    let pre = count(&db.pool).await;
    assert_eq!(pre, 2, "two batch rows landed in projection");

    // Wipe + rebuild. With the audit-chain fix, the rebuilder
    // replays MESSAGE_SENT and reproduces both rows. Pre-fix this
    // would leave count=0.
    sqlx::query("DELETE FROM messages")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = rebuild_messages(&db.pool).await.expect("rebuild");
    assert!(
        report.events_processed >= 2,
        "rebuild should replay both batch events, got {report:?}"
    );

    let post = count(&db.pool).await;
    assert_eq!(post, pre, "batch rows must round-trip through audit_log");
}
