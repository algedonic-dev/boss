//! Hexagonal port: `PolicyRepository` — what the engine needs from
//! persistence. Adapters implement it.

use async_trait::async_trait;

use crate::types::{PolicyRule, UserOverride};

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

#[async_trait]
pub trait PolicyRepository: Send + Sync {
    /// Every active rule in the store.
    async fn list_rules(&self) -> Result<Vec<PolicyRule>, PolicyError>;

    /// Single rule lookup. Returns None if no rule exists for this
    /// (role, resource, action) triple.
    async fn rule_for(&self, id: &str) -> Result<Option<PolicyRule>, PolicyError>;

    /// Upsert a rule. Idempotent: if a row with the same id exists,
    /// update it; otherwise insert. Writes a `rule.upsert` audit row.
    async fn upsert_rule(&self, rule: &PolicyRule, changed_by: &str) -> Result<(), PolicyError>;

    /// Soft-delete (sets `active=false`). Writes `rule.deactivate` audit.
    async fn deactivate_rule(&self, id: &str, changed_by: &str) -> Result<(), PolicyError>;

    /// Active (non-expired) overrides for one user.
    async fn list_user_overrides(&self, user_id: &str) -> Result<Vec<UserOverride>, PolicyError>;

    /// Upsert a user override. Writes `override.upsert` audit.
    async fn upsert_user_override(
        &self,
        ov: &UserOverride,
        changed_by: &str,
    ) -> Result<(), PolicyError>;

    /// Remove a user override (by setting expires_at to now). Writes
    /// `override.deactivate` audit.
    async fn deactivate_user_override(&self, id: &str, changed_by: &str)
    -> Result<(), PolicyError>;

    /// Reconcile the in-DB rules against a set of code-defined defaults.
    ///
    /// For each rule:
    ///   - Missing (no row with that id) → insert as `updated_by =
    ///     'bootstrap'`.
    ///   - Present and bootstrap-owned (`updated_by = 'bootstrap'`) but
    ///     drifted from the default body (scope or active changed) →
    ///     upsert, restamping `updated_by = 'bootstrap'`.
    ///   - Present and operator-owned (any other `updated_by`) →
    ///     preserve the operator edit untouched.
    ///
    /// This is the design's escape valve from the original "seed only
    /// if missing" loop, which silently let a default's scope drift
    /// out of the live DB whenever the code-side default changed
    /// after first boot. Bootstrap rules now self-heal on every
    /// service restart; operator-tuned rules still survive.
    async fn bootstrap_reconcile(
        &self,
        defaults: &[PolicyRule],
    ) -> Result<ReconcileStats, PolicyError>;
}

/// Result of a `bootstrap_reconcile` call. Counts each branch so the
/// service log records how much drift was healed on this boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileStats {
    /// New rule rows inserted.
    pub inserted: usize,
    /// Bootstrap-owned rows whose scope or active flag was refreshed
    /// to match the current code default.
    pub refreshed: usize,
    /// Operator-edited rows left untouched.
    pub preserved: usize,
    /// Bootstrap-owned rows already matching the default — no write.
    pub unchanged: usize,
}
