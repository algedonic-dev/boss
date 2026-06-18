//! Domain types for the Cybernetics agent stack.
//!
//! These types describe what flows through the system — messages to agents,
//! claims on those messages, costs, runs, budgets, and registry entries.
//! They have no behavior beyond construction and serialization; behavior lives
//! in adapters that implement the ports in [`crate::port`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::define_id;

define_id!(MessageId);
define_id!(RunId);
define_id!(ClaimId);

/// Stable, slug-based identifier for an agent on a VM.
///
/// Slugs are lowercase kebab-case: `[a-z][a-z0-9-]*`, 1..=64 chars, must not
/// start or end with a hyphen and must not contain consecutive hyphens.
/// Validation is enforced at construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AgentIdError {
    #[error("agent id is empty")]
    Empty,
    #[error("agent id is longer than 64 characters")]
    TooLong,
    #[error("agent id must start with a lowercase letter")]
    BadStart,
    #[error("agent id must end with a lowercase letter or digit")]
    BadEnd,
    #[error("agent id contains invalid character '{0}'")]
    BadChar(char),
    #[error("agent id contains consecutive hyphens")]
    DoubleHyphen,
}

impl AgentId {
    pub fn try_new(s: impl Into<String>) -> Result<Self, AgentIdError> {
        let s: String = s.into();
        if s.is_empty() {
            return Err(AgentIdError::Empty);
        }
        if s.len() > 64 {
            return Err(AgentIdError::TooLong);
        }
        let bytes = s.as_bytes();
        let first = bytes[0] as char;
        if !first.is_ascii_lowercase() {
            return Err(AgentIdError::BadStart);
        }
        let last = bytes[bytes.len() - 1] as char;
        if !(last.is_ascii_lowercase() || last.is_ascii_digit()) {
            return Err(AgentIdError::BadEnd);
        }
        let mut prev_hyphen = false;
        for c in s.chars() {
            let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
            if !ok {
                return Err(AgentIdError::BadChar(c));
            }
            if c == '-' {
                if prev_hyphen {
                    return Err(AgentIdError::DoubleHyphen);
                }
                prev_hyphen = true;
            } else {
                prev_hyphen = false;
            }
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for AgentId {
    type Error = AgentIdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::try_new(s)
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

/// A message destined for an agent. Immutable once constructed.
///
/// Messages enter Cybernetics via the event bus (NATS) and are persisted
/// to the per-agent inbox (`MessageQueue`) before dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: MessageId,
    pub timestamp: DateTime<Utc>,
    pub target: AgentId,
    /// Dot-separated kind (e.g. `"work.plan-feature"`).
    pub kind: String,
    pub payload: serde_json::Value,
    /// Optional NATS subject for replies.
    pub reply_to: Option<String>,
    /// Correlation id for tracing a chain of messages.
    pub correlation_id: Option<Uuid>,
}

impl Message {
    pub fn new(target: AgentId, kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            id: MessageId::new(),
            timestamp: Utc::now(),
            target,
            kind: kind.into(),
            payload,
            reply_to: None,
            correlation_id: None,
        }
    }

    pub fn with_reply_to(mut self, subject: impl Into<String>) -> Self {
        self.reply_to = Some(subject.into());
        self
    }

    pub fn with_correlation(mut self, id: Uuid) -> Self {
        self.correlation_id = Some(id);
        self
    }
}

/// A message pulled from a queue and assigned to a dispatcher.
///
/// Holds a `claim_id` that the dispatcher must present to `ack`/`nack` the
/// message after the run completes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimedMessage {
    pub claim_id: ClaimId,
    pub message: Message,
    pub claimed_at: DateTime<Utc>,
    pub attempt: u32,
}

/// Cost incurred by an agent run. All monetary values in micro-USD
/// (1_000_000 = $1.00) to avoid floating point.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub usd_micros: u64,
}

impl Cost {
    pub const ZERO: Cost = Cost {
        input_tokens: 0,
        output_tokens: 0,
        usd_micros: 0,
    };

    pub fn saturating_sum(self, other: Cost) -> Cost {
        Cost {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            usd_micros: self.usd_micros.saturating_add(other.usd_micros),
        }
    }
}

impl std::ops::Add for Cost {
    type Output = Cost;
    fn add(self, rhs: Cost) -> Cost {
        self.saturating_sum(rhs)
    }
}

/// Outcome reported by an agent after a dispatch completes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Success {
        cost: Cost,
        response: serde_json::Value,
    },
    Failed {
        cost: Cost,
        error: String,
    },
    Cancelled,
}

impl Outcome {
    pub fn cost(&self) -> Cost {
        match self {
            Outcome::Success { cost, .. } | Outcome::Failed { cost, .. } => *cost,
            Outcome::Cancelled => Cost::ZERO,
        }
    }
}

/// Decision returned from a budget check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BudgetDecision {
    Allow { remaining_usd_micros: u64 },
    Deny { reason: String },
}

impl BudgetDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, BudgetDecision::Allow { .. })
    }
}

/// Static per-agent configuration held by the registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSpec {
    pub id: AgentId,
    pub display_name: String,
    pub system_prompt: String,
    pub model: String,
    /// Hard hourly cap; runs are denied if recording would exceed this.
    pub hourly_budget_usd_micros: u64,
    /// Max in-flight runs for this agent on this VM.
    pub max_concurrent_runs: u32,
}

/// Time window for cost queries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Window {
    LastHour,
    LastDay,
    Since { at: DateTime<Utc> },
}

/// Lifecycle status of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Starting,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Handle to an in-flight or finished agent run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunHandle {
    pub run_id: RunId,
    pub agent: AgentId,
    pub message_id: MessageId,
    pub claim_id: ClaimId,
    pub started_at: DateTime<Utc>,
    pub status: RunStatus,
}

/// Notification that a dispatched run reached a terminal state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunCompletion {
    pub run: RunHandle,
    pub outcome: Outcome,
    pub finished_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_accepts_slug() {
        let id = AgentId::try_new("planner").unwrap();
        assert_eq!(id.as_str(), "planner");
        assert_eq!(id.to_string(), "planner");
    }

    #[test]
    fn agent_id_accepts_hyphenated_slug_with_digits() {
        AgentId::try_new("code-reviewer-2").unwrap();
    }

    #[test]
    fn agent_id_rejects_empty() {
        assert_eq!(AgentId::try_new(""), Err(AgentIdError::Empty));
    }

    #[test]
    fn agent_id_rejects_uppercase() {
        assert!(matches!(
            AgentId::try_new("Planner"),
            Err(AgentIdError::BadStart)
        ));
    }

    #[test]
    fn agent_id_rejects_starting_digit() {
        assert!(matches!(
            AgentId::try_new("1planner"),
            Err(AgentIdError::BadStart)
        ));
    }

    #[test]
    fn agent_id_rejects_trailing_hyphen() {
        assert!(matches!(
            AgentId::try_new("planner-"),
            Err(AgentIdError::BadEnd)
        ));
    }

    #[test]
    fn agent_id_rejects_double_hyphen() {
        assert_eq!(
            AgentId::try_new("plan--ner"),
            Err(AgentIdError::DoubleHyphen)
        );
    }

    #[test]
    fn agent_id_rejects_invalid_char() {
        assert_eq!(
            AgentId::try_new("plan_ner"),
            Err(AgentIdError::BadChar('_'))
        );
    }

    #[test]
    fn agent_id_rejects_too_long() {
        let s: String = "a".repeat(65);
        assert_eq!(AgentId::try_new(s), Err(AgentIdError::TooLong));
    }

    #[test]
    fn agent_id_serde_round_trips_as_string() {
        let id = AgentId::try_new("planner").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"planner\"");
        let back: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn agent_id_serde_rejects_invalid_string() {
        let bad = "\"Planner\"";
        assert!(serde_json::from_str::<AgentId>(bad).is_err());
    }

    #[test]
    fn message_builder_defaults() {
        let agent = AgentId::try_new("planner").unwrap();
        let msg = Message::new(agent.clone(), "work.plan", serde_json::json!({"x": 1}));
        assert_eq!(msg.target, agent);
        assert_eq!(msg.kind, "work.plan");
        assert!(msg.reply_to.is_none());
        assert!(msg.correlation_id.is_none());
    }

    #[test]
    fn message_with_reply_to_and_correlation() {
        let agent = AgentId::try_new("planner").unwrap();
        let corr = Uuid::new_v4();
        let msg = Message::new(agent, "k", serde_json::json!({}))
            .with_reply_to("boss.s1.vm1.planner.out.done")
            .with_correlation(corr);
        assert_eq!(
            msg.reply_to.as_deref(),
            Some("boss.s1.vm1.planner.out.done")
        );
        assert_eq!(msg.correlation_id, Some(corr));
    }

    #[test]
    fn message_round_trips_serde() {
        let agent = AgentId::try_new("planner").unwrap();
        let msg = Message::new(agent, "work.plan", serde_json::json!({"x": 1}));
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn cost_add_saturates_and_sums_fields() {
        let a = Cost {
            input_tokens: 10,
            output_tokens: 20,
            usd_micros: 500,
        };
        let b = Cost {
            input_tokens: 5,
            output_tokens: 7,
            usd_micros: 100,
        };
        let c = a + b;
        assert_eq!(c.input_tokens, 15);
        assert_eq!(c.output_tokens, 27);
        assert_eq!(c.usd_micros, 600);

        let max = Cost {
            input_tokens: u64::MAX,
            output_tokens: 0,
            usd_micros: 0,
        };
        assert_eq!((max + a).input_tokens, u64::MAX);
    }

    #[test]
    fn outcome_cost_returns_zero_for_cancelled() {
        assert_eq!(Outcome::Cancelled.cost(), Cost::ZERO);
        let c = Cost {
            input_tokens: 1,
            output_tokens: 2,
            usd_micros: 3,
        };
        assert_eq!(
            Outcome::Success {
                cost: c,
                response: serde_json::json!({})
            }
            .cost(),
            c
        );
        assert_eq!(
            Outcome::Failed {
                cost: c,
                error: "boom".into()
            }
            .cost(),
            c
        );
    }

    #[test]
    fn budget_decision_is_allowed() {
        assert!(
            BudgetDecision::Allow {
                remaining_usd_micros: 100
            }
            .is_allowed()
        );
        assert!(
            !BudgetDecision::Deny {
                reason: "cap".into()
            }
            .is_allowed()
        );
    }

    #[test]
    fn budget_decision_round_trips_serde() {
        let d = BudgetDecision::Allow {
            remaining_usd_micros: 42,
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: BudgetDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }
}
