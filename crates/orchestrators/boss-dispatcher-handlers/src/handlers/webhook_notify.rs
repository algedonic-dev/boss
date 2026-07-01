//! `webhook.notify` — forward a triggering event to a configured external
//! endpoint over HTTP. This is the system's outbound integration edge: when
//! a rule matches, the dispatcher POSTs `{topic, payload}` to the registered
//! webhook URL, exactly as a real integration calls a partner's webhook.
//!
//! The system stays unaware of who is on the other end — it just notifies a
//! configured external party. During a regen that party is the
//! brewery-engine's callback server, whose CounterpartyEngine (banks,
//! suppliers, the keg courier, the tax authority) reacts to the event and
//! emits its deferred response back through the public API. In any other
//! deployment the URL is unset and this is a no-op. The simulator never
//! subscribes to the system's event stream; it only receives these
//! callbacks and replies over the public API — preserving the sim/system
//! boundary in both directions. See docs/architecture-decisions.md.
//!
//! Lenient by design: an unset URL, a non-2xx, or an unreachable endpoint is
//! a no-op, never a dead-letter — the dispatcher's durable queue must never
//! wedge on a missing external party.

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, warn};

pub struct WebhookNotify {
    client: reqwest::Client,
    /// `None` (or an unreachable URL) → no-op: no external party is wired.
    webhook_url: Option<String>,
}

impl WebhookNotify {
    pub fn new(webhook_url: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            webhook_url: webhook_url.filter(|s| !s.trim().is_empty()),
        })
    }

    pub fn with_client(client: reqwest::Client, webhook_url: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            webhook_url: webhook_url.filter(|s| !s.trim().is_empty()),
        })
    }
}

#[async_trait]
impl Handler for WebhookNotify {
    fn name(&self) -> &'static str {
        "webhook.notify"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let Some(url) = self.webhook_url.as_deref() else {
            return Ok(()); // no external party wired — nothing to notify
        };
        let body = json!({
            "topic": ctx.triggering_topic,
            "triggered_by_event_id": ctx.triggering_event_id,
            "payload": ctx.event_payload,
        });
        match self
            .client
            .post(url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => Ok(()),
            // Non-2xx / unreachable are deliberately NOT dead-letter-worthy:
            // in most deployments no external party is listening, and we must
            // never wedge the dispatcher's durable queue on a missing one.
            Ok(resp) => {
                warn!(status = %resp.status(), topic = %ctx.triggering_topic,
                      "webhook.notify non-2xx; ignoring (no party listening?)");
                Ok(())
            }
            Err(e) => {
                debug!(error = %e, topic = %ctx.triggering_topic,
                       "webhook.notify unreachable; ignoring (no party wired)");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: "forward-invoice-created".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "commerce.invoice.created".into(),
            event_payload: json!({ "id": "inv-1", "amount_cents": 285000 }),
        }
    }

    #[tokio::test]
    async fn no_url_is_a_noop() {
        let h = WebhookNotify::new(None);
        assert!(h.invoke(&[], &ctx()).await.is_ok());
        // Empty string is treated as unset.
        let h = WebhookNotify::new(Some("   ".into()));
        assert!(h.invoke(&[], &ctx()).await.is_ok());
    }

    #[tokio::test]
    async fn unreachable_endpoint_is_a_noop_not_a_dead_letter() {
        // Nothing is listening on this port → connection refused → Ok, never Err.
        let h = WebhookNotify::new(Some("http://127.0.0.1:1/callback".into()));
        assert!(h.invoke(&[], &ctx()).await.is_ok());
    }
}
