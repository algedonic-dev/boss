//! Shared reservation projection; legacy stamps remain readable, but a
//! versioned reservation is authoritative once explicitly migrated.
use crate::conditional_metadata::{Command, Expected};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub const KEY: &str = "gate_reservation";
pub const INTENT: &str = "gate_launch_intent";
pub const OBSERVATION: &str = "gate_launch_observation";
pub const ATTEMPT: &str = "gate_launch_attempt";
pub const SUBMISSION: &str = "gate_submission";
pub const FIRST_RECORD_FIELDS: [&str; 4] = [INTENT, OBSERVATION, "gate_orphan_claim", SUBMISSION];
pub const TTL_SECONDS: i64 = 300;

/// The retained native request is data, not a reconstructed send permit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchIntent {
    pub contract_version: u32,
    pub packet: String,
    pub namespace: String,
    pub name: String,
    pub head: String,
    pub mode: String,
    #[serde(skip)]
    pub transition_generation: Option<Uuid>,
    pub file_id: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// One immutable source object is authoritative for resident consumers.
/// Raw metadata is only the legacy protocol's input, never a fallback
/// from unreadable or unbound resident presence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub contract_version: u32,
    pub command: Uuid,
    pub submitted_by: String,
    pub supervisor: String,
    pub intent: LaunchIntent,
    pub source: serde_json::Map<String, Value>,
}

pub fn submission(metadata: &Value) -> Result<Option<Submission>, String> {
    let Some(value) = metadata.get(SUBMISSION) else {
        return Ok(None);
    };
    let source: Submission = serde_json::from_value(value.clone())
        .map_err(|error| format!("durable submission is unreadable: {error}"))?;
    if source.contract_version != 1
        || source.command.is_nil()
        || source.submitted_by.is_empty()
        || source.supervisor.is_empty()
        || source.intent.contract_version != 1
    {
        return Err("durable submission contract/identity is invalid".into());
    }
    crate::metadata_immutability::view(metadata, SUBMISSION, "/source", 1, &[])?;
    Ok(Some(source))
}

pub fn source_metadata(metadata: &Value) -> Result<&Value, String> {
    if submission(metadata)?.is_some() {
        crate::metadata_immutability::view(metadata, SUBMISSION, "/source", 1, SOURCE_LOCATORS)
    } else {
        Ok(metadata)
    }
}

/// The original server receipt, never a client clock or copied raw
/// timestamp, establishes an accepted request's identity and order.
/// This pure check is shared by actuators and read-only projections.
pub fn acceptance(
    current: &boss_core::job::Job,
    receipt: &crate::conditional_metadata::Receipt,
) -> Result<Submission, String> {
    source_metadata(&current.metadata)?;
    let submission = submission(&current.metadata)?
        .ok_or("packet has explicit legacy ownership, not a resident acceptance")?;
    let encoded = serde_json::to_value(&submission).map_err(|error| error.to_string())?;
    if current.kind != "gate-run"
        || receipt.job.kind != current.kind
        || receipt.job_id != current.id
        || receipt.job.id != current.id
        || submission.intent.packet != current.id.to_string()
        || current.workflow_version != receipt.job.workflow_version
        || receipt.contract_version != 2
        || receipt.command.contract_version != 2
        || receipt.command.idempotency_key != submission.command
        || receipt.actor != submission.submitted_by
        || receipt.event_id.is_nil()
        || receipt
            .receipt_event_id
            .is_none_or(|id| id.is_nil() || id == receipt.event_id)
        || !receipt
            .command
            .immutable_keys
            .iter()
            .any(|key| key == SUBMISSION)
        || receipt.command.patch.get(SUBMISSION) != Some(&encoded)
        || receipt.job.metadata.get(SUBMISSION) != Some(&encoded)
        || receipt
            .job
            .metadata
            .pointer("/_immutable_metadata/gate_submission")
            != Some(&encoded)
    {
        return Err(
            "durable submission lacks its exact original acceptance/current protocol/index binding"
                .into(),
        );
    }
    Ok(submission)
}

/// Even malformed presence prevents a client-expiry or negative native
/// inference. It cannot grant a claim or a native permission.
pub fn durable_presence(metadata: &Value) -> bool {
    metadata.get(SUBMISSION).is_some()
}
/// Resident ownership never expires with a client heartbeat. Unreadable
/// presence remains unresolved until the protected native record is proven.
pub fn waiting_for_native(metadata: &Value) -> bool {
    durable_presence(metadata) && !native_observed(metadata)
}
pub const SOURCE_LOCATORS: &[&str] = &["branch", "sha", "agent_run", "worktree"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    pub contract_version: u32,
    pub generation: Uuid,
    pub queued_at: String,
    pub heartbeat_at: String,
    pub consumed: bool,
}

pub fn instant(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

pub fn read(metadata: &Value) -> Result<Option<Reservation>, String> {
    let Some(value) = metadata.get(KEY) else {
        return Ok(None);
    };
    let row: Reservation = serde_json::from_value(value.clone())
        .map_err(|e| format!("unreadable gate reservation: {e}"))?;
    if row.contract_version != 1
        || row.generation.is_nil()
        || instant(&row.queued_at).is_none()
        || instant(&row.heartbeat_at).is_none()
    {
        return Err("unreadable gate reservation identity or timestamps".into());
    }
    Ok(Some(row))
}

fn queued_at(metadata: &Value) -> Option<&str> {
    if metadata.get(KEY).is_some() {
        if native_observed(metadata) {
            return None;
        }
        let row = read(metadata).ok()??;
        return (!row.consumed)
            .then(|| metadata.get(KEY)?.get("queued_at")?.as_str())
            .flatten();
    }
    metadata
        .get("queued_at")?
        .as_str()
        .filter(|s| !s.is_empty())
}

pub fn native_observed(metadata: &Value) -> bool {
    let Some(observed) = metadata.get(OBSERVATION) else {
        return false;
    };
    let Some(intent) = metadata.get(INTENT) else {
        return false;
    };
    let Ok(bindings) = crate::metadata_immutability::bindings(metadata) else {
        return false;
    };
    if bindings.get(INTENT) != Some(intent) || bindings.get(OBSERVATION) != Some(observed) {
        return false;
    }
    if durable_presence(metadata) {
        let Ok(Some(submission)) = submission(metadata) else {
            return false;
        };
        if source_metadata(metadata).is_err()
            || serde_json::to_value(&submission.intent).ok().as_ref() != Some(intent)
        {
            return false;
        }
    }
    observed
        .get("uid")
        .and_then(Value::as_str)
        .is_some_and(|uid| !uid.is_empty())
        && observed
            .get("request_sha256")
            .and_then(Value::as_str)
            .is_some_and(|sha| Some(sha) == intent.get("sha256").and_then(Value::as_str))
        && observed
            .get("created_at")
            .and_then(Value::as_str)
            .and_then(instant)
            .is_some()
}

fn heartbeat(metadata: &Value) -> Option<&str> {
    if metadata.get(KEY).is_some() {
        read(metadata).ok()??;
        return metadata.get(KEY)?.get("heartbeat_at")?.as_str();
    }
    metadata
        .get("queue_heartbeat_at")
        .and_then(Value::as_str)
        .or_else(|| queued_at(metadata))
}

/// Presence includes malformed intent: absence of evidence cannot
/// authorize an orphan's negative terminal or safe withdrawal.
pub fn unresolved_intent(metadata: &Value) -> bool {
    metadata.get(INTENT).is_some()
        || durable_presence(metadata)
        || metadata.get("gate_launch_attempt").is_some()
}

pub fn stopped_reason(metadata: &Value, now: Option<DateTime<Utc>>) -> Option<&'static str> {
    if durable_presence(metadata) {
        return source_metadata(metadata).is_err().then_some("Durable queue request is unreadable or its index locators disagree; reconcile the original receipt");
    }
    metadata.get(KEY)?;
    let row = match read(metadata) {
        Ok(Some(row)) => row,
        _ => return Some("Gate reservation is unreadable; reconciliation requires attention"),
    };
    if metadata
        .get("launched_at")
        .and_then(Value::as_str)
        .and_then(instant)
        .is_some()
    {
        return None;
    }
    if row.consumed {
        return Some(
            "Native launch identity was observed; its launch stamp still needs reconciliation",
        );
    }
    if now.is_some_and(|now| !held(metadata, now)) {
        return Some(if unresolved_intent(metadata) {
            "Launch outcome UNKNOWN; inspect the exact recorded native Job before resuming"
        } else {
            "Queue holder expired; no bay or queue place is held"
        });
    }
    None
}

fn held(metadata: &Value, now: DateTime<Utc>) -> bool {
    queued_at(metadata).is_some()
        && heartbeat(metadata)
            .and_then(instant)
            .is_some_and(|beat| (now - beat).num_seconds() <= TTL_SECONDS)
}

fn expected(metadata: &Value, key: &str) -> Expected {
    Expected {
        present: metadata.get(key).is_some(),
        value: metadata.get(key).cloned().unwrap_or(Value::Null),
    }
}

/// The current generation's complete value is a CAS predicate; a
/// heartbeat racing this command produces a conflict and fresh read.
pub fn renew(metadata: &Value, now: DateTime<Utc>) -> Result<Command, String> {
    if metadata.get("gate_orphan_claim").is_some() {
        return Err("an original guarded negative decision prevents queue renewal".into());
    }
    if durable_presence(metadata) {
        return Err("resident owns the accepted request; a reader cannot renew or adopt it".into());
    }
    if native_observed(metadata) {
        return Err(
            "native launch identity already observed; attach rather than renew its queue place"
                .into(),
        );
    }
    let current = read(metadata)?;
    let stamp = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let keep = held(metadata, now);
    let row = Reservation {
        contract_version: 1,
        generation: current
            .as_ref()
            .filter(|_| keep)
            .map_or_else(Uuid::new_v4, |r| r.generation),
        queued_at: if keep {
            queued_at(metadata)
                .ok_or("held reservation lost its ordering stamp")?
                .to_owned()
        } else {
            stamp.clone()
        },
        heartbeat_at: stamp.clone(),
        consumed: false,
    };
    let mut guard = [
        KEY,
        SUBMISSION,
        OBSERVATION,
        "gate_orphan_claim",
        INTENT,
        ATTEMPT,
    ]
    .into_iter()
    .map(|key| (key.into(), expected(metadata, key)))
    .collect::<std::collections::BTreeMap<_, _>>();
    if current.is_none() {
        for key in ["queued_at", "queue_heartbeat_at"] {
            guard.insert(key.into(), expected(metadata, key));
        }
    }
    Ok(Command {
        contract_version: 1,
        idempotency_key: Uuid::new_v4(),
        expected: guard,
        patch: serde_json::from_value(
            json!({KEY:row,"queued_at":row.queued_at,"queue_heartbeat_at":stamp}),
        )
        .map_err(|e| e.to_string())?,
        immutable_keys: vec![],
        guarded_keys: vec![],
    })
}

pub fn consume(metadata: &Value, generation: Uuid) -> Result<Command, String> {
    if metadata.get("gate_orphan_claim").is_some() {
        return Err("an original guarded negative decision prevents native consumption".into());
    }
    let mut row = read(metadata)?.ok_or("no versioned reservation to consume")?;
    if row.generation != generation || row.consumed {
        return Err("reservation generation changed or was already consumed".into());
    }
    row.consumed = true;
    Ok(Command {
        contract_version: 1,
        idempotency_key: Uuid::new_v4(),
        expected: [KEY, OBSERVATION, "gate_orphan_claim"]
            .into_iter()
            .map(|key| (key.into(), expected(metadata, key)))
            .collect(),
        patch: serde_json::from_value(json!({KEY:row,"queued_at":null}))
            .map_err(|e| e.to_string())?,
        immutable_keys: vec![],
        guarded_keys: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_owner_does_not_expire_with_its_submitter() {
        let first = renew(&json!({}), at("2026-10-15T12:00:00Z")).unwrap();
        let mut md = Value::Object(first.patch);
        md["gate_submission"] = json!({"unreadable":"still not permission to expire or resend"});
        assert!(
            !stopped_reason(&md, Some(at("2026-10-16T12:00:00Z")))
                .is_some_and(|reason| reason.contains("expired"))
        );
        assert!(
            renew(&md, at("2026-10-16T12:00:00Z")).is_err(),
            "a resumed client cannot renew durable ownership"
        );
    }
    #[test]
    fn stale_queue_commands_cannot_cross_new_submission_observation_or_negative_claim() {
        let first = renew(&json!({}), at("2026-10-16T06:00:00Z")).unwrap();
        let metadata = Value::Object(first.patch);
        let heartbeat = renew(&metadata, at("2026-10-16T06:01:00Z")).unwrap();
        let consumed = consume(&metadata, read(&metadata).unwrap().unwrap().generation).unwrap();
        for key in [SUBMISSION, OBSERVATION, "gate_orphan_claim"] {
            let mut job = test_job();
            job.metadata = metadata.clone();
            job.metadata[key] = json!({"original":"another committed transition"});
            assert!(!heartbeat.matches(&job), "a stale heartbeat crossed {key}");
        }
        for key in [OBSERVATION, "gate_orphan_claim"] {
            let mut job = test_job();
            job.metadata = metadata.clone();
            job.metadata[key] = json!({"original":"another committed transition"});
            assert!(!consumed.matches(&job), "a stale consume crossed {key}");
        }
    }

    #[test]
    fn authored_gate_protocol_declares_all_native_first_records() {
        let workflows =
            crate::seed_loader::load_workflows(crate::registry::platform_bundle_path()).unwrap();
        let gate = workflows.iter().find(|row| row.kind == "gate-run").unwrap();
        assert_eq!(
            gate.metadata_schema.get("immutable_fields"),
            Some(&json!(FIRST_RECORD_FIELDS)),
            "the authored protocol must carry the first-write contract required by launchers"
        );
    }
    fn at(s: &str) -> DateTime<Utc> {
        instant(s).unwrap()
    }
    #[test]
    fn valid_legacy_resume_keeps_bytes_but_expired_resume_does_not_revive_age() {
        let old = json!({"queued_at":"2026-10-15T12:36:47+00:00","queue_heartbeat_at":"2026-10-15T12:38:48Z"});
        let resumed = renew(&old, at("2026-10-15T12:41:19Z")).unwrap();
        assert_eq!(resumed.patch[KEY]["queued_at"], old["queued_at"]);
        assert_eq!(resumed.patch[KEY]["heartbeat_at"], "2026-10-15T12:41:19Z");
        let expired = renew(&old, at("2026-10-15T12:45:19Z")).unwrap();
        assert_eq!(expired.patch[KEY]["queued_at"], "2026-10-15T12:45:19Z");
    }
    #[test]
    fn stale_consume_cannot_delete_a_replacement_and_legacy_overlay_is_not_authority() {
        let first = renew(&json!({}), at("2026-10-15T12:00:00Z")).unwrap();
        let mut md = Value::Object(first.patch);
        let generation = read(&md).unwrap().unwrap().generation;
        let stale = consume(&md, generation).unwrap();
        md["queued_at"] = json!("2020-01-01T00:00:00Z");
        assert_eq!(queued_at(&md), Some("2026-10-15T12:00:00Z"));
        let replacement = renew(&md, at("2026-10-15T13:00:00Z")).unwrap();
        let job = boss_core::job::Job {
            metadata: Value::Object(replacement.patch),
            ..test_job()
        };
        assert!(!stale.matches(&job));
    }
    fn test_job() -> boss_core::job::Job {
        use boss_core::job::{Job, JobStatus, Priority, Subject};
        Job {
            status: JobStatus::Open,
            ..Job::new(
                "gate-run",
                Subject::new("custom", "test"),
                "test",
                "test",
                Priority::Standard,
                at("2026-10-15T12:00:00Z").date_naive(),
            )
        }
    }
}
