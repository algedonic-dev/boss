//! Handler trait + dispatch layer.
//!
//! Handlers are the side-effect vocabulary the rule registry composes.
//! Each one is a registered Rust function with a stable name; rules
//! invoke them by name with args evaluated to concrete `Value`s.
//!
//! This slice is the pure-trait + dispatch loop + in-memory recording
//! handler for tests. Real handlers that make HTTP calls (jobs.spawn,
//! inventory.po.place, commerce.invoice.issue, etc.) land in later
//! passes; the registry + trait shape locks down here.

use super::expr::Value;
use super::registry::MatchedRule;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Invocation context — what a handler gets when it fires
// ---------------------------------------------------------------------------

/// Per-invocation context. Carries the actor provenance the
/// audit_log shape established in the design doc:
/// every dispatcher-fired event has `actor = rule:<rule-name>`,
/// `triggered_by_event_id = <upstream id>`, `executed_by =
/// automation:dispatcher`. Real handlers will stamp these onto every
/// downstream event they emit.
///
/// `event_payload` carries the full inner payload of the
/// triggering event so handlers that need structured access (step
/// metadata arrays, nested objects) can read it directly rather
/// than having to pass everything through the args-eval DSL. Args
/// stay for scalar parameterization (reason, due_days, etc.); the
/// payload is for "the thing the upstream event is about."
///
/// `event_timestamp` is the triggering event's own envelope `timestamp`
/// — the instant the fact was recorded, not the instant the dispatcher
/// got round to it (backlog eabc5943). A handler that stamps a time
/// onto what it writes reads it through [`InvocationContext::firing_instant`],
/// never off its own clock: a consumer's clock makes the record say
/// when it was READ, and a replay says something different each time.
/// `None` only where the delivery carried no readable timestamp.
#[derive(Debug, Clone)]
pub struct InvocationContext {
    pub rule_name: String,
    pub triggering_event_id: String,
    pub triggering_topic: String,
    pub event_payload: serde_json::Value,
    pub event_timestamp: Option<DateTime<Utc>>,
}

impl InvocationContext {
    /// The instant this firing is about: the tick's `_at` when the clock
    /// fired it, else the triggering event's own `timestamp`.
    ///
    /// REFUSED — permanently, naming both fields — when neither is
    /// present. It used to fall back to the wall clock, which stamped a
    /// dead-letter item closed by `events.outbox.redelivered` with the
    /// dispatcher's time of consumption rather than the act's (backlog
    /// eabc5943): a provenance defect that no redelivery repairs, so no
    /// redelivery is spent on it.
    pub fn firing_instant(&self) -> Result<DateTime<Utc>, HandlerError> {
        let at = self
            .event_payload
            .get("_at")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        at.or(self.event_timestamp).ok_or_else(|| {
            HandlerError::Permanent(format!(
                "rule {:?} on {} (event {}): no instant to stamp — the payload carries no \
                 `_at` and the event no `timestamp`; refusing rather than stamp the \
                 dispatcher's own clock",
                self.rule_name, self.triggering_topic, self.triggering_event_id
            ))
        })
    }

    /// The DAY this firing is about: a clock-day firing's `_day` — the
    /// only thing the schedule runner hands a day rule — else the date of
    /// [`Self::firing_instant`].
    ///
    /// For a handler whose question is a day ("has this week's retro
    /// opened?"), so a day-fired rule answers it for the day the calendar
    /// fired, not the day the dispatcher consumed the firing — a catch-up
    /// replay of Monday is still Monday (backlog 2b03a2df). Refused,
    /// permanently and by name, when there is neither.
    pub fn firing_day(&self) -> Result<NaiveDate, HandlerError> {
        match self.payload_day() {
            Some(day) => Ok(day),
            None => self
                .firing_instant()
                .map(|t| t.date_naive())
                .map_err(|_| self.no_anchor()),
        }
    }

    /// The instant a handler MEASURES against when a clock day may fire
    /// it: [`Self::firing_instant`] when the firing carries one, else the
    /// midnight (00:00Z) that opens [`Self::firing_day`].
    ///
    /// A clock-day firing names no instant on purpose (eabc5943), so a
    /// day-fired handler that measures ages defines its anchor here, once:
    /// the midnight is the instant a day rule falls DUE (the schedule
    /// runner's `next_due`), a fact of the calendar rather than of
    /// whichever process read it, and the same on every replay. Backlog
    /// 2b03a2df, whose daily cadence sweep measured silence off the
    /// clock service at consumption time.
    pub fn firing_instant_or_day_start(&self) -> Result<DateTime<Utc>, HandlerError> {
        if let Ok(at) = self.firing_instant() {
            return Ok(at);
        }
        self.payload_day()
            .map(|d| d.and_time(NaiveTime::MIN).and_utc())
            .ok_or_else(|| self.no_anchor())
    }

    fn payload_day(&self) -> Option<NaiveDate> {
        self.event_payload
            .get("_day")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
    }

    fn no_anchor(&self) -> HandlerError {
        HandlerError::Permanent(format!(
            "rule {:?} on {} (event {}): no day and no instant to anchor on — the payload \
             carries no readable `_day` or `_at` and the event no `timestamp`; refusing rather \
             than read the dispatcher's own clock",
            self.rule_name, self.triggering_topic, self.triggering_event_id
        ))
    }
}

// ---------------------------------------------------------------------------
// Handler trait
// ---------------------------------------------------------------------------

/// One side-effect handler. The trait is async because real handlers
/// make HTTP calls; in-memory test handlers can return immediately.
#[async_trait]
pub trait Handler: Send + Sync {
    /// Stable name used in rule registry rows (`handler = "..."`).
    fn name(&self) -> &'static str;

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError>;
}

#[derive(Debug, Error)]
pub enum HandlerError {
    #[error("missing required arg {0:?}")]
    MissingArg(String),
    #[error("arg {arg:?} expected {expected}, got {got}")]
    BadArgType {
        arg: String,
        expected: &'static str,
        got: &'static str,
    },
    #[error("downstream call failed: {0}")]
    Downstream(String),
    /// A deterministic request-data error — the call will fail
    /// identically on every redelivery (HTTP 422 by house contract:
    /// services answer 422 for semantic validation failures like an
    /// unknown account code from a seed typo; convergent conflicts use
    /// 409 and not-yet-projected reads 404, both of which stay
    /// retryable). The runner terminates the event immediately instead
    /// of burning the redelivery budget on the same failure.
    #[error("permanent: {0}")]
    Permanent(String),
}

impl HandlerError {
    /// Errors that cannot change on redelivery: bad rule authoring
    /// (missing/mistyped args) and downstream-declared data errors.
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            Self::Permanent(_) | Self::MissingArg(_) | Self::BadArgType { .. }
        )
    }
}

// ---------------------------------------------------------------------------
// Handler argument lookup helpers
// ---------------------------------------------------------------------------

/// Args come from the matcher as an ordered list. Real handlers
/// usually want random-access lookup by name; this helper does the
/// linear scan + type check in one go.
pub fn arg<'a>(args: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    args.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// String-typed arg lookup with consistent error shape.
pub fn arg_string<'a>(args: &'a [(String, Value)], name: &str) -> Result<&'a str, HandlerError> {
    match arg(args, name) {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(HandlerError::BadArgType {
            arg: name.to_string(),
            expected: "string",
            got: other.kind(),
        }),
        None => Err(HandlerError::MissingArg(name.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Handler registry
// ---------------------------------------------------------------------------

/// Registered handlers keyed by name. Lookups are O(1).
///
/// Handlers are owned via Arc so the dispatcher can hold a single
/// registry and clone Arcs into per-task scopes without surfacing
/// lifetime hairballs.
#[derive(Clone, Default)]
pub struct HandlerRegistry {
    handlers: HashMap<String, Arc<dyn Handler>>,
}

impl HandlerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, handler: Arc<dyn Handler>) {
        self.handlers.insert(handler.name().to_string(), handler);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Handler>> {
        self.handlers.get(name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.handlers.keys().map(String::as_str).collect()
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Outcome of one handler invocation. The dispatcher collects these
/// per matched rule so the caller (or audit_log) can see exactly which
/// handlers fired and which failed.
#[derive(Debug)]
pub struct InvocationResult {
    pub rule_name: String,
    pub handler: String,
    pub outcome: Result<(), HandlerError>,
}

/// Outcome class for when a rule named a handler that wasn't registered.
#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("rule {rule:?} named handler {handler:?} which is not registered")]
    UnknownHandler { rule: String, handler: String },
}

/// Fire every invocation in every matched rule, in declaration order,
/// against the registered handlers. Handler errors don't abort the
/// dispatch — each invocation's result lands in the returned vec so
/// the caller can decide what to do (retry, alert, log).
///
/// UnknownHandler IS a hard error: it represents a registry/code
/// drift that the operator should see immediately rather than have
/// silently swallowed.
pub async fn dispatch(
    matched: &[MatchedRule],
    handlers: &HandlerRegistry,
    triggering_event_id: &str,
    triggering_topic: &str,
    event_payload: &serde_json::Value,
    event_timestamp: Option<DateTime<Utc>>,
) -> Result<Vec<InvocationResult>, DispatchError> {
    let mut out = Vec::new();
    for m in matched {
        let ctx = InvocationContext {
            rule_name: m.rule_name.clone(),
            triggering_event_id: triggering_event_id.to_string(),
            triggering_topic: triggering_topic.to_string(),
            event_payload: event_payload.clone(),
            event_timestamp,
        };
        for inv in &m.invocations {
            let Some(h) = handlers.get(&inv.handler) else {
                return Err(DispatchError::UnknownHandler {
                    rule: m.rule_name.clone(),
                    handler: inv.handler.clone(),
                });
            };
            // Inherit the triggering event's partition for the whole
            // invocation. Downstream services decide `_simulated` from
            // the `x-sim-origin` header, and the handlers read this
            // task-local to set it — so a side effect of simulated
            // work is simulated, and a side effect of a real user's
            // action is real. The chain is "not real" (packet
            // 508cc38c): a shadow packet's side effects — until car 2
            // makes handlers skip shadow entirely — are marked exactly
            // as a simulated packet's are, never as real work.
            //
            // Without this the dispatcher's writes fell back to
            // clock-mode, which on a sim deployment marks EVERYTHING
            // simulated: 251,609 dispatcher-rule events, 57% of the
            // log, were marked that way with no reference to what
            // actually triggered them.
            let partition = boss_core::partition::Partition::from_event_payload(event_payload);
            let outcome = boss_core::sim_origin::with_sim_chain(
                partition.fails_closed(),
                h.invoke(&inv.args, &ctx),
            )
            .await;
            out.push(InvocationResult {
                rule_name: m.rule_name.clone(),
                handler: inv.handler.clone(),
                outcome,
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// RecordingHandler — for tests and for the integration-test fake
// ---------------------------------------------------------------------------

/// Handler that records every invocation it received. Used by tests
/// to assert what the dispatch loop actually fired. Also useful in
/// integration tests as a fake side-effect stand-in.
pub struct RecordingHandler {
    name: &'static str,
    pub calls: tokio::sync::Mutex<Vec<RecordedCall>>,
}

#[derive(Debug, Clone)]
pub struct RecordedCall {
    pub args: Vec<(String, Value)>,
    pub rule_name: String,
    pub triggering_event_id: String,
    pub triggering_topic: String,
    pub event_timestamp: Option<DateTime<Utc>>,
}

impl RecordingHandler {
    pub fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name,
            calls: tokio::sync::Mutex::new(Vec::new()),
        })
    }

    pub async fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().await.clone()
    }
}

#[async_trait]
impl Handler for RecordingHandler {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        self.calls.lock().await.push(RecordedCall {
            args: args.to_vec(),
            rule_name: ctx.rule_name.clone(),
            triggering_event_id: ctx.triggering_event_id.clone(),
            triggering_topic: ctx.triggering_topic.clone(),
            event_timestamp: ctx.event_timestamp,
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FailingHandler — for tests that need to exercise the error path
// ---------------------------------------------------------------------------

pub struct FailingHandler {
    name: &'static str,
    msg: String,
}

impl FailingHandler {
    pub fn new(name: &'static str, msg: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            name,
            msg: msg.into(),
        })
    }
}

#[async_trait]
impl Handler for FailingHandler {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        _ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        Err(HandlerError::Downstream(self.msg.clone()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::expr::{HelperResolver, NoHelpers, Value};
    use super::super::registry::{self, Registry};
    use super::*;
    use serde_json::json;

    fn run(reg: &Registry, topic: &str, payload: &serde_json::Value) -> Vec<MatchedRule> {
        registry::match_event(reg, topic, payload, &NoHelpers).matched
    }

    fn run_with_helpers(
        reg: &Registry,
        topic: &str,
        payload: &serde_json::Value,
        h: &dyn HelperResolver,
    ) -> Vec<MatchedRule> {
        registry::match_event(reg, topic, payload, h).matched
    }

    // ----- registry mechanics -----

    #[tokio::test]
    async fn registers_and_looks_up_handler() {
        let mut reg = HandlerRegistry::new();
        let h = RecordingHandler::new("inventory.po.place");
        reg.register(h.clone());
        assert!(reg.get("inventory.po.place").is_some());
        assert!(reg.get("nope").is_none());
    }

    // ----- dispatch happy path -----

    #[tokio::test]
    async fn dispatches_single_matched_rule_to_recording_handler() {
        let toml = r#"
[[rule]]
name = "place-po-on-procurement"
on_event = "step.done.procurement"
[[rule.do]]
handler = "inventory.po.place"
args = { vendor = "subject.id" }
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let payload = json!({ "subject": { "id": "vnd-001" } });
        let matched = run(&reg, "step.done.procurement", &payload);

        let recorder = RecordingHandler::new("inventory.po.place");
        let mut hreg = HandlerRegistry::new();
        hreg.register(recorder.clone());

        let recorded_at = chrono::DateTime::parse_from_rfc3339("2026-09-28T04:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let results = dispatch(
            &matched,
            &hreg,
            "evt-123",
            "step.done.procurement",
            &serde_json::json!({}),
            Some(recorded_at),
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].outcome.is_ok());

        let calls = recorder.calls().await;
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.rule_name, "place-po-on-procurement");
        assert_eq!(c.triggering_event_id, "evt-123");
        assert_eq!(c.triggering_topic, "step.done.procurement");
        assert_eq!(
            c.event_timestamp,
            Some(recorded_at),
            "the handler sees the event's own timestamp (eabc5943)"
        );
        assert_eq!(
            c.args,
            vec![("vendor".to_string(), Value::String("vnd-001".into()))]
        );
    }

    // ----- multi-handler / multi-rule -----

    #[tokio::test]
    async fn dispatches_multiple_rules_in_declaration_order() {
        let toml = r#"
[[rule]]
name = "r-second"
on_event = "step.done.procurement"
[[rule.do]]
handler = "h1"
[[rule]]
name = "r-first-but-declared-second"
on_event = "step.done.procurement"
[[rule.do]]
handler = "h2"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let payload = json!({});
        let matched = run(&reg, "step.done.procurement", &payload);

        let h1 = RecordingHandler::new("h1");
        let h2 = RecordingHandler::new("h2");
        let mut hreg = HandlerRegistry::new();
        hreg.register(h1.clone());
        hreg.register(h2.clone());

        let results = dispatch(
            &matched,
            &hreg,
            "evt-1",
            "step.done.procurement",
            &serde_json::json!({}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].rule_name, "r-second");
        assert_eq!(results[1].rule_name, "r-first-but-declared-second");
    }

    #[tokio::test]
    async fn rule_with_multiple_do_steps_dispatches_each_in_order() {
        let toml = r#"
[[rule]]
name = "place-and-notify"
on_event = "step.done.procurement"
[[rule.do]]
handler = "inventory.po.place"
[[rule.do]]
handler = "audit.log"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let payload = json!({});
        let matched = run(&reg, "step.done.procurement", &payload);

        let h1 = RecordingHandler::new("inventory.po.place");
        let h2 = RecordingHandler::new("audit.log");
        let mut hreg = HandlerRegistry::new();
        hreg.register(h1.clone());
        hreg.register(h2.clone());

        let results = dispatch(
            &matched,
            &hreg,
            "evt-1",
            "step.done.procurement",
            &serde_json::json!({}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].handler, "inventory.po.place");
        assert_eq!(results[1].handler, "audit.log");
    }

    // ----- unknown handler is a hard error -----

    #[tokio::test]
    async fn unknown_handler_is_dispatch_error() {
        let toml = r#"
[[rule]]
name = "rule-naming-ghost"
on_event = "x"
[[rule.do]]
handler = "ghost.handler"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let matched = run(&reg, "x", &json!({}));

        let hreg = HandlerRegistry::new();
        let err = dispatch(&matched, &hreg, "evt-1", "x", &serde_json::json!({}), None)
            .await
            .unwrap_err();
        match err {
            DispatchError::UnknownHandler { rule, handler } => {
                assert_eq!(rule, "rule-naming-ghost");
                assert_eq!(handler, "ghost.handler");
            }
        }
    }

    // ----- handler error doesn't abort dispatch -----

    #[tokio::test]
    async fn handler_error_lands_in_result_but_dispatch_continues() {
        let toml = r#"
[[rule]]
name = "r1"
on_event = "x"
[[rule.do]]
handler = "will.fail"
[[rule]]
name = "r2"
on_event = "x"
[[rule.do]]
handler = "will.succeed"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let matched = run(&reg, "x", &json!({}));

        let failer = FailingHandler::new("will.fail", "downstream 500");
        let succer = RecordingHandler::new("will.succeed");
        let mut hreg = HandlerRegistry::new();
        hreg.register(failer);
        hreg.register(succer.clone());

        let results = dispatch(&matched, &hreg, "evt-1", "x", &serde_json::json!({}), None)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(matches!(
            results[0].outcome,
            Err(HandlerError::Downstream(_))
        ));
        assert!(results[1].outcome.is_ok());
        // The successful handler did receive its call:
        assert_eq!(succer.calls().await.len(), 1);
    }

    // ----- the firing instant (eabc5943) -----

    fn firing(payload: serde_json::Value, ts: Option<&str>) -> InvocationContext {
        InvocationContext {
            rule_name: "close-the-dead-letter-item-when-the-row-is-redelivered".into(),
            triggering_event_id: "evt-act".into(),
            triggering_topic: "events.outbox.redelivered".into(),
            event_payload: payload,
            event_timestamp: ts.map(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .unwrap()
                    .with_timezone(&Utc)
            }),
        }
    }

    /// A clock firing is about its tick: `_at` wins.
    #[test]
    fn the_ticks_at_is_the_firing_instant_when_the_clock_fired_it() {
        let ctx = firing(
            json!({"_at": "2026-09-17T10:05:00+00:00"}),
            Some("2026-09-17T10:05:03Z"),
        );
        assert_eq!(
            ctx.firing_instant().unwrap().to_rfc3339(),
            "2026-09-17T10:05:00+00:00"
        );
    }

    /// Any other firing is about the fact that triggered it: the event's
    /// own timestamp, not the moment the dispatcher read it.
    #[test]
    fn the_events_timestamp_is_the_firing_instant_of_a_fact() {
        let ctx = firing(json!({"outbox_id": 4242}), Some("2026-09-28T04:31:07Z"));
        assert_eq!(
            ctx.firing_instant().unwrap().to_rfc3339(),
            "2026-09-28T04:31:07+00:00"
        );
    }

    /// Neither: refused, permanently, naming both fields — never the
    /// dispatcher's own clock, which is the provenance defect.
    #[test]
    fn no_instant_is_refused_permanently_rather_than_read_off_the_wall_clock() {
        let err = firing(json!({"outbox_id": 4242}), None)
            .firing_instant()
            .expect_err("no _at and no timestamp");
        assert!(err.is_permanent(), "{err}");
        let said = err.to_string();
        for named in [
            "`_at`",
            "`timestamp`",
            "events.outbox.redelivered",
            "evt-act",
        ] {
            assert!(said.contains(named), "names {named:?}: {said}");
        }
    }

    // ----- the firing DAY, and a day firing's anchor (2b03a2df) -----

    fn day_firing(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "cadence-silence-sweep-daily".into(),
            triggering_event_id: "clock-day:2026-09-21".into(),
            triggering_topic: "clock.day".into(),
            event_payload: payload,
            event_timestamp: None,
        }
    }

    /// A clock-day firing carries its day and nothing else (the schedule
    /// runner's `{"_day": ...}`), so the day is `_day` and the only
    /// instant it can be anchored to is the midnight that opens it — the
    /// instant a day rule falls due (`next_due`), never the moment the
    /// dispatcher got round to it.
    #[test]
    fn a_day_firing_is_about_its_day_and_anchored_at_its_midnight() {
        let ctx = day_firing(json!({"_day": "2026-09-21"}));
        assert_eq!(ctx.firing_day().unwrap().to_string(), "2026-09-21");
        assert_eq!(
            ctx.firing_instant_or_day_start().unwrap().to_rfc3339(),
            "2026-09-21T00:00:00+00:00"
        );
        assert!(
            ctx.firing_instant().is_err(),
            "the instant itself is still refused: a day firing names no time"
        );
    }

    /// A firing that carries an instant keeps it: the tick's `_at`, or a
    /// fact's own timestamp. The day is the day of that firing.
    #[test]
    fn a_firing_with_an_instant_is_anchored_at_that_instant() {
        let tick = day_firing(json!({"_day": "2026-09-17", "_at": "2026-09-17T10:05:00+00:00"}));
        assert_eq!(tick.firing_day().unwrap().to_string(), "2026-09-17");
        assert_eq!(
            tick.firing_instant_or_day_start().unwrap().to_rfc3339(),
            "2026-09-17T10:05:00+00:00"
        );
        let fact = firing(json!({"outbox_id": 4242}), Some("2026-09-28T04:31:07Z"));
        assert_eq!(fact.firing_day().unwrap().to_string(), "2026-09-28");
        assert_eq!(
            fact.firing_instant_or_day_start().unwrap().to_rfc3339(),
            "2026-09-28T04:31:07+00:00"
        );
    }

    /// No day and no instant — or a `_day` that does not read as a date —
    /// is refused permanently, naming every field it looked for, rather
    /// than read off the consumer's clock.
    #[test]
    fn no_day_and_no_instant_is_refused_by_name() {
        for payload in [json!({}), json!({"_day": "monday"})] {
            let ctx = day_firing(payload);
            for err in [
                ctx.firing_day().map(|_| ()).expect_err("no day"),
                ctx.firing_instant_or_day_start()
                    .map(|_| ())
                    .expect_err("no anchor"),
            ] {
                assert!(err.is_permanent(), "{err}");
                let said = err.to_string();
                for named in [
                    "`_day`",
                    "`_at`",
                    "`timestamp`",
                    "clock.day",
                    "clock-day:2026-09-21",
                ] {
                    assert!(said.contains(named), "names {named:?}: {said}");
                }
            }
        }
    }

    // ----- arg helpers -----

    #[tokio::test]
    async fn arg_string_lookup_works() {
        let args = vec![
            (
                "kind".to_string(),
                Value::String("ingredient-restock".into()),
            ),
            ("subject".to_string(), Value::String("vnd-001".into())),
        ];
        assert_eq!(arg_string(&args, "kind").unwrap(), "ingredient-restock");
        assert_eq!(arg_string(&args, "subject").unwrap(), "vnd-001");
        assert!(matches!(
            arg_string(&args, "missing"),
            Err(HandlerError::MissingArg(_))
        ));
    }

    #[tokio::test]
    async fn arg_string_lookup_type_check_errors() {
        let args = vec![("count".to_string(), Value::Int(42))];
        assert!(matches!(
            arg_string(&args, "count"),
            Err(HandlerError::BadArgType { .. })
        ));
    }

    // ----- end-to-end through the canonical reorder rule -----

    struct ReorderHelpers;

    impl HelperResolver for ReorderHelpers {
        fn call(&self, name: &str, args: &[Value]) -> Result<Value, super::super::expr::EvalError> {
            match name {
                "open_po_exists" => Ok(Value::Bool(false)),
                "vendor_for" => match args.first() {
                    Some(Value::String(sku)) => Ok(Value::String(format!("vnd-for-{sku}"))),
                    _ => Err(super::super::expr::EvalError::TypeError {
                        expected: "string",
                        got: "other",
                    }),
                },
                _ => Err(super::super::expr::EvalError::UnknownHelper(
                    name.to_string(),
                )),
            }
        }
    }

    #[tokio::test]
    async fn canonical_reorder_rule_fires_jobs_spawn_with_resolved_subject() {
        let toml = r#"
[[rule]]
name = "spawn-restock-on-low-inventory"
on_event = "inventory.parts.consumed"
when = "on_hand <= reorder_point AND NOT open_po_exists(part_sku)"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"ingredient-restock\"", subject = "vendor_for(part_sku)" }
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let payload = json!({
            "part_sku": "PKG-CO2-50LB",
            "on_hand": 10,
            "reorder_point": 20,
        });
        let matched = run_with_helpers(&reg, "inventory.parts.consumed", &payload, &ReorderHelpers);
        assert_eq!(matched.len(), 1);

        let spawn = RecordingHandler::new("jobs.spawn");
        let mut hreg = HandlerRegistry::new();
        hreg.register(spawn.clone());

        let results = dispatch(
            &matched,
            &hreg,
            "evt-consume-1",
            "inventory.parts.consumed",
            &serde_json::json!({}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].outcome.is_ok());

        let calls = spawn.calls().await;
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        // Rule as actor (D2): the recorded actor identity IS the rule
        // name; the triggering event id chains causation back to the
        // upstream consume event.
        assert_eq!(c.rule_name, "spawn-restock-on-low-inventory");
        assert_eq!(c.triggering_event_id, "evt-consume-1");
        assert_eq!(c.triggering_topic, "inventory.parts.consumed");
        assert_eq!(
            c.args,
            vec![
                (
                    "kind".to_string(),
                    Value::String("ingredient-restock".into())
                ),
                (
                    "subject".to_string(),
                    Value::String("vnd-for-PKG-CO2-50LB".into())
                ),
            ]
        );
    }
}
