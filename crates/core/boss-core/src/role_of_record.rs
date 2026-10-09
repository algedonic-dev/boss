//! The caller's role as the record holds it (design abf9eeae, car 2).
//! Registry adapters answer this port; no role taxonomy or privilege
//! inference belongs here. A row with no role differs from no row, and
//! neither is a registry that could not answer.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleRecord {
    /// The registry's canonical identity, including when an alias matched.
    pub actor_id: String,
    /// A Class code, never a closed list of platform roles.
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoleLookupError {
    #[error("role registry unavailable: {0}")]
    Unavailable(String),
    #[error("role registry ambiguous: {0}")]
    Ambiguous(String),
}

/// Whether `actor` is a member of the family a registry row declares in
/// `signs_for` — an id its signer writes one per firing
/// (`automation:rule:<name>` under the dispatcher's `automation:rule:`).
/// The prefix alone names no member.
///
/// ONE definition, read by the registry's own `row_of_record`
/// (`boss-jobs`, which answers the audit log's actors) and by the
/// resolver every service mounts (`boss-policy-client`, which answers a
/// request's). They were two readers of one field and only the first
/// read it: measured 2026-10-07, the resolver called 34 dispatcher rules
/// unregistered while their row sat in the registry (backlog ddf0773e).
pub fn is_family_member(signs_for: &str, actor: &str) -> bool {
    actor.len() > signs_for.len() && actor.starts_with(signs_for)
}

#[async_trait]
pub trait RoleOfRecord: Send + Sync {
    async fn role_for(&self, actor: &str) -> Result<Option<RoleRecord>, RoleLookupError>;
}
