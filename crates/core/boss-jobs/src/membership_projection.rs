//! Durable membership generation port. Its watermark and derived state are
//! committed together; a viewer never loads packet history (c7870939).
use crate::{
    membership_replay::projection::MembershipProjection,
    port::{JobScope, JobsError},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
#[cfg(any(feature = "postgres", test))]
use serde_json::Map;
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_GENERATIONS: usize = 16;
pub const READER_IDLE: std::time::Duration = std::time::Duration::from_secs(600);

#[derive(Clone)]
pub struct ProjectionCursor {
    pub until: chrono::DateTime<chrono::Utc>,
    pub audit_cutoff: i64,
    pub currently_authorized: Option<std::collections::BTreeSet<String>>,
}

pub struct ProjectionSource {
    pub input: crate::membership_replay::ReplayInput,
    pub replacement_history: Option<crate::membership_replay::ReplayInput>,
    pub replacement_ids: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionIdentity {
    pub actor: Option<String>,
    pub scope: JobScope,
}

impl ProjectionIdentity {
    pub fn new(actor: Option<String>, mut scope: JobScope) -> Self {
        match &mut scope {
            JobScope::OwnerIn(ids) | JobScope::AccountIn(ids) => {
                ids.sort();
                ids.dedup();
            }
            _ => {}
        }
        Self { actor, scope }
    }

    pub(crate) fn key(&self) -> Result<String, JobsError> {
        use sha2::{Digest, Sha256};
        Ok(hex::encode(Sha256::digest(
            serde_json::to_vec(self).map_err(storage)?,
        )))
    }
}

pub struct StoredProjection {
    pub revision: i64,
    pub checkpoint: MembershipProjection,
}

pub struct ProjectionReading {
    pub revision: i64,
    pub reading: crate::membership_replay::MembershipReading,
}

#[derive(Debug)]
pub struct ProjectionWindows {
    pub revision: i64,
    pub until: chrono::DateTime<chrono::Utc>,
    pub this: crate::membership_replay::MembershipReading,
    pub both: Result<crate::membership_replay::MembershipReading, String>,
}

/// Canonical domain readings, not HTTP responses. Both are derived by the
/// original fold's bounded read at the checkpoint's one original cut.
#[cfg(feature = "postgres")]
#[derive(Serialize, Deserialize)]
pub(crate) struct CanonicalWindows {
    hours24: Result<crate::membership_replay::MembershipReading, String>,
    hours48: Result<crate::membership_replay::MembershipReading, String>,
}

#[cfg(feature = "postgres")]
impl CanonicalWindows {
    pub fn from_checkpoint(checkpoint: &MembershipProjection) -> Self {
        let binding = checkpoint.binding();
        Self {
            hours24: checkpoint
                .read(checkpoint.until() - chrono::Duration::hours(24), binding)
                .map_err(|e| e.to_string()),
            hours48: checkpoint
                .read(checkpoint.until() - chrono::Duration::hours(48), binding)
                .map_err(|e| e.to_string()),
        }
    }

    pub fn windows(
        self,
        revision: i64,
        hours: i64,
        until: chrono::DateTime<chrono::Utc>,
        cutoff: i64,
    ) -> Result<ProjectionWindows, JobsError> {
        for (duration, reading) in [(24, &self.hours24), (48, &self.hours48)] {
            match reading {
                Ok(reading)
                    if reading.until == until
                        && reading.since == until - chrono::Duration::hours(duration)
                        && reading.audit_cutoff == cutoff => {}
                Err(why) if !why.trim().is_empty() => {}
                _ => {
                    return Err(storage(
                        "canonical reading does not belong to its original generation cut",
                    ));
                }
            }
        }
        let (this, both) = match hours {
            24 => (self.hours24, self.hours48),
            48 => (
                self.hours48,
                Err("96-hour comparison exceeds the retained 48-hour membership history".into()),
            ),
            _ => {
                return Err(storage(
                    "membership projection window must be 24 or 48 hours",
                ));
            }
        };
        Ok(ProjectionWindows {
            revision,
            until,
            this: this.map_err(storage)?,
            both,
        })
    }
}

#[async_trait]
pub trait MembershipProjectionRepository: Send + Sync {
    /// A paired, cut-anchored view. Adapters may publish canonical summaries
    /// atomically with the checkpoint to avoid transferring every crossing
    /// on a viewer read. The default preserves the arbitrary-window port.
    async fn read_windows(
        &self,
        identity: &ProjectionIdentity,
        hours: i64,
        observed_at: chrono::DateTime<chrono::Utc>,
        binding: &crate::membership_replay::projection::ProjectionBinding,
    ) -> Result<Option<ProjectionWindows>, JobsError> {
        if !matches!(hours, 24 | 48) {
            return Err(storage(
                "membership projection window must be 24 or 48 hours",
            ));
        }
        let Some(witness) = self
            .read(
                identity,
                observed_at - chrono::Duration::hours(hours),
                binding,
            )
            .await?
        else {
            return Ok(None);
        };
        let this = self
            .read(
                identity,
                witness.reading.until - chrono::Duration::hours(hours),
                binding,
            )
            .await?
            .ok_or_else(|| storage("membership projection current generation disappeared"))?;
        if this.revision != witness.revision || this.reading.until != witness.reading.until {
            return Err(storage(
                "membership projection generation changed between header witness and current window",
            ));
        }
        let both = if hours == 24 {
            match self
                .read(
                    identity,
                    witness.reading.until - chrono::Duration::hours(48),
                    binding,
                )
                .await
            {
                Ok(Some(wide))
                    if wide.revision == this.revision
                        && wide.reading.until == this.reading.until =>
                {
                    Ok(wide.reading)
                }
                Ok(Some(_)) => {
                    return Err(storage(
                        "membership projection generation changed between paired reads",
                    ));
                }
                Ok(None) => {
                    return Err(storage(
                        "membership projection comparison generation disappeared",
                    ));
                }
                Err(error) => Err(error.to_string()),
            }
        } else {
            Err("96-hour comparison exceeds the retained 48-hour membership history".into())
        };
        Ok(Some(ProjectionWindows {
            revision: this.revision,
            until: this.reading.until,
            this: this.reading,
            both,
        }))
    }
    /// Viewer path: bounded crossing facts and header only. The caller
    /// supplies its freshly authorized binding, never a cached census.
    async fn read(
        &self,
        identity: &ProjectionIdentity,
        since: chrono::DateTime<chrono::Utc>,
        binding: &crate::membership_replay::projection::ProjectionBinding,
    ) -> Result<Option<ProjectionReading>, JobsError>;
    /// Recovery/updater path only: reconstruct the bounded latest state.
    async fn load(
        &self,
        identity: &ProjectionIdentity,
    ) -> Result<Option<StoredProjection>, JobsError>;
    /// Publication under the caller's original owned deadline/cancellation.
    /// Unsupported adapters refuse before writing; an acknowledged revision
    /// always means publication, even if its acknowledgement crosses a deadline.
    async fn commit_with_budget(
        &self,
        _identity: &ProjectionIdentity,
        _previous: Option<&StoredProjection>,
        _next: &MembershipProjection,
        _budget: &crate::membership_replay::ReplayBudget,
    ) -> Result<Option<i64>, JobsError> {
        Err(storage(
            "adapter does not support budget-aware projection publication",
        ))
    }

    /// None means another writer won; no part of this candidate committed.
    async fn commit(
        &self,
        identity: &ProjectionIdentity,
        previous: Option<&StoredProjection>,
        next: &MembershipProjection,
    ) -> Result<Option<i64>, JobsError>;
}

#[derive(Default)]
pub struct InMemoryMembershipProjection {
    entries: tokio::sync::Mutex<MemoryGenerations>,
}

#[derive(Default)]
struct MemoryGenerations {
    entries: BTreeMap<String, StoredProjection>,
    commit_order: std::collections::VecDeque<String>,
}

impl InMemoryMembershipProjection {
    pub fn new() -> Self {
        Self::default()
    }
    async fn publish(
        &self,
        identity: &ProjectionIdentity,
        previous: Option<&StoredProjection>,
        next: &MembershipProjection,
        budget: Option<&crate::membership_replay::ReplayBudget>,
    ) -> Result<Option<i64>, JobsError> {
        publication_check(budget)?;
        let next_rows = CheckpointRows::split(next)?;
        next_rows.require_identity(identity)?;
        let old = previous
            .map(|previous| CheckpointRows::split(&previous.checkpoint))
            .transpose()?;
        if let Some(previous) = previous {
            if let Some(old) = &old {
                old.require_identity(identity)?;
            }
            if next.audit_cutoff() < previous.checkpoint.audit_cutoff()
                || next.until() < previous.checkpoint.until()
            {
                return Err(storage("native generation watermark cannot move backwards"));
            }
        }
        let prepared = next.clone();
        let mut entries = self.entries.lock().await;
        publication_check(budget)?;
        let key = identity.key()?;
        let revision = entries.entries.get(&key).map_or(0, |entry| entry.revision);
        if revision != previous.map_or(0, |entry| entry.revision) {
            return Ok(None);
        }
        // Eviction can reuse a revision; a delta must still name its exact basis.
        if let Some(expected) = &old {
            let current = entries
                .entries
                .get(&key)
                .map(|entry| CheckpointRows::split(&entry.checkpoint))
                .transpose()?;
            if !current.is_some_and(|current| {
                current.header == expected.header && current.rows == expected.rows
            }) {
                return Ok(None);
            }
        }
        publication_check(budget)?;
        if let Some(old) = &old
            && old.header == next_rows.header
            && old.rows == next_rows.rows
        {
            return Ok(Some(revision));
        }
        let revision = revision
            .checked_add(1)
            .ok_or_else(|| storage("generation revision exhausted"))?;
        publication_check(budget)?;
        entries.commit_order.retain(|old| old != &key);
        entries.commit_order.push_back(key.clone());
        entries.entries.insert(
            key,
            StoredProjection {
                revision,
                checkpoint: prepared,
            },
        );
        while entries.entries.len() > MAX_GENERATIONS {
            if let Some(oldest) = entries.commit_order.pop_front() {
                entries.entries.remove(&oldest);
            }
        }
        Ok(Some(revision))
    }
}

#[async_trait]
impl MembershipProjectionRepository for InMemoryMembershipProjection {
    async fn read(
        &self,
        identity: &ProjectionIdentity,
        since: chrono::DateTime<chrono::Utc>,
        binding: &crate::membership_replay::projection::ProjectionBinding,
    ) -> Result<Option<ProjectionReading>, JobsError> {
        let entries = self.entries.lock().await;
        entries
            .entries
            .get(&identity.key()?)
            .map(|entry| {
                Ok(ProjectionReading {
                    revision: entry.revision,
                    reading: entry.checkpoint.read(since, binding).map_err(storage)?,
                })
            })
            .transpose()
    }
    async fn load(
        &self,
        identity: &ProjectionIdentity,
    ) -> Result<Option<StoredProjection>, JobsError> {
        Ok(self
            .entries
            .lock()
            .await
            .entries
            .get(&identity.key()?)
            .map(|entry| StoredProjection {
                revision: entry.revision,
                checkpoint: entry.checkpoint.clone(),
            }))
    }
    async fn commit_with_budget(
        &self,
        identity: &ProjectionIdentity,
        previous: Option<&StoredProjection>,
        next: &MembershipProjection,
        budget: &crate::membership_replay::ReplayBudget,
    ) -> Result<Option<i64>, JobsError> {
        self.publish(identity, previous, next, Some(budget)).await
    }
    async fn commit(
        &self,
        identity: &ProjectionIdentity,
        previous: Option<&StoredProjection>,
        next: &MembershipProjection,
    ) -> Result<Option<i64>, JobsError> {
        self.publish(identity, previous, next, None).await
    }
}

pub(crate) fn storage(why: impl std::fmt::Display) -> JobsError {
    JobsError::Storage(format!("membership projection: {why}"))
}

/// A storage representation, never a second evaluator. Splitting the
/// reducer's own serialization keeps unchanged packet payloads and source
/// witnesses out of steady-state writes. Recovery joins exactly that data.
pub(crate) struct CheckpointRows {
    pub header: Value,
    pub rows: BTreeMap<(String, String), Value>,
}

impl CheckpointRows {
    /// A deterministic witness of the complete delta basis, including row keys.
    /// Length framing separates JSON values; sorted maps remove insertion order.
    #[cfg(feature = "postgres")]
    pub fn basis_digest(&self) -> Result<String, JobsError> {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        let mut add = |value: Vec<u8>| {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        };
        add(serde_json::to_vec(&self.header).map_err(storage)?);
        for (key, value) in &self.rows {
            add(serde_json::to_vec(&(key, value)).map_err(storage)?);
        }
        Ok(hex::encode(digest.finalize()))
    }

    pub fn split(checkpoint: &MembershipProjection) -> Result<Self, JobsError> {
        Self::split_serialized(serde_json::to_value(checkpoint).map_err(storage)?)
    }

    fn split_serialized(mut header: Value) -> Result<Self, JobsError> {
        let object = header
            .as_object_mut()
            .ok_or_else(|| storage("checkpoint is not an object"))?;
        let mut rows = BTreeMap::new();
        let packets = object
            .get_mut("state")
            .and_then(Value::as_object_mut)
            .and_then(|s| s.remove("packets"))
            .and_then(|v| match v {
                Value::Object(values) => Some(values),
                _ => None,
            })
            .ok_or_else(|| storage("checkpoint packet state unavailable"))?;
        for (key, value) in packets {
            rows.insert(("packet".into(), key), value);
        }
        for (field, bucket) in [("facts", "fact"), ("seen", "seen"), ("group_spans", "span")] {
            let values = object
                .remove(field)
                .and_then(|v| match v {
                    Value::Array(values) => Some(values),
                    _ => None,
                })
                .ok_or_else(|| storage(format!("checkpoint {field} unavailable")))?;
            for (index, value) in values.into_iter().enumerate() {
                let key = if bucket == "seen" {
                    value
                        .as_str()
                        .ok_or_else(|| storage("source identity is not a UUID string"))?
                        .to_owned()
                } else {
                    format!("{index:020}")
                };
                rows.insert((bucket.into(), key), value);
            }
        }
        Ok(Self { header, rows })
    }

    #[cfg(feature = "postgres")]
    pub fn join(mut self) -> Result<MembershipProjection, JobsError> {
        let object = self
            .header
            .as_object_mut()
            .ok_or_else(|| storage("stored header is not an object"))?;
        let mut packets = Map::new();
        let (mut facts, mut seen, mut spans) = (vec![], vec![], vec![]);
        for ((bucket, key), value) in self.rows {
            match bucket.as_str() {
                "packet" => {
                    packets.insert(key, value);
                }
                "fact" => facts.push(value),
                "seen" => seen.push(value),
                "span" => spans.push(value),
                _ => return Err(storage("stored checkpoint has an unknown row bucket")),
            }
        }
        object
            .get_mut("state")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| storage("stored typed state unavailable"))?
            .insert("packets".into(), Value::Object(packets));
        object.insert("facts".into(), Value::Array(facts));
        object.insert("seen".into(), Value::Array(seen));
        object.insert("group_spans".into(), Value::Array(spans));
        serde_json::from_value(self.header).map_err(storage)
    }

    pub fn require_identity(&self, identity: &ProjectionIdentity) -> Result<(), JobsError> {
        let binding = self
            .header
            .get("binding")
            .ok_or_else(|| storage("checkpoint binding unavailable"))?;
        let actor = serde_json::to_value(&identity.actor).map_err(storage)?;
        let scope = serde_json::to_value(&identity.scope).map_err(storage)?;
        if binding.get("actor") != Some(&actor) || binding.get("scope") != Some(&scope) {
            return Err(storage("checkpoint belongs to a different caller or scope"));
        }
        Ok(())
    }
}

pub(crate) fn publication_check(
    budget: Option<&crate::membership_replay::ReplayBudget>,
) -> Result<(), JobsError> {
    if let Some(budget) = budget {
        budget.check().map_err(storage)?;
    }
    Ok(())
}

#[cfg(test)]
mod checkpoint_rows_tests {
    use super::*;

    #[cfg(feature = "postgres")]
    #[test]
    fn basis_digest_covers_the_entire_sorted_persisted_basis() {
        let header = serde_json::json!({"until": 40, "audit_cutoff": 3, "nested": {"b":2,"a":1}});
        let rows = [
            (
                ("fact".into(), "new-only".into()),
                serde_json::json!({"b":2,"a":1}),
            ),
            (("packet".into(), "old".into()), serde_json::json!({"v":1})),
        ];
        let basis = CheckpointRows {
            header: header.clone(),
            rows: rows.clone().into_iter().collect(),
        };
        let expected = basis.basis_digest().unwrap();
        let reordered = CheckpointRows {
            header: serde_json::from_str(r#"{"nested":{"a":1,"b":2},"audit_cutoff":3,"until":40}"#)
                .unwrap(),
            rows: rows.into_iter().rev().collect(),
        };
        assert_eq!(reordered.basis_digest().unwrap(), expected);
        for value in [serde_json::json!(41), serde_json::json!(4)] {
            let mut changed = CheckpointRows {
                header: header.clone(),
                rows: basis.rows.clone(),
            };
            let field = if value == 41 { "until" } else { "audit_cutoff" };
            changed.header[field] = value;
            assert_ne!(changed.basis_digest().unwrap(), expected);
        }
        for mode in 0..4 {
            let mut changed = CheckpointRows {
                header: header.clone(),
                rows: basis.rows.clone(),
            };
            let key = ("fact".into(), "new-only".into());
            match mode {
                0 => {
                    changed.rows.remove(&key);
                }
                1 => {
                    changed.rows.insert(key, serde_json::json!({"a":9,"b":2}));
                }
                2 => {
                    let row = changed.rows.remove(&key).unwrap();
                    changed
                        .rows
                        .insert(("fact".into(), "different-key".into()), row);
                }
                _ => {
                    changed.rows.insert(
                        ("seen".into(), "new-source".into()),
                        serde_json::json!("new-source"),
                    );
                }
            }
            assert_ne!(changed.basis_digest().unwrap(), expected, "mutation {mode}");
        }
    }

    #[test]
    fn malformed_serialized_shapes_keep_the_original_refusals() {
        let valid = serde_json::json!({
            "state": {"packets": {}}, "facts": [], "seen": [], "group_spans": []
        });
        let assert_refused = |value: Value, why: &str| {
            let error = match CheckpointRows::split_serialized(value) {
                Ok(_) => panic!("malformed checkpoint unexpectedly accepted: {why}"),
                Err(error) => error,
            };
            assert_eq!(error.to_string(), storage(why).to_string());
        };
        assert_refused(Value::Null, "checkpoint is not an object");
        for state in [
            Value::Null,
            serde_json::json!({}),
            serde_json::json!({"packets": []}),
        ] {
            let mut value = valid.clone();
            value["state"] = state;
            assert_refused(value, "checkpoint packet state unavailable");
        }
        let mut missing_state = valid.clone();
        missing_state.as_object_mut().unwrap().remove("state");
        assert_refused(missing_state, "checkpoint packet state unavailable");
        for field in ["facts", "seen", "group_spans"] {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert_refused(missing, &format!("checkpoint {field} unavailable"));
            for wrong in [Value::Null, serde_json::json!({})] {
                let mut value = valid.clone();
                value[field] = wrong;
                assert_refused(value, &format!("checkpoint {field} unavailable"));
            }
        }
        let mut nonstring = valid;
        nonstring["seen"] = serde_json::json!([42]);
        assert_refused(nonstring, "source identity is not a UUID string");
    }

    #[test]
    fn owned_extraction_preserves_nonempty_serialized_rows_and_original_bytes() {
        // Nested payloads and Unicode make a shallow/shape-only comparison insufficient.
        let original = serde_json::json!({
            "schema": 1, "binding": {"actor": "alice", "scope": "all"},
            "state": {"packets": {"packet-α": {"steps": [{"ask": "Keep café \"open\"?\nC:\\archive"}]}}, "other": [1,2]},
            "facts": [{"station": "ready", "source": {"ids": ["event-a", "event-b"]}}, {"station": "closed"}],
            "seen": ["00000000-0000-0000-0000-000000000001", "00000000-0000-0000-0000-000000000002"],
            "group_spans": [["2026-01-01", "2026-01-02"]]
        });
        let split = CheckpointRows::split_serialized(original.clone()).unwrap();
        let mut expected_header = original.clone();
        let object = expected_header.as_object_mut().unwrap();
        let mut expected_rows = BTreeMap::new();
        let packets = object
            .get_mut("state")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("packets")
            .unwrap();
        for (key, value) in packets.as_object().unwrap() {
            expected_rows.insert(("packet".into(), key.clone()), value.clone());
        }
        for (field, bucket) in [("facts", "fact"), ("seen", "seen"), ("group_spans", "span")] {
            let values = object.remove(field).unwrap();
            for (index, value) in values.as_array().unwrap().iter().enumerate() {
                let key = if bucket == "seen" {
                    value.as_str().unwrap().to_owned()
                } else {
                    format!("{index:020}")
                };
                expected_rows.insert((bucket.into(), key), value.clone());
            }
        }
        assert_eq!(
            serde_json::to_vec(&split.header).unwrap(),
            serde_json::to_vec(&expected_header).unwrap()
        );
        assert_eq!(split.rows, expected_rows);
        let mut joined = split.header;
        let object = joined.as_object_mut().unwrap();
        let mut packets = Map::new();
        for (bucket, field) in [("fact", "facts"), ("seen", "seen"), ("span", "group_spans")] {
            object.insert(
                field.into(),
                Value::Array(
                    split
                        .rows
                        .iter()
                        .filter(|((kind, _), _)| kind == bucket)
                        .map(|(_, v)| v.clone())
                        .collect(),
                ),
            );
        }
        for ((bucket, key), value) in split.rows {
            if bucket == "packet" {
                packets.insert(key, value);
            }
        }
        object
            .get_mut("state")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("packets".into(), Value::Object(packets));
        assert_eq!(
            serde_json::to_vec(&joined).unwrap(),
            serde_json::to_vec(&original).unwrap()
        );
    }
}
