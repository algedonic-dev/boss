//! Conditional metadata writes and their immutable, actor-bound receipts.
//! The row, receipt and events commit together (design 2bd4a54e).
use boss_core::job::{Job, JobId, JobStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub present: bool,
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub contract_version: u32,
    pub idempotency_key: Uuid,
    pub expected: std::collections::BTreeMap<String, Expected>,
    pub patch: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub immutable_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guarded_keys: Vec<String>,
}

impl Command {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !matches!(self.contract_version, 1 | 2) || self.idempotency_key.is_nil() {
            return Err(
                "conditional metadata requires supported contract_version and a non-nil idempotency_key",
            );
        }
        if self.expected.is_empty() || self.patch.is_empty() {
            return Err("conditional metadata requires nonempty expected and patch objects");
        }
        if (self.contract_version == 1 && !self.guarded_keys.is_empty())
            || self
                .patch
                .contains_key(crate::metadata_immutability::GUARDED_KEY)
            || self.guarded_keys.iter().enumerate().any(|(index, key)| {
                key.is_empty()
                    || key == crate::metadata_immutability::KEY
                    || key == crate::metadata_immutability::GUARDED_KEY
                    || self.guarded_keys[..index].contains(key)
                    || self.immutable_keys.contains(key)
                    || self
                        .expected
                        .get(key)
                        .is_none_or(|expected| expected.present)
                    || self.patch.get(key).is_none_or(Value::is_null)
            })
        {
            return Err(
                "guarded first bindings require version2, unique non-reserved absent guards and non-null patches",
            );
        }
        if self.patch.contains_key(crate::metadata_immutability::KEY)
            || self.immutable_keys.iter().any(|key| {
                !self.patch.contains_key(key)
                    || !self.expected.contains_key(key)
                    || key == crate::metadata_immutability::KEY
            })
        {
            return Err(
                "immutable fields must be patched and guarded; the binding record is server-owned",
            );
        }
        if self.expected.iter().any(|(key, expected)| {
            key.is_empty() || (!expected.present && !expected.value.is_null())
        }) {
            return Err("an absent expected key has no value");
        }
        Ok(())
    }

    pub fn matches(&self, job: &Job) -> bool {
        job.status == JobStatus::Open
            && self.expected.iter().all(|(key, expected)| {
                match (job.metadata.get(key), expected.present) {
                    (None, false) => true,
                    (Some(value), true) => value == &expected.value,
                    _ => false,
                }
            })
    }

    pub fn apply(&self, job: &Job) -> Result<Job, crate::port::JobsError> {
        let mut metadata = job.metadata.as_object().cloned().unwrap_or_default();
        for (key, value) in &self.patch {
            if value.is_null() {
                metadata.remove(key);
            } else {
                metadata.insert(key.clone(), value.clone());
            }
        }
        let mut next = Job {
            metadata: Value::Object(metadata),
            ..job.clone()
        };
        crate::metadata_immutability::apply_guarded(&job.metadata, &mut next.metadata, self)
            .map_err(crate::port::JobsError::ImmutableMetadata)?;
        crate::metadata_immutability::bind(&job.metadata, &mut next.metadata, &self.immutable_keys)
            .map_err(crate::port::JobsError::ImmutableMetadata)?;
        Ok(next)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub contract_version: u32,
    pub job_id: JobId,
    pub actor: String,
    pub command: Command,
    pub applied_at: chrono::DateTime<chrono::Utc>,
    pub event_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_event_id: Option<Uuid>,
    pub job: Job,
}

/// An adapter decision, not part of the immutable receipt. Replaying a
/// transaction must not wake readiness against its historical post-state.
pub struct Outcome {
    pub receipt: Receipt,
    pub applied: bool,
}

pub const EVENT: &str = "jobs.metadata.conditionally_merged";
pub const RECEIPT_LINK: &str = "_conditional_receipt_event_id";

#[cfg(feature = "postgres")]
pub(crate) mod replay;

/// The existing state fact and original receipt form one transaction record.
/// Version1 keeps its historical serialized receipt shape.
pub fn recorded_pair(
    stamp: &boss_core::publisher::EventStamp,
    command: &Command,
    job: Job,
) -> Result<(Receipt, [boss_core::event::Event; 2]), crate::port::JobsError> {
    let mut state = stamp.event(
        crate::events::JOB_UPDATED,
        serde_json::to_value(&job)
            .map_err(|error| crate::port::JobsError::Storage(error.to_string()))?,
    );
    let marker_id = (command.contract_version == 2).then(Uuid::new_v4);
    if let Some(id) = marker_id {
        state
            .payload
            .as_object_mut()
            .ok_or_else(|| {
                crate::port::JobsError::Storage("conditional post-state is not an object".into())
            })?
            .insert(RECEIPT_LINK.into(), serde_json::json!(id));
    }
    let receipt = Receipt {
        contract_version: command.contract_version,
        job_id: job.id,
        actor: stamp.actor().to_string(),
        command: command.clone(),
        applied_at: stamp.timestamp,
        event_id: state.id,
        receipt_event_id: marker_id,
        job,
    };
    let mut marker = stamp.event(
        EVENT,
        serde_json::to_value(&receipt)
            .map_err(|error| crate::port::JobsError::Storage(error.to_string()))?,
    );
    if let Some(id) = marker_id {
        marker.id = id;
    }
    Ok((receipt, [state, marker]))
}
