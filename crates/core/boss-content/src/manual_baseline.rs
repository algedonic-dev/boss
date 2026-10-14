//! Present observations preserve legacy claims without fabricating past events.
use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{ContentError, ManualSection, ManualSectionVersion};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualSequence {
    pub last_value: i64,
    pub is_called: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualInventory {
    pub sections: Vec<Value>,
    pub history: Vec<Value>,
    pub sequence: ManualSequence,
    pub section_count: usize,
    pub history_count: usize,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManualBaseline {
    /// The caller's retry key, never the immutable fact's UUID.
    pub admission_id: Uuid,
    /// Independently stamped producer identity, persisted with the observation.
    pub event_id: Uuid,
    pub event_source: String,
    pub observed_at: DateTime<Utc>,
    pub observed_by: String,
    pub inventory: ManualInventory,
    pub log_cutoff: i64,
}

impl ManualBaseline {
    pub(crate) fn validate_fact(
        &self,
        event_id: Uuid,
        source: &str,
        timestamp: DateTime<Utc>,
        payload: &Value,
    ) -> Result<(), String> {
        self.inventory.validate()?;
        if self.admission_id.is_nil()
            || self.event_id.is_nil()
            || self.event_id == self.admission_id
            || self.event_id != event_id
            || self.event_source.trim().is_empty()
            || self.event_source != source
            || self.observed_at.timestamp_micros() != timestamp.timestamp_micros()
            || payload["_actor"].as_str() != Some(self.observed_by.as_str())
            || self.log_cutoff < 0
        {
            return Err("manual baseline witness disagrees with its stamped fact".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineAdmission {
    pub admission_id: Uuid,
    pub expected_sha256: String,
}

#[async_trait]
pub trait ManualBaselineRepository: Send + Sync {
    /// Complete, privileged raw state. Never an audience-filtered tree.
    async fn manual_inventory(&self) -> Result<ManualInventory, ContentError>;
    /// Compare the complete inventory under the protected boundary, then
    /// atomically record one present observation. Same admission is a retry.
    async fn observe_manual_baseline(
        &self,
        admission: &BaselineAdmission,
        stamp: &EventStamp,
    ) -> Result<ManualBaseline, ContentError>;
}

#[derive(Deserialize, Serialize)]
struct StoredHistory {
    id: i64,
    #[serde(flatten)]
    version: ManualSectionVersion,
}

impl ManualInventory {
    pub fn from_rows(
        sections: Vec<Value>,
        history: Vec<Value>,
        sequence: ManualSequence,
    ) -> Result<Self, String> {
        let mut inventory = Self {
            section_count: sections.len(),
            history_count: history.len(),
            sections,
            history,
            sequence,
            sha256: String::new(),
        };
        inventory.validate_rows()?;
        // UUIDs have one stored spelling; history has integer identities.
        inventory.sections.sort_by_key(|v| v["id"].to_string());
        inventory.history.sort_by_key(|v| v["id"].as_i64());
        inventory.sha256 = inventory.digest()?;
        Ok(inventory)
    }

    fn digest(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "sections": self.sections, "history": self.history, "sequence": self.sequence,
        }))
        .map_err(|e| e.to_string())?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_rows()?;
        if self.section_count != self.sections.len()
            || self.history_count != self.history.len()
            || self.sha256 != self.digest()?
        {
            return Err("manual inventory counts or hash disagree with its complete rows".into());
        }
        Ok(())
    }

    fn validate_rows(&self) -> Result<(), String> {
        if self.sections.is_empty() || self.sequence.last_value < 1 {
            return Err("manual inventory contains no legacy section or valid sequence".into());
        }
        let mut sections = BTreeMap::new();
        let mut slugs = BTreeMap::new();
        for raw in &self.sections {
            let s: ManualSection =
                serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
            complete_fields(raw, &serde_json::to_value(&s).map_err(|e| e.to_string())?)?;
            if s.current_version < 1
                || s.slug.trim().is_empty()
                || s.title.trim().is_empty()
                || slugs.insert(s.slug.clone(), s.id).is_some()
                || sections.insert(s.id, s).is_some()
            {
                return Err("manual inventory has invalid or repeated section identity".into());
            }
        }
        let mut ids = BTreeSet::new();
        let mut versions = BTreeMap::new();
        for raw in &self.history {
            let h: StoredHistory =
                serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
            complete_fields(raw, &serde_json::to_value(&h).map_err(|e| e.to_string())?)?;
            let v = &h.version;
            let s = sections
                .get(&v.section_id)
                .ok_or("manual history has no section")?;
            if h.id < 1
                || !ids.insert(h.id)
                || v.version < 1
                || v.version > s.current_version
                || versions.insert((v.section_id, v.version), h).is_some()
            {
                return Err("manual inventory has invalid or repeated history identity".into());
            }
        }
        for s in sections.values() {
            for version in 1..=s.current_version {
                if !versions.contains_key(&(s.id, version)) {
                    return Err("manual inventory has incomplete version coverage".into());
                }
            }
            let current = &versions
                .get(&(s.id, s.current_version))
                .ok_or("manual inventory has no current version")?
                .version;
            if current.title != s.title
                || current.body != s.body
                || current.audience != s.audience
                || current.edited_at != s.updated_at
            {
                return Err("manual current section disagrees with stored history".into());
            }
            let mut ancestors = BTreeSet::new();
            let mut parent = s.parent_slug.as_ref();
            while let Some(slug) = parent {
                let id = slugs
                    .get(slug)
                    .ok_or("manual inventory has a missing parent")?;
                if *id == s.id || !ancestors.insert(*id) {
                    return Err("manual inventory has a parent cycle".into());
                }
                parent = sections
                    .get(id)
                    .ok_or("manual inventory has a missing parent")?
                    .parent_slug
                    .as_ref();
            }
        }
        let max_id = ids
            .last()
            .copied()
            .ok_or("manual inventory has no history")?;
        if self.sequence.last_value < max_id
            || (!self.sequence.is_called && self.sequence.last_value == max_id)
        {
            return Err("manual inventory sequence would collide with stored history".into());
        }
        Ok(())
    }
}

fn complete_fields(raw: &Value, typed: &Value) -> Result<(), String> {
    let keys = |v: &Value| {
        v.as_object()
            .map(|m| m.keys().cloned().collect::<BTreeSet<_>>())
    };
    if keys(raw) != keys(typed) {
        return Err("manual inventory must carry every stored field and no unknown field".into());
    }
    Ok(())
}
