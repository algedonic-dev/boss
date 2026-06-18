//! Fire-and-forget domain event publisher.
//!
//! Wraps an `EventBus` and optionally an `AuditWriter`. On error,
//! returns false but never propagates — write operations must not
//! fail because the event bus or audit log is unavailable.

use std::sync::Arc;

use crate::actor::ActorId;
use crate::audit::AuditWriter;
use crate::event::Event;
use crate::port::EventBus;

/// Trait-erased clock probe — DomainPublisher only needs to
/// know whether the deploy is in sim mode, not the full ClockClient
/// surface. Keeping the trait local (no boss-clock-client dep
/// inside boss-core) preserves the existing crate hierarchy.
#[async_trait::async_trait]
pub trait SimulatedProbe: Send + Sync {
    /// Returns true when the deploy's clock is currently in sim
    /// mode (i.e., the audit_log row about to land represents
    /// simulated rather than real activity).
    async fn simulated(&self) -> bool;
}

/// Publishes domain events to the event bus and optionally to the audit log.
#[derive(Clone)]
pub struct DomainPublisher {
    bus: Arc<dyn EventBus>,
    audit: Option<Arc<dyn AuditWriter>>,
    source: String,
    /// Optional sim-mode probe. When set, every emit stamps
    /// `_simulated: bool` on the audit_log payload centrally, so no
    /// handler has to thread the bit through its emit call sites.
    /// When unset, emits don't carry `_simulated`.
    sim_probe: Option<Arc<dyn SimulatedProbe>>,
}

impl DomainPublisher {
    /// Create a publisher for the given service name (e.g., "catalog", "people").
    pub fn new(bus: Arc<dyn EventBus>, source: impl Into<String>) -> Self {
        Self {
            bus,
            audit: None,
            source: source.into(),
            sim_probe: None,
        }
    }

    /// Attach an audit writer for persisting events to the audit log.
    pub fn with_audit(mut self, writer: Arc<dyn AuditWriter>) -> Self {
        self.audit = Some(writer);
        self
    }

    /// Attach a sim-mode probe. Every subsequent emit (via
    /// `emit_at` or `emit_with_actor_at`) auto-fetches the probe
    /// and stamps `_simulated: <bool>` on the audit_log payload.
    /// Service binaries wire this at startup using their
    /// ClockClient (which exposes `now().simulated`); handlers
    /// don't have to thread the bit through their emit call sites.
    pub fn with_sim_probe(mut self, probe: Arc<dyn SimulatedProbe>) -> Self {
        self.sim_probe = Some(probe);
        self
    }

    /// Publish a domain event with a caller-supplied timestamp.
    /// Returns true on success, false on error. Never panics or
    /// propagates errors. The actor defaults to [`Self::default_actor`]
    /// — the current request's authenticated identity if we're inside a
    /// request, otherwise this service's own automation identity
    /// (`automation:<source>`); never an anonymous "system". Callers
    /// that know a more specific actor should use
    /// [`Self::emit_with_actor_at`] (Level-B actor-stamping invariant:
    /// every transition has a named CPU).
    ///
    /// `timestamp` MUST come from your authoritative clock
    /// (`state.clock.now().await.now`). Making it a required arg
    /// keeps the stamped time aligned with whichever clock the rest
    /// of the system is running on (sim or wall) — there is no
    /// `Utc::now()` fallback to drift to.
    ///
    /// When a `SimulatedProbe` is wired (via `with_sim_probe`)
    /// the published payload also gains `_simulated: bool`, stamped
    /// centrally rather than via per-handler edits.
    pub async fn emit_at(
        &self,
        kind: &str,
        payload: serde_json::Value,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        self.emit_with_actor_at(kind, self.default_actor(), payload, timestamp)
            .await
    }

    /// The actor for an emit that didn't name one explicitly: the
    /// current request's authenticated identity if we're inside a
    /// request scope, otherwise this service's own automation identity
    /// (`automation:<source>`). Never anonymous — there is no `system`
    /// actor; a write with no traceable authority is still attributed
    /// to the process that made it.
    fn default_actor(&self) -> ActorId {
        crate::actor_context::current_actor()
            .unwrap_or_else(|| ActorId::Automation(self.source.clone()))
    }

    /// Publish with an explicit actor + a caller-supplied
    /// timestamp. Injects `_actor` into the event payload before
    /// publishing so the audit_log row carries the named CPU.
    /// ActorId serializes to a string (`emp-032` for humans,
    /// `automation:<slug>` for named automations) per `crate::actor`'s
    /// wire format.
    ///
    /// Also injects `_simulated: bool` when a SimulatedProbe is
    /// wired (see `with_sim_probe`).
    pub async fn emit_with_actor_at(
        &self,
        kind: &str,
        actor: ActorId,
        payload: serde_json::Value,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        let mut payload = inject_actor(payload, &actor);
        // The sim marker stamps when EITHER:
        // - the current task is part of a sim chain (the
        //   incoming request had x-sim-origin: true → task-local
        //   IN_SIM_CHAIN is set), OR
        // - the clock-mode probe reports sim mode.
        // The task-local wins when set so a sim chain that hops
        // through a wall-clock service still stamps its events as
        // simulated — data-integrity invariant the correctness
        // protocol requires.
        let mut simulated = crate::sim_origin::is_in_sim_chain();
        if !simulated && let Some(probe) = &self.sim_probe {
            simulated = probe.simulated().await;
        }
        if simulated || self.sim_probe.is_some() {
            payload = inject_simulated(payload, simulated);
        }
        let event = Event::new(&self.source, kind, payload, timestamp);
        self.publish(event).await
    }

    /// Emit with actor + timestamp + the SIM marker. Injects
    /// `_simulated: bool` into the payload alongside `_actor`, so
    /// the audit_log row carries the sim-vs-real distinction
    /// forever. Handlers pass `ctx.simulated` from their
    /// `state.clock.now().await` result.
    ///
    /// Future queries filter via `payload->>'_simulated' = 'true'`
    /// to scope a report to sim activity or real activity alone.
    pub async fn emit_with_actor_simulated_at(
        &self,
        kind: &str,
        actor: ActorId,
        payload: serde_json::Value,
        timestamp: chrono::DateTime<chrono::Utc>,
        simulated: bool,
    ) -> bool {
        let payload = inject_simulated(inject_actor(payload, &actor), simulated);
        let event = Event::new(&self.source, kind, payload, timestamp);
        self.publish(event).await
    }

    /// Emit with the SIM marker but no explicit actor — defaults to
    /// [`Self::default_actor`] (the request's identity, else this
    /// service's `automation:<source>`). Convenience wrapper around
    /// `emit_with_actor_simulated_at`.
    pub async fn emit_simulated_at(
        &self,
        kind: &str,
        payload: serde_json::Value,
        timestamp: chrono::DateTime<chrono::Utc>,
        simulated: bool,
    ) -> bool {
        self.emit_with_actor_simulated_at(kind, self.default_actor(), payload, timestamp, simulated)
            .await
    }

    /// Publish a pre-built `Event`. Used by services like assets that
    /// already construct an `Event` via a domain bridge and need the
    /// id and timestamp on the wire to match what the rest of the
    /// service stores. Same fire-and-forget contract as `emit`.
    pub async fn publish(&self, event: Event) -> bool {
        let bus_ok = self.bus.publish(event.clone()).await.is_ok();
        if let Some(audit) = &self.audit {
            let _ = audit.write(&event).await;
        }
        bus_ok
    }

    /// Publish a batch of pre-built `Event`s. Routes the audit-log
    /// writes through `AuditWriter::write_batch` so a Postgres-backed
    /// writer can collapse the per-event INSERTs into one bulk
    /// statement. Bus publishes still happen one at a time because
    /// every NATS subject is per-event.
    ///
    /// Same fire-and-forget contract as `publish`: bus or audit
    /// failures are absorbed silently and never fail the underlying
    /// domain write.
    pub async fn publish_batch(&self, events: Vec<Event>) {
        for event in &events {
            let _ = self.bus.publish(event.clone()).await;
        }
        if let Some(audit) = &self.audit {
            let _ = audit.write_batch(&events).await;
        }
    }

    /// The service name this publisher was created for.
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Stamp the `_actor` field on an event payload so audit_log
/// readers can answer "who fired this transition" without joining.
/// `_actor` is the load-bearing field for the Level-B
/// actor-stamping invariant; the payload also gets `_source` so
/// debug consumers can correlate without walking back to the
/// audit_log row.
///
/// Object payloads gain the field; non-object payloads (legacy
/// arrays, scalars) are wrapped in a `{ _actor, value }` object so
/// the field is always reachable. The wrap path is rare — every
/// modern emitter uses an object — but it keeps the invariant
/// universal.
/// Add the `_simulated: bool` field to an event payload alongside
/// `_actor`. Same shape as `inject_actor` — mutates in place when
/// the payload is an object, wraps non-object payloads in
/// `{ _simulated, value }`. Replayed events that already carry an
/// explicit `_simulated` keep their original value (matching the
/// rebuilder semantics for `_actor`).
pub fn inject_simulated(mut payload: serde_json::Value, simulated: bool) -> serde_json::Value {
    if let serde_json::Value::Object(ref mut map) = payload {
        map.entry("_simulated".to_string())
            .or_insert(serde_json::Value::Bool(simulated));
        return payload;
    }
    serde_json::json!({ "_simulated": simulated, "value": payload })
}

fn inject_actor(mut payload: serde_json::Value, actor: &ActorId) -> serde_json::Value {
    let actor_value = serde_json::to_value(actor).unwrap_or(serde_json::Value::Null);
    if let serde_json::Value::Object(ref mut map) = payload {
        // Don't overwrite an explicit `_actor` already on the
        // payload — that's how a rebuilder replays a historical
        // event with the recorded actor preserved.
        map.entry("_actor".to_string()).or_insert(actor_value);
        return payload;
    }
    serde_json::json!({
        "_actor": actor_value,
        "value": payload,
    })
}

#[cfg(test)]
mod inject_tests {
    use super::*;

    #[test]
    fn injects_into_object_payload() {
        let p = serde_json::json!({"foo": "bar"});
        let out = inject_actor(p, &ActorId::Human("emp-1".into()));
        assert_eq!(out["_actor"], serde_json::Value::String("emp-1".into()));
        assert_eq!(out["foo"], "bar");
    }

    #[test]
    fn preserves_explicit_actor_on_replay() {
        let p = serde_json::json!({"foo": "bar", "_actor": "emp-5"});
        let out = inject_actor(p, &ActorId::Automation("rebuilder".into()));
        // Replayed event keeps its original actor.
        assert_eq!(out["_actor"], "emp-5");
    }

    #[test]
    fn wraps_non_object_payload() {
        let p = serde_json::json!(["a", "b"]);
        let out = inject_actor(p, &ActorId::Automation("platform".into()));
        assert_eq!(out["_actor"], "automation:platform");
        assert_eq!(out["value"], serde_json::json!(["a", "b"]));
    }

    #[test]
    fn inject_simulated_adds_true_field() {
        let p = serde_json::json!({"foo": "bar"});
        let out = inject_simulated(p, true);
        assert_eq!(out["_simulated"], true);
        assert_eq!(out["foo"], "bar");
    }

    #[test]
    fn inject_simulated_adds_false_field() {
        let p = serde_json::json!({"foo": "bar"});
        let out = inject_simulated(p, false);
        assert_eq!(out["_simulated"], false);
    }

    #[test]
    fn inject_simulated_preserves_replay_value() {
        // Replayed event keeps its original _simulated flag — same
        // semantics as the _actor preservation.
        let p = serde_json::json!({"foo": "bar", "_simulated": true});
        let out = inject_simulated(p, false);
        assert_eq!(out["_simulated"], true);
    }

    #[test]
    fn inject_simulated_wraps_non_object_payload() {
        let p = serde_json::json!(["a", "b"]);
        let out = inject_simulated(p, true);
        assert_eq!(out["_simulated"], true);
        assert_eq!(out["value"], serde_json::json!(["a", "b"]));
    }
}
