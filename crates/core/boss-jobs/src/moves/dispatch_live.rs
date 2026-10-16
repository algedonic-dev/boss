//! Immutable dispatch context: what the registered packet and step
//! actually said when the mover observed the handoff (a4de1e72).

use boss_core::job::{Job, JobStatus, Step, StepStatus};
use serde::{Deserialize, Serialize};

/// Missing context is historical/unread, not a registration assertion.
/// A refused context conserves the concrete disagreement that was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DispatchContext {
    Registered {
        run: String,
        parent: String,
        step_id: String,
        source_kind: String,
        source_version: i32,
        source_step: String,
    },
    Refused {
        why: String,
    },
}

impl DispatchContext {
    pub fn belongs_to(&self, run: &str, parent: Option<&str>) -> bool {
        match self {
            Self::Registered {
                run: recorded,
                parent: original,
                ..
            } => recorded == run && parent == Some(original.as_str()),
            Self::Refused { .. } => true,
        }
    }
}

/// Judge complete original rows, using the existing agent-resolution
/// contract. This records a claim already made; it grants no authority.
pub fn dispatch_context(
    run: &Job,
    run_steps: &[Step],
    parent: &Job,
    parent_steps: &[Step],
    active: Option<&crate::registry::WorkflowSpec>,
) -> Option<DispatchContext> {
    let refused = |why: &str| Some(DispatchContext::Refused { why: why.into() });
    if run.kind != crate::agent_budget::RUN_KIND || run.status != JobStatus::Open {
        return refused("the successor is not an open agent run");
    }
    if run
        .metadata
        .get("packet")
        .and_then(serde_json::Value::as_str)
        != Some(parent.id.to_string().as_str())
    {
        return refused("the run does not name this predecessor");
    }
    let Some(slug) = run.metadata.get("step").and_then(serde_json::Value::as_str) else {
        return refused("the run names no source step");
    };
    let Some(step) = parent_steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some(slug))
    else {
        return refused("the predecessor has no such step");
    };
    if step
        .metadata
        .get("agent_run")
        .and_then(serde_json::Value::as_str)
        != Some(run.id.to_string().as_str())
    {
        return refused("the original step does not name this registered run");
    }
    if !matches!(step.status, StepStatus::Active | StepStatus::Completed) {
        return refused("the original step was not claimed");
    }
    if active.is_some_and(|a| a.kind != parent.kind) {
        return refused("the routing registry row is for another kind");
    }
    let resolved = crate::agent_spec::resolved(step, active);
    let Some(agent) = crate::agent_spec::projected(&resolved.metadata) else {
        active?;
        return refused("the source step declares no resolvable agent contract");
    };
    if !crate::agent_spec::agent_takes(&resolved.metadata)
        || crate::agent_spec::refusal(&agent).is_some()
    {
        return refused("the source agent contract is human-only or refused");
    }
    if run
        .metadata
        .get("profile")
        .and_then(serde_json::Value::as_str)
        != Some(agent.profile.as_str())
    {
        return refused("the run profile does not match the original step contract");
    }
    if !run_steps.iter().any(|s| {
        s.spec_slug.as_deref() == Some("claimed")
            && s.status == StepStatus::Completed
            && s.metadata
                .get("trigger_name")
                .and_then(serde_json::Value::as_str)
                == Some("boss-dispatch")
    }) {
        return refused("the run has no completed supported dispatch trigger");
    }
    if run_steps
        .iter()
        .any(|s| s.spec_slug.as_deref() == Some("refused") && s.status == StepStatus::Completed)
    {
        return refused("the run has a completed refusal");
    }
    Some(DispatchContext::Registered {
        run: run.id.to_string(),
        parent: parent.id.to_string(),
        step_id: step.id.to_string(),
        source_kind: parent.kind.clone(),
        source_version: parent.workflow_version,
        source_step: slug.to_string(),
    })
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
