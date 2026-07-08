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
    /// Schedule-triggered rules contribute nothing — they fire off the
    /// clock stream, not a NATS subscription.
    pub fn subscriptions(&self) -> HashSet<String> {
        self.registry
            .rules()
            .iter()
            .filter_map(|r| r.event_pattern().map(|p| p.raw().to_string()))
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
                    // ACK on success; NAK (→ redeliver) on transient failure —
                    // dead-letter once the budget is spent; immediate Term on a
                    // permanent failure (deterministic data error — every
                    // redelivery fails identically, so the budget is noise).
                    // `settle_classified` does the failure logging
                    // (deliberately NOT phrased as the health-gate pattern for
                    // NAKs — a transient that self-heals is not a defect; only
                    // a dead-letter is, and permanent failures log as one).
                    boss_nats::durable::settle_classified(&msg, outcome).await;
                    live.record_rules();
                }
            })
            .await;
        live.mark_rules_stopped();
        info!("rules runner: message stream ended");
        Ok(())
    }

    async fn handle(
        &self,
        topic: &str,
        event_id: &str,
        payload: &Value,
    ) -> boss_nats::durable::Settle {
        use boss_nats::durable::Settle;
        let matched =
            match registry::match_event(&self.registry, topic, payload, self.helpers.as_ref()) {
                Ok(m) => m,
                Err(e) => return Settle::Retry(format!("matching event on {topic}: {e}")),
            };
        if matched.is_empty() {
            return Settle::Ack;
        }
        let results =
            match handler::dispatch(&matched, &self.handlers, event_id, topic, payload).await {
                Ok(r) => r,
                Err(e) => {
                    return Settle::Retry(format!("dispatching matched rules for {topic}: {e}"));
                }
            };
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
        let mut all_permanent = true;
        for r in results {
            match &r.outcome {
                Ok(()) => {
                    // Per-fire log at DEBUG, not INFO: at warp the runner
                    // fires tens of rules/sec, and an INFO line each
                    // flooded syslog (26G incident, 2026-06-23). Failures
                    // still surface via the `bail!` below.
                    debug!(
                        rule = %r.rule_name,
                        handler = %r.handler,
                        triggering_event = %event_id,
                        "rule fired"
                    );
                }
                Err(e) => {
                    if !e.is_permanent() {
                        all_permanent = false;
                    }
                    failures.push(format!("{}/{}: {}", r.rule_name, r.handler, e));
                }
            }
        }
        if !failures.is_empty() {
            let msg = format!(
                "{} handler(s) failed on {topic}: {}",
                failures.len(),
                failures.join("; ")
            );
            // Term only when EVERY failure is deterministic: if any
            // transient failure is present, NAK — the idempotent re-run
            // lets the transients converge, and the ride-along permanent
            // failures re-fail harmlessly until the event either fully
            // converges or Terms on a later all-permanent pass.
            return if all_permanent {
                boss_nats::durable::Settle::Permanent(msg)
            } else {
                boss_nats::durable::Settle::Retry(msg)
            };
        }
        boss_nats::durable::Settle::Ack
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
        // A transient failure must surface as Retry so the message NAKs.
        match res {
            boss_nats::durable::Settle::Retry(msg) => assert!(
                msg.contains("boom"),
                "the propagated error should name the failed handler: {msg}"
            ),
            other => panic!("expected Retry, got {}", settle_name(&other)),
        }
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
        assert!(
            matches!(res, boss_nats::durable::Settle::Ack),
            "an unmatched event must ACK, not retry"
        );
    }

    struct FixedHandler {
        name: &'static str,
        result: fn() -> Result<(), crate::rules::handler::HandlerError>,
    }
    #[async_trait::async_trait]
    impl crate::rules::handler::Handler for FixedHandler {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn invoke(
            &self,
            _args: &[(String, crate::rules::expr::Value)],
            _ctx: &crate::rules::handler::InvocationContext,
        ) -> Result<(), crate::rules::handler::HandlerError> {
            (self.result)()
        }
    }

    fn runner_with(
        handlers: Vec<(
            &'static str,
            fn() -> Result<(), crate::rules::handler::HandlerError>,
        )>,
        rules_toml: &str,
    ) -> RulesRunner {
        let mut reg = HandlerRegistry::new();
        for (name, result) in handlers {
            reg.register(std::sync::Arc::new(FixedHandler { name, result }));
        }
        RulesRunner {
            registry: Registry::from_toml(rules_toml).unwrap(),
            handlers: reg,
            helpers: Arc::new(NoHelpers),
        }
    }

    const TWO_HANDLER_RULES: &str = r#"
[[rule]]
name = "r-perm"
on_event = "step.done.x"
[[rule.do]]
handler = "h.perm"
[[rule]]
name = "r-trans"
on_event = "step.done.x"
[[rule.do]]
handler = "h.trans"
"#;

    /// Queue item 4 (HandlerError::Permanent): all-deterministic
    /// failures Term immediately; ANY transient in the mix NAKs so the
    /// idempotent re-run can converge the transients.
    #[tokio::test]
    async fn all_permanent_failures_term_immediately() {
        use crate::rules::handler::HandlerError;
        let runner = runner_with(
            vec![
                ("h.perm", || {
                    Err(HandlerError::Permanent("422 seed typo".into()))
                }),
                ("h.trans", || {
                    Err(HandlerError::Permanent("422 second typo".into()))
                }),
            ],
            TWO_HANDLER_RULES,
        );
        match runner
            .handle("step.done.x", "e1", &serde_json::json!({}))
            .await
        {
            boss_nats::durable::Settle::Permanent(msg) => {
                assert!(msg.contains("seed typo"), "{msg}");
            }
            other => panic!("expected Permanent, got {}", settle_name(&other)),
        }
    }

    #[tokio::test]
    async fn any_transient_failure_naks_for_redelivery() {
        use crate::rules::handler::HandlerError;
        let runner = runner_with(
            vec![
                ("h.perm", || {
                    Err(HandlerError::Permanent("422 seed typo".into()))
                }),
                ("h.trans", || {
                    Err(HandlerError::Downstream("503 not yet".into()))
                }),
            ],
            TWO_HANDLER_RULES,
        );
        match runner
            .handle("step.done.x", "e1", &serde_json::json!({}))
            .await
        {
            boss_nats::durable::Settle::Retry(msg) => {
                assert!(msg.contains("2 handler(s) failed"), "{msg}");
            }
            other => panic!("expected Retry, got {}", settle_name(&other)),
        }
    }

    #[tokio::test]
    async fn success_acks() {
        let runner = runner_with(
            vec![("h.perm", || Ok(())), ("h.trans", || Ok(()))],
            TWO_HANDLER_RULES,
        );
        assert!(matches!(
            runner
                .handle("step.done.x", "e1", &serde_json::json!({}))
                .await,
            boss_nats::durable::Settle::Ack
        ));
    }

    fn settle_name(s: &boss_nats::durable::Settle) -> &'static str {
        match s {
            boss_nats::durable::Settle::Ack => "Ack",
            boss_nats::durable::Settle::Retry(_) => "Retry",
            boss_nats::durable::Settle::Permanent(_) => "Permanent",
        }
    }
}
