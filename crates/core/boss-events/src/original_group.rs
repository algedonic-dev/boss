//! Portable original transaction evidence; storage adapters conserve
//! these values and pure historical readers validate the same roster.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupReference {
    pub id: Uuid,
    pub schema_version: i32,
    pub event_count: i64,
    pub member_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginalGroup {
    pub reference: GroupReference,
    pub event_ids: Vec<Uuid>,
    pub audit_sequences: Vec<Option<i64>>,
    pub complete: bool,
}

impl OriginalGroup {
    /// UUID strings have no escaping ambiguity. This is Postgres's
    /// jsonb UUID-array text encoding, pinned against its producer in
    /// the actual PostgreSQL contract tests.
    pub fn roster_hash(ids: &[Uuid]) -> String {
        let text = format!(
            "[{}]",
            ids.iter()
                .map(|id| format!("\"{id}\""))
                .collect::<Vec<_>>()
                .join(", ")
        );
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn validate(&self, cutoff: i64) -> Result<bool, String> {
        let ids: std::collections::BTreeSet<_> = self.event_ids.iter().collect();
        let seqs: std::collections::BTreeSet<_> = self.audit_sequences.iter().flatten().collect();
        if self.reference.schema_version != 1
            || self.reference.event_count < 1
            || self.reference.event_count as usize != self.event_ids.len()
            || self.audit_sequences.len() != self.event_ids.len()
            || ids.len() != self.event_ids.len()
            || seqs.len() != self.audit_sequences.iter().flatten().count()
            || seqs.iter().any(|seq| **seq <= 0 || **seq > cutoff)
            || self.reference.member_hash != Self::roster_hash(&self.event_ids)
            || self.complete != self.audit_sequences.iter().all(Option::is_some)
        {
            return Err(
                "original transaction reference, roster, cutoff or completeness differs".into(),
            );
        }
        Ok(self.complete)
    }
}
