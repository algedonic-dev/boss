//! NATS runner — subscribes to all topics referenced by the rule
//! registry, feeds incoming events through the matcher, and dispatches
//! matched rules to the handler registry.
//!
//! Separate from `dispatcher::run_loop` (which handles role-based
//! step assignment) so the two concerns stay decoupled. The main
//! binary starts both; they share the same NATS connection but
//! subscribe to disjoint topic sets.

use super::expr::HelperResolver;
use super::handler::{self, HandlerRegistry};
use super::registry::{self, Registry};
use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Max events processed in parallel. A strictly-serial loop awaits each
/// event's handlers (1-2 HTTP round-trips) before pulling the next, which
/// caps the runner at ~tens of events/sec; at warp that trails job
/// generation and lets ready markers + steps pile up over a long regen.
/// This is the side-effect path (invoice / COGS / shipping / ledger), so it
/// must keep pace with the 16-wide workforce's completions. Raised 6 → 12
/// now that the cluster runs max_connections=400 (was 100): the fan-out
/// across module services has pool headroom, and a transient "too many
/// clients" blip is NAK'd + redelivered by the JetStream layer rather than
/// lost. Saturate the DB, don't error it — dead-letters reappearing is the
/// signal this went too high.
const MAX_CONCURRENT_EVENTS: usize = 12;

/// What the rules runner needs to operate.
pub struct RulesRunner {
    pub registry: Registry,
    pub handlers: HandlerRegistry,
    pub helpers: Arc<dyn HelperResolver + Send + Sync>,
}

impl RulesRunner {
    /// Compute the set of NATS topics to subscribe to. Walks the
    /// rule registry, deduplicating identical `on_event` patterns.
    pub fn subscriptions(&self) -> HashSet<String> {
        self.registry
            .rules()
            .iter()
            .map(|r| r.on_event.raw().to_string())
            .collect()
    }

    /// Bind a durable JetStream consumer covering every topic the registry
    /// references and run the match-then-dispatch loop. Runs until the
    /// message stream ends (NATS disconnect).
    ///
    /// Durability is the point: each event is ACK'd only after its handlers
    /// succeed. A handler failure NAKs the message, so the server redelivers
    /// it on a backoff schedule and the work self-heals once a transient
    /// condition clears — instead of the silent drop that orphaned Jobs
    /// under plain core-NATS subscribe.
    pub async fn run(
        &self,
        js: async_nats::jetstream::Context,
        live: std::sync::Arc<crate::liveness::DispatcherLiveness>,
    ) -> Result<()> {
        let subjects: Vec<String> = self.subscriptions().into_iter().collect();
        if subjects.is_empty() {
            info!("rules runner: no rules registered, nothing to consume");
            return Ok(());
        }
        // One consumer over the coarse (first-token) wildcards covering the
        // registry's subjects — non-overlapping, so the server accepts it and
        // each event arrives exactly once. `handle` re-matches precisely via
        // `match_event`; the few extra subjects pulled in match no rule and
        // ACK as no-ops.
        let filters = boss_nats::durable::coarse_filter_subjects(&subjects);
        info!(
            ?filters,
            durable = "dispatcher-rules",
            "rules runner: opening durable consumer"
        );
        let messages = boss_nats::durable::open_durable(
            &js,
            boss_nats::durable::STREAM_NAME,
            "dispatcher-rules",
            filters,
        )
        .await
        .context("opening durable rules consumer")?;
        // Consumer bound — the step-completion side-effect path is live (see
        // crate::liveness::DispatcherLiveness; a process that's health-200 but
        // whose rules consumer died runs zero side-effects and stalls Jobs).
        live.mark_rules_running();

        // Process events with bounded concurrency (see MAX_CONCURRENT_EVENTS).
        // `handle` only reads shared state (the rule registry + stateless
        // Arc'd handlers), so distinct events process safely in parallel;
        // a single Job's step chain stays ordered because each event only
        // fires after its predecessor's completion lands.
        messages
            .for_each_concurrent(MAX_CONCURRENT_EVENTS, |msg| {
                let live = live.clone();
                async move {
                    let msg = match msg {
                        Ok(m) => m,
                        Err(e) => {
                            warn!(error = %e, "rules runner: message stream error");
                            return;
                        }
                    };
                    let subject = msg.subject.to_string();
                    let envelope: Value = match serde_json::from_slice(&msg.payload) {
                        Ok(v) => v,
                        Err(e) => {
                            // Unparseable: redelivery can't help — ACK so it
                            // doesn't loop, and move on.
                            debug!(error = %e, subject = %subject, "skip non-JSON event (ACK)");
                            let _ = msg.ack().await;
                            return;
                        }
                    };
                    // events come as {id, timestamp, source, kind, payload}.
                    let event_id = envelope
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let payload = envelope.get("payload").cloned().unwrap_or(envelope);
                    let outcome = self.handle(&subject, &event_id, &payload).await;
                    // ACK on success; NAK (→ redeliver) on failure; dead-letter
                    // once the redelivery budget is spent. `settle` does the
                    // failure logging (deliberately NOT phrased as the
                    // health-gate's permanent-failure pattern — a transient NAK
                    // that self-heals is not a defect; only a dead-letter is).
                    boss_nats::durable::settle(&msg, outcome).await;
                    live.record_rules();
                }
            })
            .await;
        live.mark_rules_stopped();
        info!("rules runner: message stream ended");
        Ok(())
    }

    async fn handle(&self, topic: &str, event_id: &str, payload: &Value) -> Result<()> {
        let matched = registry::match_event(&self.registry, topic, payload, self.helpers.as_ref())
            .with_context(|| format!("matching event on {topic}"))?;
        if matched.is_empty() {
            return Ok(());
        }
        let results = handler::dispatch(&matched, &self.handlers, event_id, topic, payload)
            .await
            .with_context(|| format!("dispatching matched rules for {topic}"))?;
        // Collect handler failures and propagate them: a failed handler must
        // surface as an `Err` here so the caller NAKs the message and the
        // server redelivers it. The previous version logged failures but
        // returned `Ok`, which under JetStream would ACK away a dropped side
        // effect — exactly the silent-orphaning this layer exists to stop.
        //
        // NOTE: a NAK redelivers the whole event, re-running EVERY matched
        // handler — including any that already succeeded. For multi-handler
        // subjects (`step.done.production-produce`, `step.done.shipment`)
        // the handlers must therefore be idempotent on their source key, or
        // a partial failure double-applies the survivors on retry.
        let mut failures = Vec::new();
        for r in results {
            match &r.outcome {
                Ok(()) => {
                    info!(
                        rule = %r.rule_name,
                        handler = %r.handler,
                        triggering_event = %event_id,
                        "rule fired"
                    );
                }
                Err(e) => {
                    failures.push(format!("{}/{}: {}", r.rule_name, r.handler, e));
                }
            }
        }
        if !failures.is_empty() {
            anyhow::bail!(
                "{} handler(s) failed on {topic}: {}",
                failures.len(),
                failures.join("; ")
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::expr::NoHelpers;
    use super::*;

    #[test]
    fn subscriptions_dedupes_repeated_topics() {
        let toml = r#"
[[rule]]
name = "r1"
on_event = "step.done.procurement"
[[rule.do]]
handler = "h1"
[[rule]]
name = "r2"
on_event = "step.done.procurement"
[[rule.do]]
handler = "h2"
[[rule]]
name = "r3"
on_event = "inventory.parts.consumed"
[[rule.do]]
handler = "h3"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let runner = RulesRunner {
            registry: reg,
            handlers: HandlerRegistry::new(),
            helpers: Arc::new(NoHelpers),
        };
        let subs = runner.subscriptions();
        assert_eq!(subs.len(), 2);
        assert!(subs.contains("step.done.procurement"));
        assert!(subs.contains("inventory.parts.consumed"));
    }

    #[test]
    fn empty_registry_has_no_subscriptions() {
        let runner = RulesRunner {
            registry: Registry::empty(),
            handlers: HandlerRegistry::new(),
            helpers: Arc::new(NoHelpers),
        };
        assert!(runner.subscriptions().is_empty());
    }

    #[tokio::test]
    async fn handle_propagates_handler_failure() {
        // The load-bearing durable contract: a failed handler must surface
        // as `Err` from `handle` so the caller NAKs the message and the
        // server redelivers it. The pre-JetStream version logged the failure
        // and returned `Ok`, which under a durable consumer would ACK away a
        // dropped side effect — the exact silent-orphaning this layer ends.
        let toml = r#"
[[rule]]
name = "r-fail"
on_event = "step.done.billing"
[[rule.do]]
handler = "boom"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let mut handlers = HandlerRegistry::new();
        handlers.register(handler::FailingHandler::new("boom", "policy unreachable"));
        let runner = RulesRunner {
            registry: reg,
            handlers,
            helpers: Arc::new(NoHelpers),
        };
        let payload = serde_json::json!({
            "job_id": "j1", "step_id": "s1", "kind": "billing"
        });
        let res = runner.handle("step.done.billing", "evt-1", &payload).await;
        let err = res.expect_err("a failed handler must propagate as Err so the message NAKs");
        assert!(
            err.to_string().contains("boom"),
            "the propagated error should name the failed handler: {err}"
        );
    }

    #[tokio::test]
    async fn handle_is_ok_when_no_rule_matches() {
        // No matched rule = nothing to do = ACK (not a failure). Keeps the
        // consumer from NAK-looping the many events no rule cares about.
        let runner = RulesRunner {
            registry: Registry::empty(),
            handlers: HandlerRegistry::new(),
            helpers: Arc::new(NoHelpers),
        };
        let res = runner
            .handle("step.done.unmatched", "evt", &serde_json::json!({}))
            .await;
        assert!(res.is_ok(), "an unmatched event must be Ok (ACK), not Err");
    }
}
