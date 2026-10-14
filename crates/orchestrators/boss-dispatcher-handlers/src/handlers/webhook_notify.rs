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
use std::time::Duration;
use tracing::{debug, warn};

/// How long one forward may take, connect to last byte. The dispatcher
/// awaits its handlers one at a time, so until backlog 6486b6f2
/// (2026-10-09) a host that accepted the connection and never answered
/// held every rule behind it for as long as it chose — "must never wedge
/// the dispatcher's durable queue", above, held only for a host that
/// refused. Ten seconds is the dispatcher's own client bound
/// (`boss_dispatcher::dispatcher`); the one receiver that exists, a
/// regen's callback server, answers as soon as it has read the body.
pub const TIMEOUT: Duration = Duration::from_secs(10);

pub struct WebhookNotify {
    client: reqwest::Client,
    /// `None` (or an unreachable URL) → no-op: no external party is wired.
    webhook_url: Option<String>,
}

impl WebhookNotify {
    /// A client of its OWN, never `common::api_client()`: that one stamps
    /// the estate machine token on every request, and this handler
    /// POSTs every forwarded event to whatever host the URL names — a
    /// party outside the estate. Until backlog ee96c839 (2026-09-28) it
    /// sent on `api_client()`, so the configured host itself received the
    /// token. No default headers, and redirects off so a 3xx cannot
    /// carry the event body on to a host nobody configured. Bounded by
    /// [`TIMEOUT`].
    ///
    /// A `Result`, not an `.expect()` (6486b6f2, finding 5): this is
    /// library code, and the binary that registers it says what failed
    /// at boot rather than panicking.
    pub fn new(webhook_url: Option<String>) -> reqwest::Result<Arc<Self>> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .build()?;
        Ok(Self::with_client(client, webhook_url))
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
            event_timestamp: None,
            rule_name: "forward-invoice-created".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "commerce.invoice.created".into(),
            event_payload: json!({ "id": "inv-1", "amount_cents": 285000 }),
        }
    }

    #[tokio::test]
    async fn no_url_is_a_noop() {
        let h = WebhookNotify::new(None).unwrap();
        assert!(h.invoke(&[], &ctx()).await.is_ok());
        // Empty string is treated as unset.
        let h = WebhookNotify::new(Some("   ".into())).unwrap();
        assert!(h.invoke(&[], &ctx()).await.is_ok());
    }

    #[tokio::test]
    async fn unreachable_endpoint_is_a_noop_not_a_dead_letter() {
        // Nothing is listening on this port → connection refused → Ok, never Err.
        let h = WebhookNotify::new(Some("http://127.0.0.1:1/callback".into())).unwrap();
        assert!(h.invoke(&[], &ctx()).await.is_ok());
    }

    /// Backlog 6486b6f2, finding 3: the dispatcher awaits its handlers one
    /// at a time (`boss_dispatcher::rules::handler`, the dispatch loop),
    /// so a host that takes the connection and never answers held the
    /// whole queue for as long as it liked — the client had no timeout.
    /// The production constructor is what is judged, so this waits out
    /// the real bound; the outer guard is the bound plus slack, and only
    /// an unbounded client reaches it.
    #[tokio::test]
    async fn a_host_that_never_answers_is_abandoned_within_the_bound() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/callback", listener.local_addr().unwrap());
        // Accept, read nothing, write nothing, and keep the socket open.
        let silent = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
            drop(sock);
        });
        let h = WebhookNotify::new(Some(url)).unwrap();
        let guard = TIMEOUT + std::time::Duration::from_secs(5);
        let out = tokio::time::timeout(guard, h.invoke(&[], &ctx())).await;
        silent.abort();
        let Ok(result) = out else {
            panic!(
                "webhook.notify was still waiting on a silent host after {guard:?}: the \
                 client has no timeout, so one such host wedges the dispatcher's queue"
            );
        };
        assert!(
            result.is_ok(),
            "a timed-out forward is a no-op, never a dead-letter"
        );
    }

    /// Finding 4: redirects are off so a 3xx cannot carry the event body
    /// on to a host nobody configured, and nothing held that — deleting
    /// the policy left every test green. A 302 to a second listener must
    /// come back as a response, and the second listener must see no one.
    #[tokio::test]
    async fn a_redirect_is_answered_not_followed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let elsewhere = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/callback", first.local_addr().unwrap());
        let location = format!("http://{}/stolen", elsewhere.local_addr().unwrap());
        let redirector = tokio::spawn(async move {
            let (mut sock, _) = first.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = sock.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let reply = format!(
                "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\n\
                 connection: close\r\n\r\n"
            );
            sock.write_all(reply.as_bytes()).await.unwrap();
        });
        let h = WebhookNotify::new(Some(url)).unwrap();
        assert!(
            h.invoke(&[], &ctx()).await.is_ok(),
            "a 3xx is a non-2xx: a no-op"
        );
        redirector.await.unwrap();
        let followed =
            tokio::time::timeout(std::time::Duration::from_millis(500), elsewhere.accept()).await;
        assert!(
            followed.is_err(),
            "webhook.notify followed the 302 and sent the event to the host its Location named"
        );
    }
}
