//! Immutable first step evidence; ordinary metadata remains replaceable.
use boss_core::{
    job::{JobId, StepId},
    publisher::EventStamp,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The full server record, returned unchanged on replay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FirstRecord {
    pub job_id: JobId,
    pub step_id: StepId,
    pub key: String,
    pub value: Value,
    pub actor: boss_core::actor::ActorId,
    pub recorded_at: DateTime<Utc>,
    pub event_id: uuid::Uuid,
    /// Authoritative scalar spelling survives a JSONB projection round trip.
    pub value_json: String,
    pub canonical: Vec<u8>,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "result", content = "record", rename_all = "snake_case")]
pub enum FirstRecordResult {
    Recorded(FirstRecord),
    Replayed(FirstRecord),
    Conflict {
        job_id: JobId,
        step_id: StepId,
        key: String,
    },
    Terminal,
    NotFound,
}

impl FirstRecord {
    pub fn new(
        job_id: JobId,
        step_id: StepId,
        key: &str,
        value: &Value,
        stamp: &EventStamp,
        event_id: uuid::Uuid,
    ) -> Self {
        use sha2::{Digest, Sha256};
        let canonical = boss_core::job::canonical_json_bytes(value);
        Self {
            job_id,
            step_id,
            key: key.into(),
            value: value.clone(),
            actor: stamp.actor().clone(),
            recorded_at: stamp.timestamp,
            event_id,
            value_json: value.to_string(),
            digest: hex::encode(Sha256::digest(&canonical)),
            canonical,
        }
    }
    pub fn matches(&self, value: &Value) -> bool {
        self.canonical == boss_core::job::canonical_json_bytes(value)
    }
}

pub(crate) fn valid_key(key: &str) -> bool {
    !key.trim().is_empty() && key.len() <= 256
}

pub(crate) fn preserve<'a>(
    records: impl Iterator<Item = &'a FirstRecord>,
    metadata: &Value,
) -> Result<(), crate::port::JobsError> {
    for record in records {
        if !metadata
            .get(&record.key)
            .is_some_and(|value| record.matches(value))
        {
            return Err(crate::port::JobsError::FirstRecordImmutable {
                id: record.step_id,
                key: record.key.clone(),
            });
        }
    }
    Ok(())
}
