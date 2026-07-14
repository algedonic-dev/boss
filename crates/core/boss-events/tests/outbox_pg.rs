//! Transactional event outbox — the durable hand-off that makes an
//! event's persistence atomic with the state change it describes
//! (docs/design/transactional-audit-log.md, Option B).
//!
//! Contract under test:
//! - `record_event_in_tx` joins the caller's transaction: commit
//!   lands the row, rollback removes every trace, and a ref-check
//!   rejection aborts the caller's write instead of punching a
//!   post-commit provenance hole (the 2026-07-13 incident class).
//! - `drain_outbox_once` is the whole relay pipeline: outbox →
//!   audit_log (chained, id-ordered) → bus → delivered_at, and every
//!   crash point retries idempotently (no duplicate audit rows, no
//!   lost notifications).

#![cfg(feature = "postgres")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use boss_core::event::Event;
use boss_core::port::{EventBus, EventBusError, EventStream};
use boss_events::outbox::{drain_outbox_once, pending_count, record_event_in_tx};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{TimeZone, Utc};
use uuid::Uuid;

fn event(kind: &str) -> Event {
    Event {
        id: Uuid::new_v4(),
        timestamp: Utc.with_ymd_and_hms(2026, 7, 13, 12, 0, 0).unwrap(),
        source: "outbox-test".to_string(),
        kind: kind.to_string(),
        payload: serde_json::json!({"n": 1}),
    }
}

/// A bus that always fails — the relay's crash-point double.
struct FailingBus {
    attempts: AtomicUsize,
}
#[async_trait::async_trait]
impl EventBus for FailingBus {
    async fn publish(&self, _e: Event) -> Result<(), EventBusError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(EventBusError::PublishFailed("bus down".into()))
    }
    async fn subscribe(&self, _p: &str) -> Result<Box<dyn EventStream>, EventBusError> {
        Err(EventBusError::SubscribeFailed("stub".into()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn commit_then_drain_lands_in_audit_log_bus_and_marks_delivered() {
    let db = TestDb::new().await;
    let e = event("outbox.test.committed");

    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &e).await.expect("record");
    tx.commit().await.unwrap();

    assert_eq!(pending_count(&db.pool).await.unwrap(), 1);

    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .expect("drain");
    assert_eq!(stats.delivered, 1);

    // Audit row exists, chained (the trigger filled the hash pair).
    let (kind, hash_len): (String, i32) =
        sqlx::query_as("SELECT kind, length(row_hash)::int FROM audit_log WHERE event_id = $1")
            .bind(e.id)
            .fetch_one(&db.pool)
            .await
            .expect("audit row");
    assert_eq!(kind, "outbox.test.committed");
    assert_eq!(hash_len, 32);

    // Bus saw it; outbox is drained.
    bus.assert_event_emitted("outbox.test.committed");
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_leaves_no_trace() {
    let db = TestDb::new().await;
    let e = event("outbox.test.rolled-back");

    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &e).await.expect("record");
    tx.rollback().await.unwrap();

    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE event_id = $1")
        .bind(e.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "rollback must remove the outbox row");
}

#[tokio::test(flavor = "multi_thread")]
async fn ref_check_rejection_aborts_the_recording_transaction() {
    // The incident shape: an event referencing an account that does
    // not exist. With the outbox trigger the rejection happens INSIDE
    // the domain tx — the caller gets an Err and aborts, instead of
    // committing state whose provenance is silently dropped later.
    let db = TestDb::new().await;
    sqlx::query(
        "INSERT INTO audit_log_ref_checks (event_kind, field_path, ref_table, ref_column) \
         VALUES ('outbox.test.ref-checked', 'account_id', 'accounts', 'id')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let mut e = event("outbox.test.ref-checked");
    e.payload = serde_json::json!({"account_id": "account-does-not-exist"});

    let mut tx = db.pool.begin().await.unwrap();
    // TestDb disables ref-checks database-wide (test sessions write
    // audit_log without full projections); re-enable for exactly this
    // transaction — SET LOCAL cannot leak through the pool.
    sqlx::query("SET LOCAL audit_log.ref_check = 'on'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let err = record_event_in_tx(&mut tx, &e)
        .await
        .expect_err("missing ref must reject the insert");
    assert!(
        err.contains("non-existent"),
        "error should carry the trigger's message: {err}"
    );
    // The tx is poisoned by the failed statement — rollback and
    // verify nothing landed anywhere.
    tx.rollback().await.unwrap();
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn bus_failure_retries_without_duplicating_the_audit_row() {
    // Crash-point contract: audit INSERT committed, NATS publish
    // failed → the row stays pending. The re-drain must NOT insert a
    // second audit row (idempotent by event_id), must re-publish, and
    // must then mark delivered.
    let db = TestDb::new().await;
    let e = event("outbox.test.retry");

    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &e).await.unwrap();
    tx.commit().await.unwrap();

    let failing = Arc::new(FailingBus {
        attempts: AtomicUsize::new(0),
    });
    let stats = drain_outbox_once(&db.pool, &(failing.clone() as Arc<dyn EventBus>), 100)
        .await
        .expect("drain with failing bus is not an error — the row just stays pending");
    assert_eq!(stats.delivered, 0);
    assert_eq!(failing.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        pending_count(&db.pool).await.unwrap(),
        1,
        "undelivered row stays pending"
    );

    // The audit half already landed (committed before the publish).
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_id = $1")
        .bind(e.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // Retry with a working bus: no duplicate, published, delivered.
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(stats.delivered, 1);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_id = $1")
        .bind(e.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 1, "retry must not duplicate the audit row");
    bus.assert_event_emitted("outbox.test.retry");
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn drain_preserves_outbox_order_in_audit_log() {
    let db = TestDb::new().await;
    let first = event("outbox.test.order-a");
    let second = event("outbox.test.order-b");

    for e in [&first, &second] {
        let mut tx = db.pool.begin().await.unwrap();
        record_event_in_tx(&mut tx, e).await.unwrap();
        tx.commit().await.unwrap();
    }

    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();

    let ids: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT event_id FROM audit_log WHERE kind LIKE 'outbox.test.order-%' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        ids.iter().map(|r| r.0).collect::<Vec<_>>(),
        vec![first.id, second.id],
        "audit id order must follow outbox order"
    );
    // Bus order too — the relay is the single ordering point.
    let kinds: Vec<String> = bus.events().iter().map(|e| e.kind.clone()).collect();
    assert_eq!(kinds, vec!["outbox.test.order-a", "outbox.test.order-b"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_outbox_drain_is_a_cheap_no_op() {
    let db = TestDb::new().await;
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(stats.delivered, 0);
    assert_eq!(bus.event_count(), 0);
}
