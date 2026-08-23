//! Per-VM Cybernetics coordinator.
//!
//! Owns the react-on-enqueue loop: ingress through [`Cybernetics::submit`],
//! dispatch chaining through [`Cybernetics::run`], with structured S3
//! telemetry emitted at every step via the event bus.

use std::sync::Arc;

use boss_core::agent::{AgentId, BudgetDecision, Message, MessageId, Outcome, RunCompletion};
use boss_core::port::{AgentDispatcher, AgentRegistry, CostLedger, DispatchError, MessageQueue};
use serde_json::json;
use tokio::sync::watch;
use tracing::{debug, error, warn};

use crate::telemetry::{
    COST_RECORDED, DISPATCH_COMPLETED, DISPATCH_DENIED, DISPATCH_REQUESTED, DISPATCH_SKIPPED,
    DISPATCH_STARTED, MESSAGE_ENQUEUED, MESSAGE_REJECTED,
};

#[derive(Debug, thiserror::Error)]
pub enum CyberneticsError {
    #[error("agent {0} is not in the registry")]
    UnknownAgent(AgentId),
    #[error("port error: {0}")]
    Port(String),
}

/// Shared configuration + ports for one Cybernetics instance.
pub struct Cybernetics {
    vm_id: String,
    queue: Arc<dyn MessageQueue>,
    ledger: Arc<dyn CostLedger>,
    dispatcher: Arc<dyn AgentDispatcher>,
    registry: Arc<dyn AgentRegistry>,
    /// Envelope builder for telemetry events — resolves the
    /// `_actor` / `_simulated` stamping (sim probe) exactly like the
    /// retired post-commit emits. Publishing itself moved to the
    /// `recorder` (outbox phase 2).
    publisher: Arc<boss_core::publisher::DomainPublisher>,
    /// OUTBOX (phase 2): where telemetry events go. Cybernetics
    /// events ARE its rebuildable state (agent dispatches, denials,
    /// cost records) but have no domain row write to join, so they
    /// record via the record-only outbox port; boss-event-relay
    /// delivers to audit_log + NATS with the same guarantees as
    /// every domain write.
    recorder: Arc<dyn boss_core::port::EventRecorder>,
}

impl Cybernetics {
    pub fn new(
        vm_id: impl Into<String>,
        queue: Arc<dyn MessageQueue>,
        ledger: Arc<dyn CostLedger>,
        dispatcher: Arc<dyn AgentDispatcher>,
        registry: Arc<dyn AgentRegistry>,
        publisher: Arc<boss_core::publisher::DomainPublisher>,
        recorder: Arc<dyn boss_core::port::EventRecorder>,
    ) -> Self {
        Self {
            vm_id: vm_id.into(),
            queue,
            ledger,
            dispatcher,
            registry,
            publisher,
            recorder,
        }
    }

    pub fn vm_id(&self) -> &str {
        &self.vm_id
    }

    /// Ingress: accept a message, enqueue it, and try to dispatch immediately.
    pub async fn submit(&self, message: Message) -> Result<MessageId, CyberneticsError> {
        let agent = message.target.clone();
        let spec = self
            .registry
            .get(&agent)
            .await
            .map_err(|e| CyberneticsError::Port(e.to_string()))?;
        if spec.is_none() {
            self.emit(
                MESSAGE_REJECTED,
                json!({
                    "agent": agent,
                    "message_id": message.id,
                    "reason": "unknown_agent",
                }),
            )
            .await;
            return Err(CyberneticsError::UnknownAgent(agent));
        }

        let message_id = self
            .queue
            .enqueue(message.clone())
            .await
            .map_err(|e| CyberneticsError::Port(e.to_string()))?;

        let depth = self.queue.depth(&agent).await.unwrap_or(0);
        self.emit(
            MESSAGE_ENQUEUED,
            json!({
                "agent": agent,
                "message_id": message_id,
                "kind": message.kind,
                "depth": depth,
            }),
        )
        .await;

        self.try_dispatch(&agent).await;
        Ok(message_id)
    }

    /// Long-running loop: consume dispatcher completions, ack, record cost,
    /// and chain into the next dispatch for the affected agent.
    pub async fn run(&self, mut cancel: watch::Receiver<bool>) -> Result<(), CyberneticsError> {
        let mut completions = self
            .dispatcher
            .completions()
            .await
            .map_err(|e| CyberneticsError::Port(e.to_string()))?;

        loop {
            tokio::select! {
                biased;
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        debug!(vm = %self.vm_id, "cybernetics loop shutting down");
                        return Ok(());
                    }
                }
                next = completions.next() => {
                    match next {
                        Some(c) => self.handle_completion(c).await,
                        None => {
                            warn!(vm = %self.vm_id, "completions stream closed");
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn handle_completion(&self, completion: RunCompletion) {
        let agent = completion.run.agent.clone();
        let cost = completion.outcome.cost();

        self.emit(
            DISPATCH_COMPLETED,
            json!({
                "agent": agent,
                "run_id": completion.run.run_id,
                "message_id": completion.run.message_id,
                "status": completion.run.status,
                "outcome": &completion.outcome,
                "finished_at": completion.finished_at,
            }),
        )
        .await;

        // Ack the queue claim. Cancelled runs may not have a claim if the
        // cancellation came from outside the normal dispatch path, but
        // here we always attempt ack and log failures.
        if let Err(e) = self
            .queue
            .ack(completion.run.claim_id, completion.outcome.clone())
            .await
        {
            warn!(error = %e, claim = %completion.run.claim_id, "ack failed");
        }

        if matches!(
            completion.outcome,
            Outcome::Success { .. } | Outcome::Failed { .. }
        ) {
            if let Err(e) = self.ledger.record(&agent, cost).await {
                error!(error = %e, agent = %agent, "cost record failed");
            } else {
                self.emit(
                    COST_RECORDED,
                    json!({
                        "agent": agent,
                        "run_id": completion.run.run_id,
                        "cost": cost,
                    }),
                )
                .await;
            }
        }

        // Chain: try to dispatch the next message for this agent.
        self.try_dispatch(&agent).await;
    }

    async fn try_dispatch(&self, agent: &AgentId) {
        let spec = match self.registry.get(agent).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                warn!(agent = %agent, "agent missing from registry on dispatch");
                return;
            }
            Err(e) => {
                error!(error = %e, agent = %agent, "registry lookup failed");
                return;
            }
        };

        // Budget gate first so we don't claim messages we can't dispatch.
        match self.ledger.check_budget(agent, &spec).await {
            Ok(BudgetDecision::Allow { .. }) => {}
            Ok(BudgetDecision::Deny { reason }) => {
                self.emit(DISPATCH_DENIED, json!({"agent": agent, "reason": reason}))
                    .await;
                return;
            }
            Err(e) => {
                error!(error = %e, agent = %agent, "budget check failed");
                return;
            }
        }

        let claimed = match self.queue.claim_next(agent).await {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(e) => {
                error!(error = %e, agent = %agent, "claim failed");
                return;
            }
        };

        self.emit(
            DISPATCH_REQUESTED,
            json!({
                "agent": agent,
                "message_id": claimed.message.id,
                "claim_id": claimed.claim_id,
                "attempt": claimed.attempt,
            }),
        )
        .await;

        match self.dispatcher.dispatch(&spec, claimed.clone()).await {
            Ok(handle) => {
                self.emit(
                    DISPATCH_STARTED,
                    json!({
                        "agent": agent,
                        "run_id": handle.run_id,
                        "message_id": handle.message_id,
                        "claim_id": handle.claim_id,
                    }),
                )
                .await;
            }
            Err(DispatchError::CapacityExceeded(_)) => {
                self.emit(
                    DISPATCH_SKIPPED,
                    json!({
                        "agent": agent,
                        "message_id": claimed.message.id,
                        "reason": "capacity_exceeded",
                    }),
                )
                .await;
                // Return the claim to the queue so another completion
                // can pick it up.
                if let Err(e) = self
                    .queue
                    .nack(claimed.claim_id, "capacity_exceeded".into())
                    .await
                {
                    warn!(error = %e, "nack after capacity exceeded failed");
                }
            }
            Err(e) => {
                error!(error = %e, agent = %agent, "dispatch failed");
                if let Err(e) = self.queue.nack(claimed.claim_id, e.to_string()).await {
                    warn!(error = %e, "nack after dispatch error failed");
                }
            }
        }
    }

    async fn emit(&self, kind: &'static str, payload: serde_json::Value) {
        // OUTBOX (phase 2): record on the transactional outbox (via
        // the EventRecorder port) instead of publishing post-hoc —
        // boss-event-relay delivers to audit_log + NATS, so agent
        // activity gets the same delivery guarantee as a domain
        // write. The publisher still builds the envelope (`_actor` /
        // `_simulated` via its sim probe) so the payload enrichment
        // is byte-identical to the retired emit path.
        //
        // We tag the payload with vm_id (the per-VM cybernetics
        // identity) so cross-VM rollups can attribute later. Source
        // is the canonical "cybernetics" string the audit log
        // already filters on for these events.
        let mut payload = payload;
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "vm_id".to_string(),
                serde_json::Value::String(self.vm_id.clone()),
            );
        }
        let actor = boss_core::actor::ActorId::Automation("cybernetics".to_string());
        // The stamp mints wall time — cluster telemetry was the worst
        // victim of the mixed-clock log (agent activity stamped on the
        // sim calendar next to wall-stamped queue accounting); sim
        // time is retired from the record (David, 2026-08-22, packet
        // a7a4cae5).
        let stamp = self.publisher.stamp_with_actor(actor).await;
        let event = stamp.event(kind, payload);
        if let Err(e) = self.recorder.record(&event).await {
            warn!(kind, error = %e, "cybernetics telemetry record failed");
        }
    }
}
