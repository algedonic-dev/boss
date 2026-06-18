//! Roundtrip tests for the shared `PgAuditWriter`.
//!
//! Locks in the contract that every service binary depends on:
//! `PgAuditWriter::write` inserts one row into `audit_log`, and that
//! row faithfully roundtrips the original `Event` (id, timestamp,
//! source, kind, payload).
//!
//! Before this writer was extracted, only `boss-catalog` populated
//! `audit_log` — the other five services emitted to NATS but never
//! persisted, and there were no tests covering the behavior at all.

#![cfg(feature = "postgres")]

use boss_core::audit::AuditWriter;
use boss_core::event::Event;
use boss_events::PgAuditWriter;
use boss_testing::TestDb;
use chrono::{TimeZone, Utc};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn write_inserts_one_row_with_all_columns_populated() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    let event = Event {
        id: Uuid::new_v4(),
        timestamp: Utc.with_ymd_and_hms(2026, 4, 11, 12, 30, 0).unwrap(),
        source: "test".to_string(),
        kind: "test.thing.happened".to_string(),
        payload: serde_json::json!({"a": 1, "b": "two"}),
    };

    writer.write(&event).await.expect("write should succeed");

    let row: (
        Uuid,
        chrono::DateTime<Utc>,
        String,
        String,
        serde_json::Value,
    ) = sqlx::query_as(
        "SELECT event_id, timestamp, source, kind, payload FROM audit_log WHERE event_id = $1",
    )
    .bind(event.id)
    .fetch_one(&db.pool)
    .await
    .expect("row should exist");

    assert_eq!(row.0, event.id);
    assert_eq!(row.1, event.timestamp);
    assert_eq!(row.2, "test");
    assert_eq!(row.3, "test.thing.happened");
    assert_eq!(row.4, event.payload);
}

#[tokio::test(flavor = "multi_thread")]
async fn multiple_writes_produce_multiple_rows() {
    // Two writes for the same kind from the same source must produce
    // two rows, not be deduplicated. The audit log is append-only;
    // collapsing duplicates would lose information.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    for i in 0..3 {
        let event = Event::new(
            "test",
            "test.repeat",
            serde_json::json!({"i": i}),
            chrono::Utc::now(),
        );
        writer.write(&event).await.unwrap();
    }

    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM audit_log WHERE kind = 'test.repeat'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn write_preserves_complex_jsonb_payload() {
    // Real domain events carry nested objects and arrays. Verify the
    // jsonb roundtrip is lossless for the kinds of structures the
    // services actually emit.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    let payload = serde_json::json!({
        "device_serial": "LUM-3D-00012345",
        "kind": "ServiceJobOpened",
        "ticket_id": "tkt-0042",
        "actor_id": "emp-007",
        "metadata": {
            "tags": ["urgent", "warranty"],
            "score": 4.5,
            "nested": {"level": 2}
        }
    });
    let event = Event::new(
        "test-source",
        "test.kind.example",
        payload.clone(),
        chrono::Utc::now(),
    );
    writer.write(&event).await.unwrap();

    let (returned,): (serde_json::Value,) =
        sqlx::query_as("SELECT payload FROM audit_log WHERE event_id = $1")
            .bind(event.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(returned, payload);
}

#[tokio::test(flavor = "multi_thread")]
async fn write_batch_inserts_all_rows_in_one_statement() {
    // The bulk path: many events go in via one INSERT, every row
    // appears in audit_log with the right contents. This is the
    // hot-path optimization called from DomainPublisher::publish_batch
    // on the assets bulk endpoint.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    let events: Vec<Event> = (0..50)
        .map(|i| {
            Event::new(
                "assets",
                "asset.received",
                serde_json::json!({"i": i, "device_serial": format!("SN-BULK-{i:03}")}),
                chrono::Utc::now(),
            )
        })
        .collect();

    writer
        .write_batch(&events)
        .await
        .expect("bulk write should succeed");

    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM audit_log WHERE kind = 'asset.received'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 50);

    // Spot-check a payload to confirm the bulk path didn't reorder
    // bind parameters between rows.
    let (payload,): (serde_json::Value,) =
        sqlx::query_as("SELECT payload FROM audit_log WHERE event_id = $1")
            .bind(events[7].id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(payload["i"], 7);
    assert_eq!(payload["device_serial"], "SN-BULK-007");
}

#[tokio::test(flavor = "multi_thread")]
async fn write_batch_with_empty_input_is_a_noop() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    writer.write_batch(&[]).await.expect("empty batch ok");
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM audit_log")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_from_different_sources_are_distinguishable() {
    // The cross-service guarantee: when six services share an audit
    // log, queries by `source` must return only that service's rows.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    for source in ["catalog", "assets", "people", "commerce"] {
        let event = Event::new(
            source,
            "test.event",
            serde_json::json!({}),
            chrono::Utc::now(),
        );
        writer.write(&event).await.unwrap();
    }

    let (catalog_count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM audit_log WHERE source = 'catalog'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(catalog_count, 1);

    let (total,): (i64,) = sqlx::query_as("SELECT count(*) FROM audit_log")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(total, 4);
}
