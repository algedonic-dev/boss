//! Credential-backed execution, declared by the pinned protocol (f623e425).
//! An actor-shaped header is not evidence of the executor's identity.
use crate::field_writer::CredentialedCaller;
use serde_json::{Value, json};

/// Frozen projection of StepSpec.executor, distinct from audience claim-for.
pub const KEY: &str = "credential_executor";

/// Absence preserves ordinary workflows. A malformed declaration refuses,
/// rather than silently turning a guarded step into an ordinary one.
pub fn judge<'a>(
    metadata: &Value,
    caller: Option<&'a CredentialedCaller>,
    host: Option<&str>,
) -> Result<Option<&'a CredentialedCaller>, String> {
    let Some(raw) = metadata.get(KEY) else {
        return Ok(None);
    };
    let principal = raw
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "the executor declaration is not a non-empty principal".to_string())?;
    crate::field_writer::admits(caller, principal, host)?;
    Ok(caller)
}

pub fn refusal(metadata: &Value, asked_by: &str, why: &str) -> Value {
    json!({"error":"only this step's credentialed executor may change its execution", "executor":metadata.get(KEY),"asked_by":asked_by,"refused_keys":["status","assignee_id"],"reason":why,"rule":"the protocol declares the executor; a server-resolved credential proves it (design f623e425)"})
}
