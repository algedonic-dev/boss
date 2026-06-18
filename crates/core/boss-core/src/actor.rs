//! `ActorId` — the named origin of every transition.
//!
//! Boss is a human-powered state machine (see
//! `docs/design/human-powered-state-machine.md`). Invariant **I-2**
//! says every transition has a named CPU — so every event and every
//! write needs an actor, full stop. Before this type existed, event
//! authors reached for `actor_id: None` when no human was involved.
//!
//! Every transition is one of exactly two kinds:
//!
//!   - **Human** — an employee took the action (a human CPU).
//!   - **Automation** — a *named* authority fired it: a dispatch rule
//!     (`automation:rule:<name>`), a scheduler/agent, or the emitting
//!     service itself (`automation:<service>`).
//!
//! There is deliberately no anonymous "system" actor. A system has no
//! inherent autonomy to do anything; attributing a transition to a
//! bare `"system"` masks the real authority that granted it. So every
//! automated transition references its explicit grant — the same way
//! the dispatcher already attributes side-effects to the rule that
//! fired them. "No one did it" is not a representable state, and
//! neither is "the system did it".
//!
//! One concept, four deliberate spellings — do not flatten:
//!   - `ActorId` — this Rust type;
//!   - `actor` — the publisher-parameter name in `EventPublisher`;
//!   - `_actor` — the audit_log payload key (the `_actor` /
//!     `_simulated` / `_source` metadata family);
//!   - `actor_id` — the serialized field on SQL columns, HTTP
//!     bodies, and TS types (the dominant spelling at boundaries).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Who (or what) fired a transition. Every event carries one.
///
/// Wire format:
///   - `ActorId::Human("emp-032")` → `"emp-032"` (bare; the SPA
///     consumes `actor_id` as an employee id when present).
///   - `ActorId::Automation("shipping-agent")` → `"automation:shipping-agent"`
///
/// The Human case serializes as a bare string (no `human:` prefix)
/// because the SPA treats `actor_id` as an opaque employee-id lookup
/// in many places (e.g. `empNames.get(actor_id)`). Automation uses the
/// `automation:` prefix so its kind is unambiguous on the wire and a
/// stale frontend doesn't render `"automation:cron"` as
/// "Employee automation:cron".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActorId {
    /// An Boss employee. The string is the `employees.id` foreign key.
    Human(String),
    /// A named automated process — agents, schedulers, cron jobs,
    /// bus subscribers. The string is a stable slug for the program
    /// (e.g. `"shipping-agent"`, `"escalation-router"`,
    /// `"warranty-expiry-scheduler"`). These slugs are free-form for
    /// now; if we need a registry of known automations later, that's
    /// a separate design.
    Automation(String),
}

impl ActorId {
    /// Short-hand for a human actor from an employee id string.
    pub fn human(emp_id: impl Into<String>) -> Self {
        Self::Human(emp_id.into())
    }

    /// Short-hand for a named automation.
    pub fn automation(name: impl Into<String>) -> Self {
        Self::Automation(name.into())
    }

    /// True if a human was the CPU on this transition.
    pub fn is_human(&self) -> bool {
        matches!(self, Self::Human(_))
    }

    /// Returns the underlying id / slug for display.
    pub fn as_slug(&self) -> &str {
        match self {
            Self::Human(id) | Self::Automation(id) => id.as_str(),
        }
    }
}

impl fmt::Display for ActorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Mirrors the Serialize impl: Human is bare; Automation uses
        // the `automation:` prefix.
        match self {
            Self::Human(id) => f.write_str(id),
            Self::Automation(name) => write!(f, "automation:{name}"),
        }
    }
}

impl FromStr for ActorId {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if let Some(rest) = s.strip_prefix("automation:") {
            Self::Automation(rest.to_string())
        } else if s == "system" {
            // Map the bare `system` actor to a typed, named catch-all
            // rather than a fake human, so every transition is
            // attributed to a real CPU.
            Self::Automation("platform".to_string())
        } else {
            Self::Human(s.to_string())
        })
    }
}

impl Serialize for ActorId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Human(id) => s.serialize_str(id),
            Self::Automation(name) => s.serialize_str(&format!("automation:{name}")),
        }
    }
}

impl<'de> Deserialize<'de> for ActorId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Accept either a string or `null`. A `null` (an event with
        // no recorded actor) maps to the `platform` automation —
        // every transition is attributed, never anonymous.
        let opt: Option<String> = Option::deserialize(d)?;
        Ok(match opt {
            Some(s) => s
                .parse()
                .unwrap_or_else(|_| Self::Automation("platform".to_string())),
            None => Self::Automation("platform".to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_roundtrip_uses_bare_string() {
        let a = ActorId::Human("emp-032".into());
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, "\"emp-032\"");
        let back: ActorId = serde_json::from_str(&j).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn automation_roundtrip_uses_prefix() {
        let a = ActorId::Automation("warranty-expiry".into());
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, "\"automation:warranty-expiry\"");
        let back: ActorId = serde_json::from_str(&j).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn legacy_system_string_maps_to_platform_automation() {
        // The `System` actor was removed in v1.1.0. A stale `"system"`
        // on the wire is read as the named `platform` automation, never
        // a fake human.
        let back: ActorId = serde_json::from_str("\"system\"").unwrap();
        assert_eq!(back, ActorId::Automation("platform".into()));
    }

    #[test]
    fn null_deserializes_to_platform_automation() {
        let a: ActorId = serde_json::from_str("null").unwrap();
        assert_eq!(a, ActorId::Automation("platform".into()));
    }
}
