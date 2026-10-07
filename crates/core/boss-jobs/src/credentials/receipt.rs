//! Original, value-free phase observations. The owner, not the caller,
//! assigns the event, actor and instant. Canonical bytes retain scalar spelling.
use boss_core::{event::Event, publisher::EventStamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CredentialsError, RotationPhase};

pub const RECEIPT_KEY: &str = "_rotation_receipt";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotationReceipt {
    pub version: u32,
    pub credential_id: String,
    pub phase: String,
    pub observation_id: String,
    pub actor: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub event_id: uuid::Uuid,
    pub evidence_json: String,
    pub canonical: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RotationOutcome {
    LegacyRecorded,
    Recorded { receipt: RotationReceipt },
    Replayed { receipt: RotationReceipt },
}

pub fn observation(evidence: &mut Value) -> Result<Option<String>, CredentialsError> {
    let Some(map) = evidence.as_object_mut() else {
        return Err(CredentialsError::InvalidObservation(
            "evidence must be an object".into(),
        ));
    };
    if map.contains_key(RECEIPT_KEY) {
        return Err(CredentialsError::InvalidObservation(
            "reserved owner receipt".into(),
        ));
    }
    let Some(value) = map.remove("observation_id") else {
        return Ok(None);
    };
    if [RECEIPT_KEY, "_actor", "_partition", "_simulated"]
        .iter()
        .any(|key| map.contains_key(*key))
    {
        return Err(CredentialsError::InvalidObservation(
            "reserved owner evidence".into(),
        ));
    }
    let Some(id) = value.as_str().filter(|id| {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
    }) else {
        return Err(CredentialsError::InvalidObservation(
            "observation_id must be a bounded durable identifier".into(),
        ));
    };
    Ok(Some(id.to_owned()))
}

impl RotationReceipt {
    pub fn new(
        id: &str,
        phase: RotationPhase,
        observation_id: String,
        evidence: &Value,
        stamp: &EventStamp,
    ) -> Result<Self, CredentialsError> {
        Ok(Self {
            version: 1,
            credential_id: id.into(),
            phase: phase.as_str().into(),
            observation_id,
            actor: stamp.actor().to_string(),
            timestamp: stamp.timestamp,
            event_id: uuid::Uuid::new_v4(),
            evidence_json: serde_json::to_string(evidence)
                .map_err(|e| CredentialsError::Storage(e.to_string()))?,
            canonical: boss_core::job::canonical_json_bytes(evidence),
        })
    }

    pub fn matches(&self, evidence: &Value, stamp: &EventStamp) -> bool {
        let valid_original =
            serde_json::from_str::<Value>(&self.evidence_json).is_ok_and(|original| {
                boss_core::job::canonical_json_bytes(&original) == self.canonical
            });
        self.version == 1
            && !self.event_id.is_nil()
            && !self.actor.is_empty()
            && !self.credential_id.is_empty()
            && RotationPhase::parse(&self.phase).is_some()
            && valid_original
            && self.actor == stamp.actor().to_string()
            && self.canonical == boss_core::job::canonical_json_bytes(evidence)
    }

    pub fn event(
        &self,
        stamp: &EventStamp,
        evidence: Value,
        phase: RotationPhase,
    ) -> Result<Event, CredentialsError> {
        let mut event = stamp.event(phase.event_kind(), evidence);
        event.id = self.event_id;
        event.payload["credential_id"] = Value::String(self.credential_id.clone());
        event.payload[RECEIPT_KEY] =
            serde_json::to_value(self).map_err(|e| CredentialsError::Storage(e.to_string()))?;
        Ok(event)
    }

    pub fn from_event(event: &Event) -> Result<Option<Self>, CredentialsError> {
        let Some(value) = event.payload.get(RECEIPT_KEY) else {
            return Ok(None);
        };
        let receipt: Self = serde_json::from_value(value.clone())
            .map_err(|_| CredentialsError::ObservationConflict)?;
        let evidence: Value = serde_json::from_str(&receipt.evidence_json)
            .map_err(|_| CredentialsError::ObservationConflict)?;
        let phase =
            RotationPhase::parse(&receipt.phase).ok_or(CredentialsError::ObservationConflict)?;
        let mut payload = event.payload.clone();
        let map = payload
            .as_object_mut()
            .ok_or(CredentialsError::ObservationConflict)?;
        map.remove(RECEIPT_KEY);
        map.remove("_actor");
        map.remove("_partition");
        map.remove("_simulated");
        if evidence.get("credential_id").is_none() {
            map.remove("credential_id");
        }
        if receipt.version != 1 || receipt.event_id.is_nil() || receipt.event_id != event.id
            || receipt.timestamp.timestamp_micros() != event.timestamp.timestamp_micros()
            || event.kind != phase.event_kind() || event.source != "jobs"
            || event.payload["credential_id"] != receipt.credential_id
            || event.payload["_actor"] != receipt.actor
            || boss_core::job::canonical_json_bytes(&evidence) != receipt.canonical
            // JSONB can normalize a redundant numeric projection (-0.0
            // becomes 0.0). The immutable original JSON and canonical
            // bytes remain authoritative; compare the projection as data.
            || payload != evidence
            || receipt.actor.is_empty() || receipt.credential_id.is_empty()
        {
            return Err(CredentialsError::ObservationConflict);
        }
        let mut control = evidence.clone();
        control["observation_id"] = Value::String(receipt.observation_id.clone());
        observation(&mut control)?;
        Ok(Some(receipt))
    }
}
