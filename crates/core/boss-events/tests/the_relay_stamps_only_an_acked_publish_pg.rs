//! The outbox relay stamps `delivered_at` only on a publish the bus
//! CONFIRMED (backlog 2e45840a, design ad9519bc).
//!
//! Until this car the NATS adapter answered Ok when a core publish was
//! accepted into the client's buffer, with no flush and no JetStream
//! ack, and the relay stamped the row delivered on that Ok. An event
//! buffered just before a disconnect, or published while the
//! `BOSS_EVENTS` stream was absent, was in `audit_log` and marked
//! delivered, and never reached the delivery buffer the dispatcher's
//! durable consumers read: a conservation violation nothing alarmed on.
//!
//! These run the REAL relay pass over the REAL adapter against a
//! loopback NATS stub (boss-nats' `nats_stub` — the pod and the gate
//! have no `nats-server`), so what is pinned is the whole path from the
//! wire's answer to the row's stamp.

use std::sync::Arc;

use boss_core::event::Event;
use boss_core::port::EventBus;
use boss_events::outbox::{drain_outbox_once, record_event_in_tx};
use boss_nats::NatsEventBus;
use boss_testing::TestDb;
use uuid::Uuid;

// The wire stub boss-nats' own adapter tests use, by path: it is test
// code, kept out of every library's production source on purpose
// (see its header).
#[path = "../../boss-nats/src/nats_stub.rs"]
mod nats_stub;
use nats_stub::{Reply, nats_stub};

/// A kind the durable stream captures (`events.>`), so its publish is
/// one the delivery buffer must hold.
const KIND: &str = "events.test.relay-confirmed";

async fn staged(db: &TestDb) -> Event {
    let event = Event {
        id: Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        source: "relay-ack-test".to_string(),
        kind: KIND.to_string(),
        payload: serde_json::json!({"n": 1}),
    };
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &event).await.expect("stage");
    tx.commit().await.unwrap();
    event
}

async fn delivered(db: &TestDb, event: &Event) -> bool {
    sqlx::query_scalar("SELECT delivered_at IS NOT NULL FROM event_outbox WHERE event_id = $1")
        .bind(event.id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn bus(url: &str) -> Arc<dyn EventBus> {
    Arc::new(NatsEventBus::connect(url).await.expect("a bus on the stub"))
}

/// The stream never acks (the ack lost in transit, or a server that
/// stored nothing): the row is NOT stamped, and the next pass owes it.
#[tokio::test(flavor = "multi_thread")]
async fn a_relay_pass_whose_publish_is_not_acked_leaves_delivered_at_null() {
    let db = TestDb::new().await;
    let event = staged(&db).await;
    let stub = nats_stub(Reply::Silent).await;

    let stats = drain_outbox_once(&db.pool, &bus(&stub.url).await, 100)
        .await
        .expect("an unacked publish is not a drain error");

    assert_eq!(stats.delivered, 0, "{stats:?}");
    assert!(
        !delivered(&db, &event).await,
        "the publish reached the server and no ack came back: the row stays owed"
    );
    assert_eq!(
        stub.received().len(),
        1,
        "the publish was sent; only its confirmation is missing"
    );
}

/// The stream is absent (no responders): the row is NOT stamped.
#[tokio::test(flavor = "multi_thread")]
async fn a_relay_pass_while_the_stream_is_absent_leaves_delivered_at_null() {
    let db = TestDb::new().await;
    let event = staged(&db).await;
    let stub = nats_stub(Reply::NoStream).await;

    let stats = drain_outbox_once(&db.pool, &bus(&stub.url).await, 100)
        .await
        .expect("an absent stream is not a drain error");

    assert_eq!(stats.delivered, 0, "{stats:?}");
    assert!(!delivered(&db, &event).await);
}

/// The stream acks: the row IS stamped, and the publish carried the
/// event id as `Nats-Msg-Id`, so a retry after a lost ack is dropped by
/// the stream's duplicate window instead of stored twice.
#[tokio::test(flavor = "multi_thread")]
async fn an_acked_relay_pass_stamps_delivered_at() {
    let db = TestDb::new().await;
    let event = staged(&db).await;
    let stub = nats_stub(Reply::Ack).await;

    let stats = drain_outbox_once(&db.pool, &bus(&stub.url).await, 100)
        .await
        .expect("drain");

    assert_eq!(stats.delivered, 1, "{stats:?}");
    assert!(delivered(&db, &event).await);
    let sent = stub.received();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].subject, KIND);
    assert!(sent[0].reply.is_some(), "a stream publish asks for its ack");
    let carried: Event = serde_json::from_slice(&sent[0].payload).expect("an Event");
    assert_eq!(carried.id, event.id);
    assert!(
        sent[0]
            .headers
            .contains(&format!("Nats-Msg-Id: {}", event.id)),
        "the event id rides as the dedup key: {:?}",
        sent[0].headers
    );
}
