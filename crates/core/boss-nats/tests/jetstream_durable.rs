//! Integration tests for the JetStream durability layer.
//!
//! These assume a `nats-server` **with JetStream enabled** at
//! `nats://127.0.0.1:4222`. They are `#[ignore]` by default. Run with:
//!
//! ```bash
//! cargo test -p boss-nats --test jetstream_durable -- --ignored
//! ```
//!
//! Each test that exercises delivery mechanics uses its own throwaway
//! stream so runs don't collide or pollute `BOSS_EVENTS` — memory-backed
//! for speed, except the purge test, which needs the file store
//! production uses.

use std::time::Duration;

use async_nats::jetstream::{self, stream};
use boss_nats::durable;
use futures::StreamExt;

const NATS_URL: &str = "nats://127.0.0.1:4222";

async fn context() -> jetstream::Context {
    let client = async_nats::connect(NATS_URL).await.unwrap();
    jetstream::new(client)
}

/// Create a unique memory-backed stream and return `(stream_name,
/// subject_root)`. Memory storage keeps tests fast and self-cleaning.
async fn throwaway_stream(ctx: &jetstream::Context, tag: &str) -> (String, String) {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("test_{tag}_{id}");
    ctx.create_stream(stream::Config {
        name: name.clone(),
        subjects: vec![format!("{name}.>")],
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::Memory,
        ..Default::default()
    })
    .await
    .unwrap();
    (name.clone(), name)
}

/// Same as [`throwaway_stream`] but file-backed, matching production
/// `BOSS_EVENTS`. Only needed where the memory store's capabilities
/// differ (purge).
async fn throwaway_file_stream(ctx: &jetstream::Context, tag: &str) -> (String, String) {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("test_{tag}_{id}");
    ctx.create_stream(stream::Config {
        name: name.clone(),
        subjects: vec![format!("{name}.>")],
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        ..Default::default()
    })
    .await
    .unwrap();
    (name.clone(), name)
}

#[tokio::test]
#[ignore = "requires nats-server with JetStream at 127.0.0.1:4222"]
async fn ack_settles_and_does_not_redeliver() {
    let ctx = context().await;
    let (stream_name, root) = throwaway_stream(&ctx, "ack").await;
    ctx.publish(format!("{root}.evt"), b"{\"id\":\"e1\"}".to_vec().into())
        .await
        .unwrap()
        .await
        .unwrap();

    let mut msgs = durable::open_durable(&ctx, &stream_name, "d", vec![format!("{root}.>")])
        .await
        .unwrap();

    let m = tokio::time::timeout(Duration::from_secs(5), msgs.next())
        .await
        .expect("a message should arrive")
        .unwrap()
        .unwrap();
    assert_eq!(&m.payload[..], b"{\"id\":\"e1\"}");
    durable::settle::<&str>(&m, Ok(())).await;

    // After ACK the stream advances; nothing more is delivered.
    let again = tokio::time::timeout(Duration::from_secs(2), msgs.next()).await;
    assert!(again.is_err(), "an ACK'd message must not redeliver");

    ctx.delete_stream(&stream_name).await.unwrap();
}

#[tokio::test]
#[ignore = "requires nats-server with JetStream at 127.0.0.1:4222"]
async fn nak_redelivers_then_ack_settles() {
    let ctx = context().await;
    let (stream_name, root) = throwaway_stream(&ctx, "nak").await;
    ctx.publish(format!("{root}.evt"), b"{\"id\":\"e2\"}".to_vec().into())
        .await
        .unwrap()
        .await
        .unwrap();

    let mut msgs = durable::open_durable(&ctx, &stream_name, "d", vec![format!("{root}.>")])
        .await
        .unwrap();

    // First delivery → handler "fails" → NAK.
    let m1 = tokio::time::timeout(Duration::from_secs(5), msgs.next())
        .await
        .expect("first delivery")
        .unwrap()
        .unwrap();
    assert_eq!(m1.info().unwrap().delivered, 1);
    durable::settle(&m1, Err("transient boom")).await;

    // Redelivered after the first backoff gap (1s) → this time ACK.
    let m2 = tokio::time::timeout(Duration::from_secs(8), msgs.next())
        .await
        .expect("redelivery after NAK")
        .unwrap()
        .unwrap();
    assert_eq!(
        m2.info().unwrap().delivered,
        2,
        "redelivery is the second attempt of the same message"
    );
    assert_eq!(&m2.payload[..], b"{\"id\":\"e2\"}");
    durable::settle::<&str>(&m2, Ok(())).await;

    // After the ACK, no further redelivery.
    let again = tokio::time::timeout(Duration::from_secs(2), msgs.next()).await;
    assert!(
        again.is_err(),
        "after ACK there must be no further redelivery"
    );

    ctx.delete_stream(&stream_name).await.unwrap();
}

#[tokio::test]
#[ignore = "requires nats-server with JetStream at 127.0.0.1:4222"]
async fn ensure_stream_is_idempotent() {
    let ctx = context().await;
    // First call creates BOSS_EVENTS; second is a no-op. Neither errors.
    durable::ensure_stream(&ctx).await.unwrap();
    durable::ensure_stream(&ctx).await.unwrap();
    ctx.get_stream(durable::STREAM_NAME)
        .await
        .expect("BOSS_EVENTS exists after ensure_stream");
}

#[tokio::test]
#[ignore = "requires nats-server with JetStream at 127.0.0.1:4222"]
async fn durable_consumer_resumes_unacked_after_reopen() {
    // A genuinely durable consumer must redeliver an un-ACK'd message even
    // across a fresh `open_durable` (simulating a dispatcher restart).
    let ctx = context().await;
    let (stream_name, root) = throwaway_stream(&ctx, "resume").await;
    ctx.publish(format!("{root}.evt"), b"{\"id\":\"e3\"}".to_vec().into())
        .await
        .unwrap()
        .await
        .unwrap();

    {
        let mut msgs = durable::open_durable(&ctx, &stream_name, "d", vec![format!("{root}.>")])
            .await
            .unwrap();
        let m = tokio::time::timeout(Duration::from_secs(5), msgs.next())
            .await
            .expect("first delivery")
            .unwrap()
            .unwrap();
        // Drop without ACK (consumer "crashes").
        let _ = m;
    }

    // Reopen the same durable; the un-ACK'd message comes back.
    let mut msgs = durable::open_durable(&ctx, &stream_name, "d", vec![format!("{root}.>")])
        .await
        .unwrap();
    let m = tokio::time::timeout(Duration::from_secs(35), msgs.next())
        .await
        .expect("un-ACK'd message redelivers after ack_wait")
        .unwrap()
        .unwrap();
    assert_eq!(&m.payload[..], b"{\"id\":\"e3\"}");
    durable::settle::<&str>(&m, Ok(())).await;

    ctx.delete_stream(&stream_name).await.unwrap();
}

#[tokio::test]
#[ignore = "requires nats-server with JetStream at 127.0.0.1:4222"]
async fn purge_drops_undelivered_messages_but_keeps_the_durable_consumer() {
    // The demo epoch restart trims audit_log back to the seed baseline.
    // Anything still sitting in the delivery buffer at that moment
    // belongs to the epoch being deleted: redelivering it against the
    // trimmed database 404s through the whole redelivery budget and
    // dead-letters — spurious noise in a gate whose whole value is
    // reading zero. `purge_stream` drops that backlog.
    //
    // Purge, NOT `reset_stream`: reset deletes the stream and with it
    // every durable consumer, so a live dispatcher would have to
    // re-create its consumers mid-flight. The consumer must survive.
    let ctx = context().await;
    // File-backed, unlike the other throwaways: the memory store rejects
    // purge outright ("partial purges not supported"), and production
    // BOSS_EVENTS is `StorageType::File` — a memory-store test here would
    // be exercising a different code path than the one that ships.
    let (stream_name, root) = throwaway_file_stream(&ctx, "purge").await;

    for i in 0..3 {
        ctx.publish(
            format!("{root}.evt"),
            format!("{{\"id\":\"e{i}\"}}").into_bytes().into(),
        )
        .await
        .unwrap()
        .await
        .unwrap();
    }

    // A durable consumer exists and has NOT read anything yet — the
    // backlog is exactly what a restart must not replay.
    let mut msgs = durable::open_durable(&ctx, &stream_name, "d", vec![format!("{root}.>")])
        .await
        .unwrap();

    durable::purge_stream(&ctx, &stream_name).await.unwrap();

    let info = ctx
        .get_stream(&stream_name)
        .await
        .unwrap()
        .info()
        .await
        .unwrap()
        .clone();
    assert_eq!(info.state.messages, 0, "purge empties the buffer");

    // The consumer is still there (reset_stream would have deleted it)
    // and simply has nothing to deliver.
    ctx.get_stream(&stream_name)
        .await
        .unwrap()
        .get_consumer::<jetstream::consumer::pull::Config>("d")
        .await
        .expect("the durable consumer survives a purge");
    let nothing = tokio::time::timeout(Duration::from_secs(2), msgs.next()).await;
    assert!(
        nothing.is_err(),
        "purged messages must not be delivered to the surviving consumer"
    );

    // A post-purge publish still flows — the stream is usable, not sealed.
    ctx.publish(format!("{root}.evt"), b"{\"id\":\"after\"}".to_vec().into())
        .await
        .unwrap()
        .await
        .unwrap();
    let m = tokio::time::timeout(Duration::from_secs(5), msgs.next())
        .await
        .expect("a post-purge message should arrive")
        .expect("stream open")
        .unwrap();
    assert_eq!(m.payload.as_ref(), b"{\"id\":\"after\"}");

    ctx.delete_stream(&stream_name).await.unwrap();
}
