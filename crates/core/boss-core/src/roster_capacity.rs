//! A narrow capacity observation. Eligibility is registry data; this port
//! neither identifies an actor nor grants one authority.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassRequirement {
    pub attribute: String,
    pub codes: Vec<String>,
}

pub fn validate_eligibility(eligibility: &[ClassRequirement]) -> Result<(), String> {
    if eligibility.is_empty() || eligibility.len() > 16 {
        return Err("eligibility must name 1..=16 Class axes".into());
    }
    let mut axes = BTreeSet::new();
    for requirement in eligibility {
        let unique_codes: BTreeSet<_> = requirement.codes.iter().collect();
        if requirement.attribute.is_empty()
            || requirement.attribute.len() > 128
            || !requirement
                .attribute
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c == b'_')
            || !axes.insert(&requirement.attribute)
            || requirement.codes.is_empty()
            || requirement.codes.len() > 128
            || unique_codes.len() != requirement.codes.len()
            || requirement
                .codes
                .iter()
                .any(|code| code.trim().is_empty() || code.len() > 128)
        {
            return Err(
                "eligibility needs distinct named axes and nonempty bounded Class codes".into(),
            );
        }
    }
    Ok(())
}

/// Only an aggregate and its source basis cross the port. No person ID,
/// credentials, name, compensation or other private profile can ride it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterSnapshot {
    pub eligibility: Vec<ClassRequirement>,
    /// None is a qualified history with no facts, not a fabricated sequence 0.
    pub revision: Option<i64>,
    pub count: u64,
}

impl RosterSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        validate_eligibility(&self.eligibility)?;
        if self.revision.is_some_and(|seq| seq < 1) || (self.count > 0 && self.revision.is_none()) {
            return Err("a nonempty roster needs a positive source audit-sequence basis".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RosterSnapshotError {
    #[error("roster capacity unavailable: {0}")]
    Unavailable(String),
    #[error("invalid roster eligibility: {0}")]
    Invalid(String),
}

#[async_trait]
pub trait RosterCapacitySnapshot: Send + Sync {
    async fn snapshot(
        &self,
        _eligibility: &[ClassRequirement],
    ) -> Result<RosterSnapshot, RosterSnapshotError> {
        Err(RosterSnapshotError::Unavailable(
            "adapter does not supply a qualified snapshot".into(),
        ))
    }
}
