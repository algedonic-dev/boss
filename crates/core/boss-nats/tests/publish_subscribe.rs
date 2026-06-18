//! Integration tests against a real NATS server.
//!
//! These tests assume a `nats-server` is running at `nats://127.0.0.1:4222`.
//! They are `#[ignore]` by default so they do not run in the normal test
//! sweep. Run with:
//!
//! ```bash
//! cargo test -p boss-nats -- --ignored
//! ```

use std::time::Duration;

use boss_core::event::Event;
use boss_core::port::EventBus;
use boss_nats::NatsEventBus;

const NATS_URL: &str = "nats://127.0.0.1:4222";

fn unique_prefix() -> String {
    format!("test.{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
#[ignore = "requires nats-server at 127.0.0.1:4222"]
async fn publish_reaches_exact_subscriber() {
    let bus = NatsEventBus::connect(NATS_URL).await.unwrap();
    let kind = format!("{}.hello", unique_prefix());

    let mut stream = bus.subscribe(&kind).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let event = Event::new(
        "integration",
        kind.clone(),
        serde_json::json!({"n": 1}),
        chrono::Utc::now(),
    );
    bus.publish(event.clone()).await.unwrap();
    bus.flush().await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.id, event.id);
    assert_eq!(received.kind, kind);
    assert_eq!(received.payload["n"], 1);
}

#[tokio::test]
#[ignore = "requires nats-server at 127.0.0.1:4222"]
async fn multi_token_wildcard_matches_nested_subjects() {
    let bus = NatsEventBus::connect(NATS_URL).await.unwrap();
    let prefix = unique_prefix();
    let mut stream = bus.subscribe(&format!("{prefix}.>")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let k1 = format!("{prefix}.one");
    let k2 = format!("{prefix}.nested.two");
    bus.publish(Event::new(
        "t",
        k1.clone(),
        serde_json::json!({}),
        chrono::Utc::now(),
    ))
    .await
    .unwrap();
    bus.publish(Event::new(
        "t",
        k2.clone(),
        serde_json::json!({}),
        chrono::Utc::now(),
    ))
    .await
    .unwrap();
    bus.flush().await.unwrap();

    let e1 = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap();
    let e2 = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap();
    let mut got = [e1.kind, e2.kind];
    got.sort();
    let mut want = [k1, k2];
    want.sort();
    assert_eq!(got, want);
}

#[tokio::test]
#[ignore = "requires nats-server at 127.0.0.1:4222"]
async fn single_token_wildcard_rejects_nested() {
    let bus = NatsEventBus::connect(NATS_URL).await.unwrap();
    let prefix = unique_prefix();
    let mut stream = bus.subscribe(&format!("{prefix}.*")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let matched = format!("{prefix}.one");
    let not_matched = format!("{prefix}.nested.two");
    bus.publish(Event::new(
        "t",
        not_matched,
        serde_json::json!({}),
        chrono::Utc::now(),
    ))
    .await
    .unwrap();
    bus.publish(Event::new(
        "t",
        matched.clone(),
        serde_json::json!({}),
        chrono::Utc::now(),
    ))
    .await
    .unwrap();
    bus.flush().await.unwrap();

    let e = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(e.kind, matched);
}

#[tokio::test]
#[ignore = "requires nats-server at 127.0.0.1:4222"]
async fn malformed_payload_is_skipped_not_fatal() {
    let bus = NatsEventBus::connect(NATS_URL).await.unwrap();
    let kind = format!("{}.evt", unique_prefix());

    let mut stream = bus.subscribe(&kind).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Publish garbage first via the raw client.
    bus.client()
        .publish(kind.clone(), b"not json".to_vec().into())
        .await
        .unwrap();
    let event = Event::new(
        "t",
        kind.clone(),
        serde_json::json!({"ok": true}),
        chrono::Utc::now(),
    );
    bus.publish(event.clone()).await.unwrap();
    bus.flush().await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.id, event.id);
}
