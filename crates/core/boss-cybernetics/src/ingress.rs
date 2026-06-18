//! Cross-VM S1 message ingress bridge.
//!
//! Subscribes to `boss.s1.{own_vm}.>` on the event bus, decodes each event's
//! payload as a [`Message`], and submits it to the local [`Cybernetics`]
//! instance. This is the on-ramp for cross-VM agent-to-agent messaging.
//!
//! Wire format:
//! ```text
//! subject: boss.s1.{target_vm}.{target_agent}.{message_kind}
//! payload: JSON-encoded Message
//! ```

use std::sync::Arc;

use boss_core::agent::Message;
use boss_core::event::Event;
use boss_core::port::{EventBus, EventStream};
use tokio::sync::watch;
use tracing::{debug, warn};

use crate::Cybernetics;

/// Ingress bridge: feeds the event bus into [`Cybernetics::submit`].
///
/// Two-step lifecycle avoids a subscribe race: call [`Ingress::attach`]
/// first to establish the subscription, then [`AttachedIngress::run`] to
/// drive the consume loop.
pub struct Ingress {
    cyb: Arc<Cybernetics>,
}

impl Ingress {
    pub fn new(cyb: Arc<Cybernetics>) -> Self {
        Self { cyb }
    }

    pub fn subject_pattern(vm_id: &str) -> String {
        format!("boss.s1.{vm_id}.>")
    }

    /// Subscribe now. Returns a handle the caller can spawn `.run()` on.
    pub async fn attach(self, bus: Arc<dyn EventBus>) -> Result<AttachedIngress, IngressError> {
        let pattern = Self::subject_pattern(self.cyb.vm_id());
        debug!(pattern = %pattern, "ingress subscribing");
        let stream = bus
            .subscribe(&pattern)
            .await
            .map_err(|e| IngressError::Subscribe(e.to_string()))?;
        Ok(AttachedIngress {
            cyb: self.cyb,
            stream,
        })
    }
}

/// An ingress with an active subscription, ready to consume.
pub struct AttachedIngress {
    cyb: Arc<Cybernetics>,
    stream: Box<dyn EventStream>,
}

impl AttachedIngress {
    /// Drive the ingress loop until cancelled.
    pub async fn run(mut self, mut cancel: watch::Receiver<bool>) -> Result<(), IngressError> {
        loop {
            tokio::select! {
                biased;
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        debug!("ingress shutting down");
                        return Ok(());
                    }
                }
                next = self.stream.next() => {
                    match next {
                        Some(event) => self.handle(event).await,
                        None => {
                            warn!("ingress stream closed");
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn handle(&self, event: Event) {
        let message: Message = match serde_json::from_value(event.payload.clone()) {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    error = %e,
                    event_id = %event.id,
                    event_kind = %event.kind,
                    "failed to decode S1 envelope"
                );
                return;
            }
        };
        if let Err(e) = self.cyb.submit(message).await {
            warn!(error = %e, "submit failed from ingress");
        }
    }
}

/// Build a cross-VM S1 envelope: an [`Event`] wrapping a [`Message`] ready
/// to publish via the event bus.
pub fn envelope(target_vm: &str, source: impl Into<String>, message: &Message) -> Event {
    let kind = format!(
        "boss.s1.{target_vm}.{}.{}",
        message.target.as_str(),
        message.kind
    );
    let payload = serde_json::to_value(message).expect("Message serializes");
    // Reuse the Message's timestamp — same clock the caller
    // already sourced when building the Message — so the envelope
    // event stays aligned with the message it transports.
    Event::new(source, kind, payload, message.timestamp)
}

#[derive(Debug, thiserror::Error)]
pub enum IngressError {
    #[error("subscribe failed: {0}")]
    Subscribe(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::agent::AgentId;

    #[test]
    fn subject_pattern_matches_vm_scope() {
        assert_eq!(
            Ingress::subject_pattern("os-worker-1"),
            "boss.s1.os-worker-1.>"
        );
    }

    #[test]
    fn envelope_kind_encodes_vm_agent_and_kind() {
        let msg = Message::new(
            AgentId::try_new("planner").unwrap(),
            "work.plan",
            serde_json::json!({"x": 1}),
        );
        let evt = envelope("os-manager-1", "test", &msg);
        assert_eq!(evt.kind, "boss.s1.os-manager-1.planner.work.plan");
        let decoded: Message = serde_json::from_value(evt.payload).unwrap();
        assert_eq!(decoded.id, msg.id);
    }
}
