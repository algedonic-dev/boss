//! NATS → SSE fan-out.
//!
//! A [`SseHub`] holds a tokio broadcast channel. Events published into the hub
//! are delivered to every connected SSE client. A background task subscribes
//! to `cybernetics.>` on NATS and publishes each decoded event into the hub.

use std::convert::Infallible;

use async_nats::Client;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use boss_core::event::Event;
use futures::{Stream, StreamExt};
use tokio::sync::{broadcast, watch};
use tokio_stream::wrappers::BroadcastStream;
use tracing::{debug, error, warn};

const DEFAULT_CHANNEL_CAPACITY: usize = 1024;
const CYBERNETICS_SUBJECT: &str = "cybernetics.>";
const AGENT_SUBJECT: &str = "agent.>";

/// Fan-out hub for live S3 telemetry events.
#[derive(Clone)]
pub struct SseHub {
    tx: broadcast::Sender<Event>,
}

impl SseHub {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CHANNEL_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Send an event to every live subscriber. Returns how many received it.
    /// Dropped events (no subscribers) are not an error.
    pub fn publish(&self, event: Event) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribe to the live stream. Each call returns a fresh receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Build a stream suitable for an axum SSE response.
    pub fn sse_stream(
        &self,
    ) -> impl Stream<Item = Result<SseEvent, Infallible>> + Send + 'static + use<> {
        let rx = self.subscribe();
        BroadcastStream::new(rx).filter_map(|item| async move {
            match item {
                Ok(event) => Some(Ok(event_to_sse(&event))),
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    warn!(skipped = n, "SSE client lagging, events dropped");
                    None
                }
            }
        })
    }

    /// Handler-ready SSE response with keep-alive.
    pub fn sse_response(
        &self,
    ) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>> + Send + 'static + use<>> {
        Sse::new(self.sse_stream()).keep_alive(KeepAlive::default())
    }
}

impl Default for SseHub {
    fn default() -> Self {
        Self::new()
    }
}

fn event_to_sse(event: &Event) -> SseEvent {
    // `.kind` becomes the SSE event type so the client can route by subject.
    let data = serde_json::to_string(event).unwrap_or_else(|e| {
        error!(error = %e, "serializing event for SSE");
        "{}".to_string()
    });
    SseEvent::default()
        .id(event.id.to_string())
        .event(event.kind.clone())
        .data(data)
}

/// Subscribe to `cybernetics.>` on NATS and forward every decoded event into
/// `hub`. Runs until `cancel` flips to true. Malformed messages are logged
/// and skipped.
pub async fn run_nats_forwarder(
    client: Client,
    hub: SseHub,
    mut cancel: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut cyb_sub = client
        .subscribe(CYBERNETICS_SUBJECT.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("subscribing to {CYBERNETICS_SUBJECT}: {e}"))?;
    let mut agent_sub = client
        .subscribe(AGENT_SUBJECT.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("subscribing to {AGENT_SUBJECT}: {e}"))?;

    loop {
        tokio::select! {
            _ = cancel.changed() => {
                if *cancel.borrow() { break; }
            }
            maybe_msg = cyb_sub.next() => {
                let Some(msg) = maybe_msg else { break; };
                if let Ok(event) = serde_json::from_slice::<Event>(&msg.payload) {
                    debug!(kind = %event.kind, "forwarding NATS → SSE");
                    hub.publish(event);
                }
            }
            maybe_msg = agent_sub.next() => {
                let Some(msg) = maybe_msg else { break; };
                if let Ok(event) = serde_json::from_slice::<Event>(&msg.payload) {
                    debug!(kind = %event.kind, "forwarding agent NATS → SSE");
                    hub.publish(event);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::pin_mut;
    use serde_json::json;
    use tokio::time::{Duration, timeout};

    fn sample_event(kind: &str) -> Event {
        Event::new(
            "cybernetics/test-vm",
            kind.to_string(),
            json!({"agent": "develop"}),
            chrono::Utc::now(),
        )
    }

    #[tokio::test]
    async fn publish_reaches_subscriber() {
        let hub = SseHub::new();
        let mut rx = hub.subscribe();

        let delivered = hub.publish(sample_event("cybernetics.message.enqueued"));
        assert_eq!(delivered, 1);

        let ev = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("timed out")
            .expect("recv");
        assert_eq!(ev.kind, "cybernetics.message.enqueued");
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_not_an_error() {
        let hub = SseHub::new();
        let delivered = hub.publish(sample_event("cybernetics.cost.recorded"));
        assert_eq!(delivered, 0);
    }

    #[tokio::test]
    async fn multiple_subscribers_each_receive_events() {
        let hub = SseHub::new();
        let mut a = hub.subscribe();
        let mut b = hub.subscribe();
        assert_eq!(hub.subscriber_count(), 2);

        hub.publish(sample_event("cybernetics.dispatch.started"));

        let ea = timeout(Duration::from_millis(100), a.recv())
            .await
            .unwrap()
            .unwrap();
        let eb = timeout(Duration::from_millis(100), b.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ea.kind, "cybernetics.dispatch.started");
        assert_eq!(eb.kind, "cybernetics.dispatch.started");
    }

    #[tokio::test]
    async fn sse_stream_yields_events_in_order() {
        let hub = SseHub::new();
        let stream = hub.sse_stream();
        pin_mut!(stream);

        hub.publish(sample_event("cybernetics.message.enqueued"));
        hub.publish(sample_event("cybernetics.dispatch.started"));

        let first = timeout(Duration::from_millis(100), stream.next())
            .await
            .unwrap();
        let second = timeout(Duration::from_millis(100), stream.next())
            .await
            .unwrap();
        assert!(first.is_some());
        assert!(second.is_some());
    }
}
