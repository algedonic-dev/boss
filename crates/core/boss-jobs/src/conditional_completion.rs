//! Conditional completion is a request, not completion provenance.
//! Its receipt comes from the committed state event, never a retry's candidate.

use boss_core::{
    actor::ActorId,
    event::Event,
    job::{JobId, Step, StepId, StepStatus},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

pub const RECEIPT_KEY: &str = "_conditional_completion";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedStep {
    pub status: StepStatus,
    // A missing holder is not an expectation of nobody.
    #[serde(deserialize_with = "present_holder")]
    pub assignee_id: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

fn present_holder<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionRequest {
    pub operation_id: Uuid,
    pub expected: ExpectedStep,
    pub evidence: Map<String, Value>,
}

impl CompletionRequest {
    pub fn matches(&self, step: &Step, version: &str) -> bool {
        self.expected.status == step.status
            && self.expected.assignee_id == step.assignee_id
            && self
                .expected
                .version
                .as_deref()
                .is_none_or(|expected| expected == version)
    }

    pub fn valid(&self) -> bool {
        !self.operation_id.is_nil()
            && !matches!(
                self.expected.status,
                StepStatus::Completed | StepStatus::Skipped
            )
            && self
                .expected
                .assignee_id
                .as_deref()
                .is_none_or(|holder| !holder.trim().is_empty())
            && self
                .expected
                .version
                .as_deref()
                .is_none_or(|version| !version.trim().is_empty())
    }

    pub fn event_id(&self, job: &JobId, step: &StepId, caller: &str) -> Uuid {
        // Length framing keeps arbitrary authenticated identifiers distinct.
        let identity = format!(
            "conditional-completion/{job}/{step}/{}/{}/{}",
            self.operation_id,
            caller.len(),
            caller
        );
        Uuid::new_v5(&Uuid::NAMESPACE_OID, identity.as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionReceipt {
    pub operation_id: Uuid,
    pub job_id: JobId,
    pub step_id: StepId,
    pub event_id: Uuid,
    pub actor: ActorId,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CompletionOutcome {
    Completed {
        receipt: CompletionReceipt,
    },
    Replayed {
        receipt: CompletionReceipt,
    },
    PreconditionFailed {
        step_id: StepId,
    },
    TerminalConflict {
        step_id: StepId,
    },
    NotFound {
        step_id: StepId,
    },
    UnsupportedCapability {
        step_id: StepId,
        kind: String,
        capability: Option<crate::step_registry::ConditionalCompletion>,
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedCompletion {
    pub request: CompletionRequest,
    pub caller: String,
    pub receipt: CompletionReceipt,
}

impl RecordedCompletion {
    /// The request is authenticated by the immutable committed event,
    /// never a candidate or a JSON marker posted into row metadata.
    pub fn authenticated_by(&self, event: &Event, step: &Step) -> bool {
        event.id == self.receipt.event_id
            && event.kind == crate::events::STEP_UPDATED
            && event.timestamp == self.receipt.recorded_at
            && self.receipt.step_id == step.id
            && self.receipt.job_id == step.job_id
            && step.status == StepStatus::Completed
            && step.completed_by.as_ref() == Some(&self.receipt.actor)
            && event.payload.get("_actor")
                == serde_json::to_value(&self.receipt.actor).ok().as_ref()
            && event.payload.get("id") == Some(&Value::String(step.id.to_string()))
            && event.payload.get("job_id") == Some(&Value::String(step.job_id.to_string()))
            && event.payload.get("status") == Some(&Value::String("completed".into()))
            && event.payload.get("metadata") == Some(&step.metadata)
            && event.payload.get(RECEIPT_KEY) == serde_json::to_value(self).ok().as_ref()
    }
}
