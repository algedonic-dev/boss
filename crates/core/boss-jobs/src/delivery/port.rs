//! Delivery-policy port — the two reads the train conductor needs from
//! the registry, and nothing else.
//!
//! No writes here. A policy change is a MIGRATION, the same way a
//! cadence threshold change is (114 -> 123 -> 131 -> 147 on
//! `cadence_rules`): retire the active row, insert the next version, so
//! "what was the policy when this train departed?" stays answerable
//! against the version the train pinned. An endpoint that let anything
//! mutate a row in place would take that answer away.

use async_trait::async_trait;

use super::types::DeliveryPolicyRow;

#[derive(Debug, thiserror::Error)]
pub enum DeliveryPolicyError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("storage: {0}")]
    Storage(String),
}

#[async_trait]
pub trait DeliveryPolicyRepository: Send + Sync {
    /// The active policy for `name`, or `None` when the registry holds
    /// none. `None` is not an error: the conductor answers it with its
    /// compiled fallback and a loud journal line.
    async fn active_policy(
        &self,
        name: &str,
    ) -> Result<Option<DeliveryPolicyRow>, DeliveryPolicyError>;

    /// One specific version, whatever its status — this is what an
    /// in-flight train reads, and a train may well outlive the
    /// retirement of the policy it departed under.
    async fn policy_version(
        &self,
        name: &str,
        version: i32,
    ) -> Result<Option<DeliveryPolicyRow>, DeliveryPolicyError>;
}
