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

#[async_trait]
pub trait RoleOfRecord: Send + Sync {
    async fn role_for(&self, actor: &str) -> Result<Option<RoleRecord>, RoleLookupError>;
}
