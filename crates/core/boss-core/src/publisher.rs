//! Fire-and-forget domain event publisher.
//!
//! Wraps an `EventBus` and optionally an `AuditWriter`. On error,
//! returns false but never propagates — write operations must not
//! fail because the event bus or audit log is unavailable.
//!
//! "Never propagates" is NOT "silent": every bus or audit failure
//! logs at ERROR with the event identity. The audit log is the
//! system of record — a dropped write there means a state change
//! with no provenance, permanently unreproducible by
//! rebuild-from-log. The structural fix (audit insert joins the
//! domain transaction; the bus demotes to post-commit notification)
//! is tracked as its own workstream; this layer's job until then is
//! to make every hole loud the moment it is punched.

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

    /// Resolve the enrichment envelope for IN-TRANSACTION (outbox)
    /// event recording — the same `_actor` / `_simulated` semantics
    /// [`Self::emit_with_actor_at`] applies at publish time, captured
    /// as a value a domain repository can apply INSIDE its own
    /// transaction via [`EventStamp::event`] +
    /// `boss_events::outbox::record_event_in_tx`. This is how an
    /// outbox-migrated emitter keeps byte-identical payload
    /// enrichment with the publisher path it replaces: one resolution
    /// (task-local sim chain OR the clock probe), two delivery
    /// mechanisms.
    pub async fn stamp_with_actor_at(
        &self,
        actor: ActorId,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> EventStamp {
        let mut simulated = crate::sim_origin::is_in_sim_chain();
        if !simulated && let Some(probe) = &self.sim_probe {
            simulated = probe.simulated().await;
        }
        EventStamp {
            source: self.source.clone(),
            actor,
            simulated: if simulated || self.sim_probe.is_some() {
                Some(simulated)
            } else {
                None
            },
            timestamp,
        }
    }

    /// Publish a pre-built `Event`. Used by services like assets that
    /// already construct an `Event` via a domain bridge and need the
    /// id and timestamp on the wire to match what the rest of the
    /// service stores. Same fire-and-forget contract as `emit`:
    /// failures never propagate to the caller — but they are LOUD.
    /// A dropped audit write is a permanent hole in the system of
    /// record (the state change committed; replay-from-log cannot
    /// reproduce it), which is exactly how the 2026-07-13
    /// replay-divergence class stayed invisible: the audit trigger
    /// rejected events post-commit and the old `let _ =` here
    /// swallowed every rejection. Making the write transactional
    /// with the domain write is tracked separately; until then,
    /// every failure logs at ERROR with the event identity.
    pub async fn publish(&self, event: Event) -> bool {
        let bus_ok = match self.bus.publish(event.clone()).await {
            Ok(()) => true,
            Err(e) => {
                tracing::error!(
                    kind = %event.kind,
                    event_id = %event.id,
                    source = %self.source,
                    error = %e,
                    "event bus publish failed — downstream consumers will not see this event"
                );
                false
            }
        };
        if let Some(audit) = &self.audit
            && let Err(e) = audit.write(&event).await
        {
            tracing::error!(
                kind = %event.kind,
                event_id = %event.id,
                source = %self.source,
                error = %e,
                "audit_log write failed — state committed WITHOUT provenance; rebuild-from-log cannot reproduce this event"
            );
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
    /// failures never fail the underlying domain write — but every
    /// failure logs at ERROR (see `publish` for why silence here
    /// punched holes in the system of record). Note a failed batch
    /// audit write loses the WHOLE batch's provenance in one shot.
    pub async fn publish_batch(&self, events: Vec<Event>) {
        for event in &events {
            if let Err(e) = self.bus.publish(event.clone()).await {
                tracing::error!(
                    kind = %event.kind,
                    event_id = %event.id,
                    source = %self.source,
                    error = %e,
                    "event bus publish failed — downstream consumers will not see this event"
                );
            }
        }
        if let Some(audit) = &self.audit
            && let Err(e) = audit.write_batch(&events).await
        {
            tracing::error!(
                batch_len = events.len(),
                first_kind = %events.first().map(|e| e.kind.as_str()).unwrap_or("<empty>"),
                source = %self.source,
                error = %e,
                "audit_log batch write failed — {} events committed WITHOUT provenance; rebuild-from-log cannot reproduce them",
                events.len()
            );
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

/// The enrichment envelope for in-transaction (outbox) event
/// recording. Resolved once per request — via
/// [`DomainPublisher::stamp_with_actor_at`] when a publisher (and
/// its sim probe) is wired, or [`EventStamp::new`] when not — and
/// handed into the domain repository, which applies it to each
/// payload it builds INSIDE its transaction. Keeps the outbox path's
/// `_actor` / `_simulated` semantics byte-identical with
/// `emit_with_actor_at`, from one resolution.
#[derive(Debug, Clone)]
pub struct EventStamp {
    source: String,
    actor: ActorId,
    /// `Some(flag)` injects `_simulated: flag`; `None` leaves the key
    /// off entirely (mirrors emit: the key appears when the chain is
    /// simulated or a probe is wired at all).
    simulated: Option<bool>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl EventStamp {
    /// Publisher-less construction (test paths, adapters without a
    /// clock probe). Still honors the task-local sim chain, so a
    /// sim-originated request stamps `_simulated: true` even here.
    pub fn new(
        source: impl Into<String>,
        actor: ActorId,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let in_chain = crate::sim_origin::is_in_sim_chain();
        Self {
            source: source.into(),
            actor,
            simulated: in_chain.then_some(true),
            timestamp,
        }
    }

    /// Build the enriched `Event` for `kind` + `payload` — the in-tx
    /// analogue of `emit_with_actor_at`'s construction.
    pub fn event(&self, kind: &str, payload: serde_json::Value) -> Event {
        let mut payload = inject_actor(payload, &self.actor);
        if let Some(simulated) = self.simulated {
            payload = inject_simulated(payload, simulated);
        }
        Event::new(&self.source, kind, payload, self.timestamp)
    }
}

#[cfg(test)]
mod publish_contract_tests {
    use super::*;
    use crate::port::{EventBus, EventBusError, EventStream};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StubBus {
        ok: bool,
        published: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl EventBus for StubBus {
        async fn publish(&self, _e: Event) -> Result<(), EventBusError> {
            self.published.fetch_add(1, Ordering::SeqCst);
            if self.ok {
                Ok(())
            } else {
                Err(EventBusError::PublishFailed("bus down".into()))
            }
        }
        async fn subscribe(&self, _p: &str) -> Result<Box<dyn EventStream>, EventBusError> {
            Err(EventBusError::SubscribeFailed("stub".into()))
        }
    }

    struct StubAudit {
        ok: bool,
        writes: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl AuditWriter for StubAudit {
        async fn write(&self, _e: &Event) -> Result<(), String> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.ok {
                Ok(())
            } else {
                Err("insert rejected by audit_log_check_refs".into())
            }
        }
    }

    /// Capture everything the publisher logs on the current thread.
    /// `#[tokio::test]` runs single-threaded, so the thread-default
    /// subscriber sees the whole async call.
    struct VecWriter(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn capture_logs() -> (Arc<Mutex<Vec<u8>>>, tracing::subscriber::DefaultGuard) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer_buf = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || VecWriter(writer_buf.clone()))
            .finish();
        (buf, tracing::subscriber::set_default(subscriber))
    }
    fn logged(buf: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8_lossy(&buf.lock().unwrap()).into_owned()
    }

    fn event() -> Event {
        Event::new(
            "test-svc",
            "commerce.invoice.created",
            serde_json::json!({"id": "inv-1"}),
            chrono::Utc::now(),
        )
    }

    #[tokio::test]
    async fn audit_failure_never_fails_the_publish_but_logs_error() {
        // The load-bearing halves of the contract: a domain write must
        // not fail because audit_log is unavailable (return stays
        // bus_ok), AND the hole punched in the system of record must
        // be LOUD — a swallowed rejection is how 260 facts went
        // unreproducible before anyone noticed (2026-07-13).
        let (buf, _guard) = capture_logs();
        let audit = Arc::new(StubAudit {
            ok: false,
            writes: AtomicUsize::new(0),
        });
        let publisher = DomainPublisher::new(
            Arc::new(StubBus {
                ok: true,
                published: AtomicUsize::new(0),
            }),
            "test-svc",
        )
        .with_audit(audit.clone());

        let ok = publisher.publish(event()).await;
        assert!(ok, "audit failure must not fail the publish");
        assert_eq!(audit.writes.load(Ordering::SeqCst), 1);
        let out = logged(&buf);
        assert!(
            out.contains("ERROR"),
            "audit failure must log at ERROR: {out}"
        );
        assert!(
            out.contains("audit_log write failed"),
            "log must name the failure: {out}"
        );
        assert!(
            out.contains("commerce.invoice.created"),
            "log must carry the event kind: {out}"
        );
        assert!(
            out.contains("insert rejected by audit_log_check_refs"),
            "log must carry the underlying error: {out}"
        );
    }

    #[tokio::test]
    async fn bus_failure_returns_false_audit_still_written_and_logged() {
        // A bus outage must not lose the audit row (the system of
        // record outranks the notification bus), must surface in the
        // return value, and must log.
        let (buf, _guard) = capture_logs();
        let audit = Arc::new(StubAudit {
            ok: true,
            writes: AtomicUsize::new(0),
        });
        let publisher = DomainPublisher::new(
            Arc::new(StubBus {
                ok: false,
                published: AtomicUsize::new(0),
            }),
            "test-svc",
        )
        .with_audit(audit.clone());

        let ok = publisher.publish(event()).await;
        assert!(!ok, "bus failure must surface in the return");
        assert_eq!(
            audit.writes.load(Ordering::SeqCst),
            1,
            "audit row must still be written when the bus is down"
        );
        let out = logged(&buf);
        assert!(
            out.contains("event bus publish failed"),
            "bus failure must log: {out}"
        );
    }

    #[tokio::test]
    async fn publish_batch_logs_every_failure() {
        let (buf, _guard) = capture_logs();
        let audit = Arc::new(StubAudit {
            ok: false,
            writes: AtomicUsize::new(0),
        });
        let publisher = DomainPublisher::new(
            Arc::new(StubBus {
                ok: false,
                published: AtomicUsize::new(0),
            }),
            "test-svc",
        )
        .with_audit(audit.clone());

        publisher.publish_batch(vec![event(), event()]).await;
        let out = logged(&buf);
        assert_eq!(
            out.matches("event bus publish failed").count(),
            2,
            "each bus failure logs: {out}"
        );
        assert!(
            out.contains("audit_log batch write failed"),
            "batch audit failure logs: {out}"
        );
    }
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
