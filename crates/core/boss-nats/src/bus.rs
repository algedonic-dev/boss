//! NATS-backed [`EventBus`] adapter.

use async_nats::jetstream::{self, Context};
use async_nats::{Client, Subscriber};
use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::port::{EventBus, EventBusError, EventStream};
use boss_core::startup::{BusLink, hold_bus};
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// How long [`NatsEventBus::connect`] waits for the first connection
/// and the durable stream ensure before it returns a bus that is still
/// reconnecting. Five seconds is async-nats's own per-attempt
/// `connection_timeout`, so a live but slow first attempt is not cut
/// off; a dark bus costs a boot this much and no more.
pub const FIRST_CONNECT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a publish to a stream-captured subject waits for the
/// stream's PubAck before it is a retryable failure. Five seconds is
/// async-nats's own JetStream request timeout, set here by name because
/// the stream's duplicate window must outlast it
/// ([`crate::durable::DUPLICATE_WINDOW`]).
pub const PUBLISH_ACK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// EventBus adapter that publishes to and subscribes from a NATS server.
///
/// - Publish subject = `event.kind`
/// - Payload = JSON-encoded `Event`
/// - Subscribe patterns use NATS wildcards (`*`, `>`)
///
/// `subscribe` uses plain **core** NATS (at-most-once, fan-out to live
/// subscribers). A [`jetstream::Context`] rides alongside on the same
/// connection: every (re)connect ensures the durable [`crate::durable`]
/// stream exists, and `publish` sends a subject that stream captures
/// through JetStream, answering Ok only on its ack, so an Ok event is in
/// the buffer durable consumers read — see [`Self::jetstream`]. Any other
/// subject is a core publish, flushed before Ok.
#[derive(Clone)]
pub struct NatsEventBus {
    client: Client,
    // Behind an Arc so the (re)connect ensure task can hold it WEAKLY:
    // that task lives as long as the client's event callback, and a
    // strong client clone inside it would keep the connection — and its
    // reconnect loop — alive after every bus clone was dropped.
    jetstream: Arc<Context>,
    // This bus as every `/health` in the process reads it (design
    // ad9519bc part c). The registry holds it weakly, so it counts for
    // exactly as long as some clone of this bus is alive.
    _health: Arc<dyn BusLink>,
}

/// The client's connection state, as `boss_core::startup::bus_state`
/// reads it — the same state the publish guard below refuses on.
struct HealthLink(Client);

impl BusLink for HealthLink {
    fn connected(&self) -> bool {
        self.0.connection_state() == async_nats::connection::State::Connected
    }
}

/// The JetStream context on `client`, its request timeout named:
/// [`PUBLISH_ACK_WAIT`] bounds a publish's wait for its ack (and, as
/// async-nats's default always did, every JetStream API request).
fn context(client: Client) -> Context {
    let mut ctx = jetstream::new(client);
    ctx.set_timeout(PUBLISH_ACK_WAIT);
    ctx
}

impl NatsEventBus {
    /// The one place a bus is assembled, so every constructor registers
    /// it for `/health`.
    fn assemble(client: Client, jetstream: Arc<Context>) -> Self {
        let health: Arc<dyn BusLink> = Arc::new(HealthLink(client.clone()));
        hold_bus(&health);
        Self {
            client,
            jetstream,
            _health: health,
        }
    }

    /// Connect to a NATS server at the given URL (e.g. `nats://127.0.0.1:4222`).
    ///
    /// A dark bus is NOT an error (backlog 2e45840a, design ad9519bc):
    /// the client retries the first connect and reconnects without limit
    /// in the background, so a service boots and serves from Postgres
    /// while NATS is unreachable. Outage 05fd165a crash-looped the boss
    /// pod on `No route to host` with the system of record up, because
    /// this returned the transport error and every binary `?`-ed it. A
    /// URL the client cannot parse is still an error: that is config.
    ///
    /// The durable event stream is ensured on EVERY (re)connect, not once
    /// here, so a bus that comes up late still gets it. This waits up to
    /// [`FIRST_CONNECT_WAIT`] for the first ensure, so on a live bus the
    /// stream exists before the caller opens a durable consumer, as it
    /// did when the ensure ran inline. A server without JetStream still
    /// yields a fully working core-NATS bus; the failed ensure is logged.
    pub async fn connect(url: &str) -> Result<Self, EventBusError> {
        let (connected_tx, mut connected_rx) = mpsc::unbounded_channel::<()>();
        let client = async_nats::ConnectOptions::new()
            .retry_on_initial_connect()
            .max_reconnects(None)
            .event_callback(move |event| {
                let connected_tx = connected_tx.clone();
                async move {
                    if matches!(event, async_nats::Event::Connected) {
                        connected_tx.send(()).ok();
                    }
                }
            })
            .connect(url)
            .await
            .map_err(|e| EventBusError::ConnectionLost(e.to_string()))?;
        let jetstream = Arc::new(context(client.clone()));

        let weak = Arc::downgrade(&jetstream);
        let (first_tx, first_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let mut first = Some(first_tx);
            while connected_rx.recv().await.is_some() {
                let Some(ctx) = weak.upgrade() else { return };
                match crate::durable::ensure_stream(&ctx).await {
                    Ok(()) => info!("JetStream stream ensured on connect"),
                    Err(e) => warn!(
                        error = %e,
                        "JetStream stream ensure failed; durable redelivery unavailable"
                    ),
                }
                if let Some(tx) = first.take() {
                    tx.send(()).ok();
                }
            }
        });

        if tokio::time::timeout(FIRST_CONNECT_WAIT, first_rx)
            .await
            .is_err()
        {
            warn!(
                url,
                waited = ?FIRST_CONNECT_WAIT,
                "NATS not reachable yet; starting degraded — the client keeps reconnecting \
                 and the durable stream is ensured when it connects"
            );
        }
        Ok(Self::assemble(client, jetstream))
    }

    /// Wrap an already-connected client. Useful for sharing one connection
    /// across multiple adapters. Does not ensure the durable stream — use
    /// [`Self::connect`] (or call [`crate::durable::ensure_stream`]) when
    /// durable delivery is required.
    pub fn from_client(client: Client) -> Self {
        let jetstream = Arc::new(context(client.clone()));
        Self::assemble(client, jetstream)
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The JetStream context on this bus's connection. Durable consumers
    /// (`crate::durable::open_durable`) bind through it.
    pub fn jetstream(&self) -> Context {
        Context::clone(&self.jetstream)
    }

    /// Flush pending publishes to the server. Use in tests or shutdown.
    pub async fn flush(&self) -> Result<(), EventBusError> {
        self.client
            .flush()
            .await
            .map_err(|e| EventBusError::PublishFailed(e.to_string()))
    }
}

/// Sort a core-NATS publish error into the two things a caller can do
/// about it (backlog e4019cbc). `MaxPayloadExceeded` (the server's
/// max_payload, 1 MiB by default and unset in the cluster manifest) and
/// `InvalidSubject` are decided by the client against the event itself
/// before anything is sent, so they are `Refused`: retrying the same
/// event cannot succeed. `Send` is the connection, `PublishFailed`, and
/// the same event may go through later. The error's own text rides in
/// both, so the reason a row was set aside is the client's words, e.g.
/// "max payload size exceeded: Payload size limit of 1048576 exceeded
/// by message size of 1310851".
fn classify_publish_error(e: async_nats::PublishError) -> EventBusError {
    match e.kind() {
        async_nats::PublishErrorKind::MaxPayloadExceeded
        | async_nats::PublishErrorKind::InvalidSubject => EventBusError::Refused(e.to_string()),
        async_nats::PublishErrorKind::Send => EventBusError::PublishFailed(e.to_string()),
    }
}

/// The same sort for a JetStream publish and its ack. Only a payload
/// over the server's max_payload is about the event (the client checks
/// it before sending). Everything else — no stream (no responders), no
/// ack within the wait, a broken connection, a stream error answer — is
/// the bus, `PublishFailed`, and the row stays owed. An invalid subject
/// never gets here: `durable::stream_captures` sends it the core way.
fn classify_stream_publish_error(e: jetstream::context::PublishError) -> EventBusError {
    match e.kind() {
        jetstream::context::PublishErrorKind::MaxPayloadExceeded => {
            EventBusError::Refused(e.to_string())
        }
        _ => EventBusError::PublishFailed(format!("not acked by the stream: {e}")),
    }
}

#[async_trait]
impl EventBus for NatsEventBus {
    async fn publish(&self, event: Event) -> Result<(), EventBusError> {
        let subject = event.kind.clone();
        // An event that cannot be encoded never will be: Refused.
        let payload =
            serde_json::to_vec(&event).map_err(|e| EventBusError::Refused(e.to_string()))?;
        // Not connected is a transport failure, said now. async-nats would
        // queue the publish in the client and answer Ok, and the outbox
        // relay stamps `delivered_at` on Ok — marking delivered an event
        // that never left the process, and losing it on a restart before
        // the bus returns (design ad9519bc: the outbox holds the publish
        // while the bus is dark). PublishFailed keeps the row owed.
        let state = self.client.connection_state();
        if state != async_nats::connection::State::Connected {
            return Err(EventBusError::PublishFailed(format!(
                "NATS not connected ({state:?})"
            )));
        }
        // Ok is a CONFIRMED publish (backlog 2e45840a, design ad9519bc).
        // A core publish answers Ok once the client has buffered it, and
        // the relay stamped `delivered_at` on that: an event buffered just
        // before a disconnect, or sent while `BOSS_EVENTS` was absent, was
        // in audit_log, marked delivered, and never in the delivery
        // buffer. So a subject the stream captures goes through
        // JetStream and is Ok only on the stream's PubAck; no ack within
        // PUBLISH_ACK_WAIT, or no stream (no responders), is
        // PublishFailed and the row stays owed. The event id rides as
        // `Nats-Msg-Id`, so the retry of a publish whose ack was lost in
        // transit is dropped by the stream's duplicate window rather
        // than stored twice (`durable::DUPLICATE_WINDOW`).
        if crate::durable::stream_captures(&subject) {
            let publish = jetstream::message::PublishMessage::build()
                .payload(payload.into())
                .message_id(event.id.to_string());
            let ack = self
                .jetstream
                .send_publish(subject, publish)
                .await
                .map_err(classify_stream_publish_error)?;
            ack.await.map_err(classify_stream_publish_error)?;
            return Ok(());
        }
        // No stream captures this subject, so there is no ack to wait
        // for — a JetStream publish here answers no-responders forever
        // and would hold the relay on the row. Core NATS offers no more
        // than the write reaching the socket, so Ok waits for that.
        self.client
            .publish(subject, payload.into())
            .await
            .map_err(classify_publish_error)?;
        tokio::time::timeout(PUBLISH_ACK_WAIT, self.client.flush())
            .await
            .map_err(|_| {
                EventBusError::PublishFailed(format!("flush not done within {PUBLISH_ACK_WAIT:?}"))
            })?
            .map_err(|e| EventBusError::PublishFailed(e.to_string()))
    }

    async fn subscribe(&self, pattern: &str) -> Result<Box<dyn EventStream>, EventBusError> {
        let sub = self
            .client
            .subscribe(pattern.to_string())
            .await
            .map_err(|e| EventBusError::SubscribeFailed(e.to_string()))?;
        Ok(Box::new(NatsEventStream { sub }))
    }
}

struct NatsEventStream {
    sub: Subscriber,
}

#[async_trait]
impl EventStream for NatsEventStream {
    async fn next(&mut self) -> Option<Event> {
        loop {
            let msg = self.sub.next().await?;
            match serde_json::from_slice::<Event>(&msg.payload) {
                Ok(event) => return Some(event),
                Err(e) => {
                    warn!(
                        error = %e,
                        subject = %msg.subject,
                        "failed to decode event payload, dropping"
                    );
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_nats::{PublishError, PublishErrorKind};

    /// Backlog e4019cbc: the outbox relay can only step past a row the
    /// bus will never carry if the adapter says which refusals are
    /// about the row. A payload over the server max_payload and an
    /// invalid subject are properties of the EVENT, refused before a
    /// byte leaves the client, so they are `Refused`; a send failure is
    /// the connection, and stays the retryable `PublishFailed`.
    #[test]
    fn a_refusal_about_the_event_is_refused_and_a_send_failure_is_retryable() {
        for kind in [
            PublishErrorKind::MaxPayloadExceeded,
            PublishErrorKind::InvalidSubject,
        ] {
            let e = classify_publish_error(PublishError::new(kind));
            assert!(
                matches!(e, EventBusError::Refused(_)),
                "{kind:?} is deterministic, got {e:?}"
            );
        }
        let e = classify_publish_error(PublishError::new(PublishErrorKind::Send));
        assert!(
            matches!(e, EventBusError::PublishFailed(_)),
            "a send failure is transport, got {e:?}"
        );
    }

    /// Backlog 2e45840a (outage 05fd165a, design ad9519bc car 1): the
    /// boss pod crash-looped on `connecting to NATS: No route to host`
    /// while Postgres stayed up, because a first connect to a dark bus
    /// was an error and every binary `?`-ed it. A dark bus is a
    /// transport condition, so the constructor hands back a bus that
    /// keeps reconnecting in the background — bounded in time, and
    /// honestly not connected.
    #[tokio::test]
    async fn a_first_connect_to_a_dark_bus_returns_a_bus_that_keeps_trying() {
        // Held for the whole test: the client keeps reconnecting to it.
        let dark = boss_testing::dark_port();
        let url = format!("nats://{}", dark.addr);
        let bus = tokio::time::timeout(
            FIRST_CONNECT_WAIT + std::time::Duration::from_secs(10),
            NatsEventBus::connect(&url),
        )
        .await
        .expect("connect returns within a bounded time on a dark bus")
        .expect("a dark bus is not a connect error");
        assert_ne!(
            bus.client().connection_state(),
            async_nats::connection::State::Connected,
            "nothing listens at {url}; the bus must not claim a connection"
        );
    }

    /// A publish on a bus that is not connected is `PublishFailed`,
    /// never `Ok`. async-nats would otherwise queue it in the client
    /// and answer Ok, and the outbox relay stamps `delivered_at` on Ok —
    /// so a relay booted against a dark bus would mark events delivered
    /// that never left the process, and lose them if it then restarted.
    /// `PublishFailed` keeps the row owed, and the relay retries it.
    #[tokio::test]
    async fn a_publish_on_a_dark_bus_fails_rather_than_queueing() {
        let dark = boss_testing::dark_port();
        let bus = NatsEventBus::connect(&format!("nats://{}", dark.addr))
            .await
            .unwrap();
        let event = Event::new(
            "test",
            "test.dark.publish",
            serde_json::json!({}),
            chrono::Utc::now(),
        );
        let e = tokio::time::timeout(std::time::Duration::from_secs(5), bus.publish(event))
            .await
            .expect("a publish on a dark bus answers at once");
        assert!(
            matches!(e, Err(EventBusError::PublishFailed(_))),
            "a dark bus is a retryable transport failure, got {e:?}"
        );
    }

    /// Design ad9519bc part (c), backlog 2e45840a: a degraded service
    /// must look degraded. The bus is held where every binary's standard
    /// `/health` reads it, so a process booted against a dark bus says
    /// `bus: disconnected` there, and its `status` is still `ok` — the
    /// process serves from Postgres; only the bus is down.
    #[tokio::test]
    async fn a_dark_bus_reads_disconnected_on_every_health_answer() {
        let dark = boss_testing::dark_port();
        let _bus = NatsEventBus::connect(&format!("nats://{}", dark.addr))
            .await
            .unwrap();
        let health = serde_json::to_value(boss_core::startup::health_response(
            "boss-test-api",
            "0.1.0",
            "postgres",
        ))
        .unwrap();
        assert_eq!(health["capabilities"]["bus"], "disconnected", "{health}");
        assert_eq!(health["status"], "ok", "the bus never fails the process");
    }

    fn event(kind: &str) -> Event {
        Event::new("test", kind, serde_json::json!({}), chrono::Utc::now())
    }

    /// Backlog 2e45840a (design ad9519bc): the outbox relay stamps
    /// `delivered_at` on Ok, so Ok on a subject the durable stream
    /// captures must mean the STREAM stored it — its PubAck came back.
    /// A core publish answered Ok when the client buffered it, so an
    /// event buffered just before a disconnect, or sent while
    /// `BOSS_EVENTS` was absent, was stamped delivered and never reached
    /// the delivery buffer. An ack that never comes is a transport
    /// failure the relay retries.
    #[tokio::test]
    async fn a_stream_publish_whose_ack_never_comes_is_not_ok() {
        let stub = crate::nats_stub::nats_stub(crate::nats_stub::Reply::Silent).await;
        let bus = NatsEventBus::connect(&stub.url).await.unwrap();
        let e = tokio::time::timeout(
            PUBLISH_ACK_WAIT + std::time::Duration::from_secs(5),
            bus.publish(event("jobs.test.unacked")),
        )
        .await
        .expect("the ack wait is bounded");
        assert!(
            matches!(e, Err(EventBusError::PublishFailed(_))),
            "no ack is a retryable transport failure, got {e:?}"
        );
    }

    /// The stream is absent (the server answers no-responders): not Ok.
    #[tokio::test]
    async fn a_stream_publish_with_no_stream_to_store_it_is_not_ok() {
        let stub = crate::nats_stub::nats_stub(crate::nats_stub::Reply::NoStream).await;
        let bus = NatsEventBus::connect(&stub.url).await.unwrap();
        let e = bus.publish(event("jobs.test.no-stream")).await;
        assert!(
            matches!(e, Err(EventBusError::PublishFailed(_))),
            "an absent stream keeps the row owed, got {e:?}"
        );
    }

    /// An acked publish is Ok, and it carried the event id as
    /// `Nats-Msg-Id`: the relay retries a publish whose ack was lost in
    /// transit, and the stream's duplicate window drops that retry
    /// rather than handing every durable consumer the event twice.
    #[tokio::test]
    async fn an_acked_stream_publish_is_ok_and_carries_its_event_id() {
        let stub = crate::nats_stub::nats_stub(crate::nats_stub::Reply::Ack).await;
        let bus = NatsEventBus::connect(&stub.url).await.unwrap();
        let e = event("step.test.acked");
        let id = e.id;
        bus.publish(e).await.expect("an acked publish is Ok");
        let sent = stub.received();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].subject, "step.test.acked");
        assert!(
            sent[0].headers.contains(&format!("Nats-Msg-Id: {id}")),
            "{:?}",
            sent[0].headers
        );
        let carried: Event = serde_json::from_slice(&sent[0].payload).expect("an Event");
        assert_eq!(carried.id, id, "the payload is the event itself");
    }

    /// A subject no stream captures has no ack to wait for — a JetStream
    /// publish there would answer no-responders forever and hold the
    /// relay on that row. It goes out as core NATS and is flushed to the
    /// socket before Ok, which is all core NATS offers.
    #[tokio::test]
    async fn a_publish_no_stream_captures_is_flushed_and_waits_for_no_ack() {
        let stub = crate::nats_stub::nats_stub(crate::nats_stub::Reply::Silent).await;
        let bus = NatsEventBus::connect(&stub.url).await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            bus.publish(event("cybernetics.test.live-only")),
        )
        .await
        .expect("no ack is waited for")
        .expect("a flushed core publish is Ok");
        // The flush put it on the socket; give the stub a moment to read.
        let mut sent = stub.received();
        for _ in 0..50 {
            if !sent.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            sent = stub.received();
        }
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].reply, None, "a core publish asks for no reply");
    }

    /// The other half of the same decision: a URL the client cannot
    /// parse is configuration, not transport, and still fails at boot.
    #[tokio::test]
    async fn a_malformed_url_is_still_an_error() {
        let e = NatsEventBus::connect("http://127.0.0.1:4222").await;
        assert!(e.is_err(), "an http:// URL is not a NATS address");
    }
}
