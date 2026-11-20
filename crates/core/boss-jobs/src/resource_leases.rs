//! Lease and physical-return contracts. A logical expiry never proves that
//! a worker or its storage stopped existing.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    flow_resources::{Allocation, Claim},
    port::JobsError,
};
pub const IDENTITY_BOUND: &str = "jobs.step.resource_identity_bound";
pub const LEASE_EXPIRED: &str = "jobs.step.resource_lease_expired";
pub const STOCK_RETURNED: &str = "jobs.step.stock_returned";
pub const MAX_SWEEP: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceIdentity {
    pub namespace: String,
    pub job_uid: Uuid,
    pub pvc_uid: Uuid,
    pub run_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityBinding {
    pub acquisition: Uuid,
    pub generation: i64,
    pub identity: ResourceIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StockObligation {
    pub claim: Claim,
    pub allocation: Allocation,
    pub reclaimer: String,
    pub identity: Option<ResourceIdentity>,
    pub returned_by: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepResult {
    pub observed_at: DateTime<Utc>,
    pub expired: usize,
}

/// This type intentionally has no Deserialize: request JSON is not a verified
/// machine observation. Only an installed trusted adapter constructs it.
pub struct VerifiedStockReturn {
    pub(crate) obligation: StockObligation,
    pub(crate) evidence: Uuid,
}
impl VerifiedStockReturn {
    /// Called only by a trusted verifier after qualifying its operation.
    pub fn from_qualified_operation(obligation: StockObligation, evidence: Uuid) -> Self {
        Self {
            obligation,
            evidence,
        }
    }
}
pub(crate) fn error(reason: &str) -> JobsError {
    JobsError::ResourceRefused(crate::flow_resources::Refusal {
        pool: "acquisition".into(),
        reason: reason.into(),
        observed: Vec::new(),
    })
}
pub(crate) fn validate_binding(
    claim: &Claim,
    active: bool,
    actor: &str,
    binding: &IdentityBinding,
    now: DateTime<Utc>,
) -> Result<(), JobsError> {
    if !active
        || claim.id != binding.acquisition
        || claim.generation != binding.generation
        || claim.holder != actor
    {
        return Err(error("resource-binding-generation-holder-mismatch"));
    }
    if claim.lease_until.is_some_and(|until| until <= now) {
        return Err(error("resource-lease-expired"));
    }
    let i = &binding.identity;
    if i.namespace.is_empty()
        || i.namespace.len() > 63
        || i.namespace.starts_with('-')
        || i.namespace.ends_with('-')
        || !i
            .namespace
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || i.job_uid.is_nil()
        || i.pvc_uid.is_nil()
        || i.run_id.is_nil()
    {
        return Err(error("invalid-resource-identity"));
    }
    Ok(())
}
pub(crate) fn obligations_for(
    claim: &Claim,
    identity: Option<ResourceIdentity>,
    pools: &[crate::pools::PoolSpec],
) -> Result<Vec<StockObligation>, JobsError> {
    claim
        .allocations
        .iter()
        .filter(|a| claim.observed.iter().any(|d| d.pool == a.pool && d.stock))
        .map(|a| {
            let p = pools
                .iter()
                .find(|p| {
                    p.name == a.pool
                        && p.version == a.version
                        && p.kind == crate::pools::PoolKind::Stock
                })
                .ok_or_else(|| error("stock-return-contract-unavailable"))?;
            let reclaimer = p
                .reclaimer
                .clone()
                .filter(|r| !r.trim().is_empty())
                .ok_or_else(|| error("stock-reclaimer-unavailable"))?;
            Ok(StockObligation {
                claim: claim.clone(),
                allocation: a.clone(),
                reclaimer,
                identity: identity.clone(),
                returned_by: None,
            })
        })
        .collect()
}
pub(crate) fn validate_return(
    current: &StockObligation,
    actor: &str,
    evidence: Uuid,
    verified: &VerifiedStockReturn,
) -> Result<(), JobsError> {
    if current.reclaimer != actor {
        return Err(error("stock-reclaimer-mismatch"));
    }
    if current.identity.is_none() {
        return Err(error("stock-resource-identity-unbound"));
    }
    if evidence.is_nil() || verified.evidence != evidence || verified.obligation != *current {
        return Err(error("stock-return-evidence-mismatch"));
    }
    Ok(())
}

#[async_trait]
pub trait ResourceReturnVerifier: Send + Sync {
    async fn verify(
        &self,
        obligation: &StockObligation,
        evidence: Uuid,
    ) -> Result<VerifiedStockReturn, JobsError>;
}

pub struct NoQualifiedReturnAdapter;
#[async_trait]
impl ResourceReturnVerifier for NoQualifiedReturnAdapter {
    async fn verify(&self, _: &StockObligation, _: Uuid) -> Result<VerifiedStockReturn, JobsError> {
        Err(JobsError::Storage("no qualified return adapter".into()))
    }
}
