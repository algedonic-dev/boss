//! NATS -> SSE fan-out for asset events.
//!
//! Mirrors the pattern from `boss-observability` but subscribes to
//! `asset.>` instead of `cybernetics.>`.

use std::convert::Infallible;

use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use boss_core::event::Event;
use futures::{Stream, StreamExt};
use tokio::sync::{broadcast, watch};
use tokio_stream::wrappers::BroadcastStream;
use tracing::{debug, error, warn};

const DEFAULT_CHANNEL_CAPACITY: usize = 1024;
const ASSET_SUBJECT: &str = "asset.>";

/// Broadcast hub for live asset events.
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

    /// Send an event to every live subscriber.
    pub fn publish(&self, event: Event) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribe to the live stream.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
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
    let data = serde_json::to_string(event).unwrap_or_else(|e| {
        error!(error = %e, "serializing event for SSE");
        "{}".to_string()
    });
    SseEvent::default()
        .id(event.id.to_string())
        .event(event.kind.clone())
        .data(data)
}

/// Subscribe to `asset.>` on NATS and forward every decoded event
/// into `hub`. Runs until `cancel` flips to true.
pub async fn run_nats_forwarder(
    client: async_nats::Client,
    hub: SseHub,
    mut cancel: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut sub = client
        .subscribe(ASSET_SUBJECT.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("subscribing to {ASSET_SUBJECT}: {e}"))?;

    loop {
        tokio::select! {
            _ = cancel.changed() => {
                if *cancel.borrow() { break; }
            }
            maybe_msg = sub.next() => {
                let Some(msg) = maybe_msg else { break; };
                match serde_json::from_slice::<Event>(&msg.payload) {
                    Ok(event) => {
                        debug!(kind = %event.kind, "forwarding NATS -> SSE");
                        hub.publish(event);
                    }
                    Err(e) => warn!(subject = %msg.subject, error = %e, "decode asset event"),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::time::{Duration, timeout};

    fn sample_event(kind: &str) -> Event {
        Event::new(
            "assets",
            kind.to_string(),
            json!({"serial": "SN-1"}),
            chrono::Utc::now(),
        )
    }

    #[tokio::test]
    async fn publish_reaches_subscriber() {
        let hub = SseHub::new();
        let mut rx = hub.subscribe();

        let delivered = hub.publish(sample_event("asset.received"));
        assert_eq!(delivered, 1);

        let ev = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("timed out")
            .expect("recv");
        assert_eq!(ev.kind, "asset.received");
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_not_an_error() {
        let hub = SseHub::new();
        let delivered = hub.publish(sample_event("asset.sold"));
        assert_eq!(delivered, 0);
    }
}
