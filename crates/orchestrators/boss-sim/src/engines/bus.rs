//! In-process pub/sub the sim engines use to coordinate within a
//! single day. Day-bounded delivery: events emitted on day N are
//! visible to other engines for the rest of day N's pipeline, then
//! cleared at end-of-day. Day N+1 starts with an empty bus.
//!
//! Topics are freeform strings of the shape `<scope>.<action>` (≥2
//! segments). Listeners match by exact string — no wildcards, no
//! topic taxonomy. Unknown topics are not rejected; an engine that
//! doesn't subscribe simply ignores them.

use serde::{Deserialize, Serialize};

/// One event flowing through the bus. Payload is opaque JSON so the
/// schema is per-topic, owned by the publisher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimBusEvent {
    /// `<scope>.<action>` — e.g. `"job.opened"`, `"ledger.payment_settled"`,
    /// `"brewery.morning_brew.delivered"`.
    pub topic: String,
    /// Stable identifier of the engine that published the event. Used
    /// for tracing + cycle-detection diagnostics. Format is
    /// `<engine-name>` or `<engine-name>:<spec-name>` for engines that
    /// host multiple specs (e.g. `counterparty:bank-ach`).
    pub source: String,
    /// Per-topic JSON payload. The publisher and subscribers agree on
    /// the schema; the bus doesn't validate it.
    pub payload: serde_json::Value,
}

impl SimBusEvent {
    pub fn new(
        topic: impl Into<String>,
        source: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            topic: topic.into(),
            source: source.into(),
            payload,
        }
    }
}

/// Day-bounded bus. Engines publish via `emit`; subscribers iterate
/// `events_matching` after each upstream engine has run. The day-loop
/// calls `clear_day` at end-of-day to drop the day's traffic and
/// start the next day clean.
#[derive(Debug, Default)]
pub struct SimEventBus {
    day_events: Vec<SimBusEvent>,
}

impl SimEventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish an event onto the bus. Returns the index it was
    /// inserted at — useful for tests asserting on insertion order.
    pub fn emit(&mut self, event: SimBusEvent) -> usize {
        let idx = self.day_events.len();
        self.day_events.push(event);
        idx
    }

    /// Convenience overload that builds the event from parts.
    pub fn publish(
        &mut self,
        topic: impl Into<String>,
        source: impl Into<String>,
        payload: serde_json::Value,
    ) -> usize {
        self.emit(SimBusEvent::new(topic, source, payload))
    }

    /// All events emitted today, in insertion order.
    pub fn events(&self) -> &[SimBusEvent] {
        &self.day_events
    }

    /// Iterator over today's events whose topic exactly matches
    /// `topic`. Hot path for engines listening on a small set of
    /// topics; no allocation.
    pub fn events_matching<'a>(&'a self, topic: &'a str) -> impl Iterator<Item = &'a SimBusEvent> {
        self.day_events.iter().filter(move |e| e.topic == topic)
    }

    /// Drop today's events. Called by the day-loop at end-of-day so
    /// the next day starts with an empty bus.
    pub fn clear_day(&mut self) {
        self.day_events.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.day_events.is_empty()
    }

    pub fn len(&self) -> usize {
        self.day_events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn emit_increments_len() {
        let mut bus = SimEventBus::new();
        assert_eq!(bus.len(), 0);
        bus.publish("job.opened", "human-worker", json!({"job_id": "job-1"}));
        bus.publish("step.done", "human-worker", json!({"step_id": "stp-1"}));
        assert_eq!(bus.len(), 2);
    }

    #[test]
    fn events_matching_filters_by_topic() {
        let mut bus = SimEventBus::new();
        bus.publish("job.opened", "h", json!({"job_id": "job-1"}));
        bus.publish("step.done", "h", json!({"step_id": "stp-1"}));
        bus.publish("job.opened", "h", json!({"job_id": "job-2"}));
        let opened: Vec<_> = bus.events_matching("job.opened").collect();
        assert_eq!(opened.len(), 2);
        assert_eq!(opened[0].payload["job_id"], "job-1");
        assert_eq!(opened[1].payload["job_id"], "job-2");
    }

    #[test]
    fn events_matching_returns_empty_for_unknown_topic() {
        let mut bus = SimEventBus::new();
        bus.publish("job.opened", "h", json!({}));
        assert_eq!(bus.events_matching("nope.never").count(), 0);
    }

    #[test]
    fn clear_day_empties_bus() {
        let mut bus = SimEventBus::new();
        bus.publish("job.opened", "h", json!({}));
        bus.publish("step.done", "h", json!({}));
        bus.clear_day();
        assert!(bus.is_empty());
        assert_eq!(bus.events().len(), 0);
    }

    #[test]
    fn insertion_order_preserved() {
        let mut bus = SimEventBus::new();
        for i in 0..5 {
            bus.publish("topic.x", "h", json!({"i": i}));
        }
        let seen: Vec<i64> = bus
            .events_matching("topic.x")
            .map(|e| e.payload["i"].as_i64().unwrap())
            .collect();
        assert_eq!(seen, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn event_serializes_round_trip() {
        let e = SimBusEvent::new("a.b", "src", json!({"k": "v"}));
        let s = serde_json::to_string(&e).unwrap();
        let back: SimBusEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.topic, "a.b");
        assert_eq!(back.source, "src");
        assert_eq!(back.payload["k"], "v");
    }
}
