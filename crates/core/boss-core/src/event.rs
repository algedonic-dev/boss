use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An immutable fact about something that happened in the system.
///
/// Events are the fundamental communication unit in Boss.
/// They are never modified after creation — they are facts.
///
/// **`timestamp` is required at construction.** There is no
/// `Utc::now()` default: a silent default lets the audit log wander
/// off-clock (wallclock-stamped rows in a sim-dated log) with no
/// visible breakage. Forcing `timestamp` as an argument means every
/// caller sources it — from `state.clock.now().await.now` in handlers,
/// from a fixture in tests, from `Utc::now()` only at the boundary
/// where truly-current wall-clock is the intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique event identifier
    pub id: Uuid,
    /// When this event occurred. **Caller-supplied**; route it
    /// from your authoritative clock (the boss-clock-api response,
    /// via `state.clock.now().await.now`).
    pub timestamp: DateTime<Utc>,
    /// Which service/module produced this event
    pub source: String,
    /// Event kind — dot-separated namespace (e.g., "agent.health.check")
    pub kind: String,
    /// Arbitrary JSON payload
    pub payload: serde_json::Value,
}

impl Event {
    pub fn new(
        source: impl Into<String>,
        kind: impl Into<String>,
        payload: serde_json::Value,
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp,
            source: source.into(),
            kind: kind.into(),
            payload,
        }
    }
}

/// A request to perform an action. Unlike events (past tense facts),
/// commands are requests that may be accepted or rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub source: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

impl Command {
    pub fn new(
        source: impl Into<String>,
        kind: impl Into<String>,
        payload: serde_json::Value,
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp,
            source: source.into(),
            kind: kind.into(),
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_is_immutable_snapshot() {
        let event = Event::new(
            "test",
            "test.created",
            serde_json::json!({"key": "value"}),
            Utc::now(),
        );

        assert!(!event.id.is_nil());
        assert_eq!(event.source, "test");
        assert_eq!(event.kind, "test.created");

        // Events serialize cleanly
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, event.id);
    }

    #[test]
    fn command_is_distinct_from_event() {
        let cmd = Command::new("cli", "agent.restart", serde_json::json!({}), Utc::now());
        assert_eq!(cmd.source, "cli");
        assert_eq!(cmd.kind, "agent.restart");
    }
}
