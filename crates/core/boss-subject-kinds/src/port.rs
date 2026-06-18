//! Hexagonal port: `SubjectKindRepository` defines what the domain
//! needs from the SubjectKind persistence layer.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One row of the `subject_kinds` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectKind {
    pub kind: String,
    pub label: String,
    /// Self-referential parent — Account can have parent='account',
    /// Recipe parent='product', etc. Lets tenants declare hierarchies
    /// without forking core. `None` for top-level kinds.
    pub parent_kind: Option<String>,
    pub description: Option<String>,
    /// `system` for core kinds shipped by Boss; tenant id (or
    /// 'brewery' / 'used-device-shop') for tenant-extended kinds.
    pub owning_team: String,
    pub metadata: serde_json::Value,
    pub sort_order: i32,
    /// Set when a kind is retired. Retired kinds are still readable
    /// (`get`) so audit / migration code can resolve old ids, but
    /// `exists_active` returns false.
    pub retired_at: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SubjectKindError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
}

/// Persistence port for the Subject Kind registry.
///
/// v1 is read-only — seeds live in the migration; authoring lands
/// when the admin UI does. Methods cover what downstream services +
/// the read-side UI need today:
///
/// - `get` returns a single kind (including retired rows).
/// - `exists_active` is the hot-path validator — every write that
///   sets a Subject's kind calls this to reject typos / unregistered
///   kinds.
/// - `list_active` powers the admin / picker UI.
/// - `children_of` walks `parent_kind` hierarchies (e.g. all kinds
///   whose parent is `account`).
#[async_trait]
pub trait SubjectKindRepository: Send + Sync {
    async fn get(&self, kind: &str) -> Result<Option<SubjectKind>, SubjectKindError>;
    async fn exists_active(&self, kind: &str) -> Result<bool, SubjectKindError>;
    async fn list_active(&self) -> Result<Vec<SubjectKind>, SubjectKindError>;
    async fn children_of(&self, parent_kind: &str) -> Result<Vec<SubjectKind>, SubjectKindError>;
}
