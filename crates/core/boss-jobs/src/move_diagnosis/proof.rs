//! Evidence copied from a credentialed executor, never a caller-supplied receipt.
use super::Diagnosis;
use boss_core::job::{Job, JobStatus, Step, StepStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const FRAME: &str = "boss-prove-receipt: ";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema: u32,
    pub artifact: uuid::Uuid,
    pub proof_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineProof {
    pub request: Job,
    pub execute: Step,
    pub protocol: crate::registry::WorkflowSpec,
    pub artifact: Job,
    pub proven: Step,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub schema: u32,
    pub diagnosis_id: uuid::Uuid,
    pub original_seq: i64,
    pub original_hash: String,
    pub packet: String,
    pub cause_event_id: uuid::Uuid,
    pub cause_seq: i64,
    pub model: String,
    pub artifact_head: String,
    pub result: String,
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("machine proof lacks {key}"))
}
fn parse(value: &Value) -> Result<Value, String> {
    match value {
        Value::String(s) => {
            serde_json::from_str(s).map_err(|_| "machine proof is not complete JSON".into())
        }
        Value::Object(_) => Ok(value.clone()),
        _ => Err("machine proof unavailable".into()),
    }
}
fn sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn validate(e: &MachineProof, diagnosis: &Diagnosis) -> Result<(), String> {
    let refuse = || {
        "machine proof does not bind a protected completed execution and exact original".to_string()
    };
    let request = &e.request;
    let execute = &e.execute;
    let artifact = &e.artifact;
    let proven = &e.proven;
    if request.kind != "ops-request"
        || request.status != JobStatus::Closed
        || artifact.kind != crate::regions::CAR_KIND
        || artifact.metadata.get("merged").and_then(Value::as_bool) != Some(true)
        || request.partition != artifact.partition
        || execute.job_id != request.id
        || execute.spec_slug.as_deref() != Some("execute")
        || execute.status != StepStatus::Completed
        || proven.job_id != artifact.id
        || proven.spec_slug.as_deref() != Some("proven")
        || proven.status != StepStatus::Completed
        || e.protocol.kind != request.kind
        || e.protocol.version != request.workflow_version
    {
        return Err(refuse());
    }
    let specs: Vec<_> = e
        .protocol
        .steps
        .iter()
        .filter(|s| s.title == "execute")
        .collect();
    let [spec] = specs.as_slice() else {
        return Err(refuse());
    };
    if spec.executor.as_deref() != Some(crate::runner_credential::PRINCIPAL)
        || execute
            .metadata
            .get(crate::credential_executor::KEY)
            .and_then(Value::as_str)
            != Some(crate::runner_credential::PRINCIPAL)
    {
        return Err(refuse());
    }
    for key in ["output", "exit_code", "disposition"] {
        for fields in [&spec.fields, &execute.fields] {
            let matches: Vec<_> = fields.iter().filter(|f| f.name == key).collect();
            if matches.len() != 1
                || matches[0].writer.as_deref() != Some(crate::runner_credential::PRINCIPAL)
            {
                return Err("machine execution evidence has no protected writer".into());
            }
        }
    }
    if text(&request.metadata, "verb")? != "run-car-probe"
        || request.metadata.get("args") != Some(&serde_json::json!([artifact.id.to_string()]))
        || text(&request.metadata, "host")? != "forge"
        || text(&execute.metadata, "disposition")? != "answered"
        || text(&execute.metadata, "exit_code")? != "0"
        || text(&execute.metadata, "runner_host")? != "forge"
        || execute
            .completed_by
            .as_ref()
            .map(ToString::to_string)
            .as_deref()
            != Some(crate::runner_credential::ACTOR)
        || text(&proven.metadata, "proven_by")? != "run-car-probe"
    {
        return Err(refuse());
    }
    let output = text(&execute.metadata, "output")?;
    if output.len() > 102400 || output.contains("[ops-runner: output truncated") {
        return Err(refuse());
    }
    let frames: Vec<_> = output
        .lines()
        .filter_map(|l| l.strip_prefix(FRAME))
        .collect();
    let [frame] = frames.as_slice() else {
        return Err("machine proof frame missing or ambiguous".into());
    };
    let expected_frame = format!("{FRAME}{frame}");
    if output.lines().last() != Some(expected_frame.as_str()) {
        return Err(refuse());
    }
    let envelope: Envelope = serde_json::from_str(frame).map_err(|_| refuse())?;
    if envelope.schema != 1 || boss_core::job::JobId::from_uuid(envelope.artifact) != artifact.id {
        return Err("executor frame names another artifact".into());
    }
    let proof: Value = serde_json::from_str(&envelope.proof_json).map_err(|_| refuse())?;
    let recorded = parse(proven.metadata.get("proof").ok_or_else(refuse)?)?;
    if proof != recorded
        || proof.get("exit").and_then(Value::as_i64) != Some(0)
        || proof.get("overridden").is_some()
        || proof.get("context_unreadable").is_some()
        || proof.get("missing_tools").is_some()
    {
        return Err(refuse());
    }
    for key in [
        "probe",
        "expect",
        "host",
        "cwd",
        "user",
        "uid",
        "at",
        "tree_head",
    ] {
        text(&proof, key)?;
    }
    let head = text(&proof, "tree_head")?;
    let merge = text(&artifact.metadata, "merge_ref")?;
    if !sha(head)
        || merge.len() < 12
        || merge.len() > 40
        || !head.starts_with(merge)
        || !matches!(text(&proof, "tree_standing")?, "current" | "unreadable")
    {
        return Err(refuse());
    }
    let at = chrono::DateTime::parse_from_rfc3339(text(&proof, "at")?).map_err(|_| refuse())?;
    if request.opened_at.is_none_or(|start| at < start)
        || execute.completed_at.is_none_or(|end| at > end)
        || proven.completed_at.is_none_or(|end| at > end)
    {
        return Err(refuse());
    }
    let stdout = text(&proof, "stdout")?;
    if stdout.contains(FRAME)
        || stdout.contains("[truncated")
        || text(&proof, "stderr").is_ok_and(|s| s.contains(FRAME))
    {
        return Err("probe-authored proof framing is refused".into());
    }
    let seen: Observation = serde_json::from_str(stdout)
        .map_err(|_| "proof output is not complete structured original bindings".to_string())?;
    let m = &diagnosis.original.r#move;
    if seen.schema != 1
        || seen.diagnosis_id != diagnosis.id
        || seen.original_seq != diagnosis.original.seq
        || seen.original_hash != diagnosis.original_hash
        || seen.packet != m.packet
        || seen.cause_event_id != m.cause_event_id
        || seen.cause_seq != m.cause_seq
        || seen.model != diagnosis.model
        || seen.artifact_head != head
        || seen.result != "false-interpretation"
    {
        return Err(refuse());
    }
    Ok(())
}
