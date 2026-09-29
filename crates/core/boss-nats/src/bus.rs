//! NATS-backed [`EventBus`] adapter.

use async_nats::jetstream::{self, Context};
use async_nats::{Client, Subscriber};
use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::port::{EventBus, EventBusError, EventStream};
use futures::StreamExt;
use tracing::warn;

/// EventBus adapter that publishes to and subscribes from a NATS server.
///
/// - Publish subject = `event.kind`
/// - Payload = JSON-encoded `Event`
/// - Subscribe patterns use NATS wildcards (`*`, `>`)
///
/// `publish`/`subscribe` use plain **core** NATS (at-most-once, fan-out to
/// live subscribers). A [`jetstream::Context`] rides alongside on the same
/// connection: connecting also ensures the durable [`crate::durable`] stream
/// exists, so events published to its subjects are captured for reliable
/// redelivery to durable consumers — see [`Self::jetstream`].
#[derive(Clone)]
pub struct NatsEventBus {
    client: Client,
    jetstream: Context,
}

impl NatsEventBus {
    /// Connect to a NATS server at the given URL (e.g. `nats://127.0.0.1:4222`).
    ///
    /// Also best-effort ensures the durable event stream exists. A server
    /// without JetStream enabled still yields a fully working core-NATS bus
    /// (publish/subscribe); only durable redelivery is unavailable, which is
    /// logged rather than fatal so non-dispatcher services start regardless.
    pub async fn connect(url: &str) -> Result<Self, EventBusError> {
        let client = async_nats::connect(url)
            .await
            .map_err(|e| EventBusError::ConnectionLost(e.to_string()))?;
        let jetstream = jetstream::new(client.clone());
        if let Err(e) = crate::durable::ensure_stream(&jetstream).await {
            warn!(error = %e, "JetStream stream ensure failed; durable redelivery unavailable");
        }
        Ok(Self { client, jetstream })
    }

    /// Wrap an already-connected client. Useful for sharing one connection
    /// across multiple adapters. Does not ensure the durable stream — use
    /// [`Self::connect`] (or call [`crate::durable::ensure_stream`]) when
    /// durable delivery is required.
    pub fn from_client(client: Client) -> Self {
        let jetstream = jetstream::new(client.clone());
        Self { client, jetstream }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The JetStream context on this bus's connection. Durable consumers
    /// (`crate::durable::open_durable`) bind through it.
    pub fn jetstream(&self) -> Context {
        self.jetstream.clone()
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

#[async_trait]
impl EventBus for NatsEventBus {
    async fn publish(&self, event: Event) -> Result<(), EventBusError> {
        let subject = event.kind.clone();
        // An event that cannot be encoded never will be: Refused.
        let payload =
            serde_json::to_vec(&event).map_err(|e| EventBusError::Refused(e.to_string()))?;
        self.client
            .publish(subject, payload.into())
            .await
            .map_err(classify_publish_error)?;
        Ok(())
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
}
