//! In-memory adapter — for tests and the `FakePolicyClient` backbone.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::port::{PolicyError, PolicyRepository, ReconcileStats};
use crate::types::{PolicyRule, UserOverride};

#[derive(Default)]
pub struct InMemoryPolicy {
    inner: Mutex<State>,
}

#[derive(Default)]
struct State {
    rules: HashMap<String, PolicyRule>,
    /// Tracks which rules came from a bootstrap seed. Mirrors the
    /// `updated_by = 'bootstrap'` discriminator the postgres adapter
    /// uses, so reconcile semantics match across adapters in tests.
    bootstrap_owned: std::collections::HashSet<String>,
    overrides: HashMap<String, UserOverride>,
}

impl InMemoryPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed with a fixed set of rules. Useful for tests that want a
    /// specific matrix without going through upsert_rule.
    pub fn with_rules(rules: impl IntoIterator<Item = PolicyRule>) -> Self {
        let me = Self::new();
        {
            let mut state = me.inner.lock().expect("poisoned lock");
            for r in rules {
                state.rules.insert(r.id.clone(), r);
            }
        }
        me
    }
}

#[async_trait]
impl PolicyRepository for InMemoryPolicy {
    async fn list_rules(&self) -> Result<Vec<PolicyRule>, PolicyError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state.rules.values().cloned().collect())
    }

    async fn rule_for(&self, id: &str) -> Result<Option<PolicyRule>, PolicyError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state.rules.get(id).cloned())
    }

    async fn upsert_rule(&self, rule: &PolicyRule, changed_by: &str) -> Result<(), PolicyError> {
        let mut state = self.inner.lock().expect("poisoned lock");
        state.rules.insert(rule.id.clone(), rule.clone());
        if changed_by == "bootstrap" {
            state.bootstrap_owned.insert(rule.id.clone());
        } else {
            state.bootstrap_owned.remove(&rule.id);
        }
        Ok(())
    }

    async fn deactivate_rule(&self, id: &str, _changed_by: &str) -> Result<(), PolicyError> {
        let mut state = self.inner.lock().expect("poisoned lock");
        match state.rules.get_mut(id) {
            Some(r) => {
                r.active = false;
                Ok(())
            }
            None => Err(PolicyError::NotFound(id.to_string())),
        }
    }

    async fn list_user_overrides(&self, user_id: &str) -> Result<Vec<UserOverride>, PolicyError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state
            .overrides
            .values()
            .filter(|o| o.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn upsert_user_override(
        &self,
        ov: &UserOverride,
        _changed_by: &str,
    ) -> Result<(), PolicyError> {
        let mut state = self.inner.lock().expect("poisoned lock");
        state.overrides.insert(ov.id.clone(), ov.clone());
        Ok(())
    }

    async fn deactivate_user_override(
        &self,
        id: &str,
        _changed_by: &str,
    ) -> Result<(), PolicyError> {
        let mut state = self.inner.lock().expect("poisoned lock");
        match state.overrides.get_mut(id) {
            Some(o) => {
                o.expires_at = Some(chrono::Utc::now());
                Ok(())
            }
            None => Err(PolicyError::NotFound(id.to_string())),
        }
    }

    async fn bootstrap_reconcile(
        &self,
        defaults: &[PolicyRule],
    ) -> Result<ReconcileStats, PolicyError> {
        let mut stats = ReconcileStats::default();
        let mut state = self.inner.lock().expect("poisoned lock");
        for rule in defaults {
            match state.rules.get(&rule.id) {
                None => {
                    state.rules.insert(rule.id.clone(), rule.clone());
                    state.bootstrap_owned.insert(rule.id.clone());
                    stats.inserted += 1;
                }
                Some(existing) => {
                    if state.bootstrap_owned.contains(&rule.id) {
                        if existing.scope != rule.scope || existing.active != rule.active {
                            state.rules.insert(rule.id.clone(), rule.clone());
                            stats.refreshed += 1;
                        } else {
                            stats.unchanged += 1;
                        }
                    } else {
                        stats.preserved += 1;
                    }
                }
            }
        }
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Action, Resource, Scope};

    #[tokio::test]
    async fn bootstrap_reconcile_inserts_missing_rules() {
        let repo = InMemoryPolicy::new();
        let defaults = vec![PolicyRule::new(
            "guest",
            Resource::job_kind(),
            Action::Read,
            Scope::All,
        )];
        let stats = repo.bootstrap_reconcile(&defaults).await.unwrap();
        assert_eq!(stats.inserted, 1);
        assert_eq!(stats.refreshed, 0);
        assert_eq!(stats.preserved, 0);
        assert!(
            repo.rule_for("guest:job-kind:read")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn bootstrap_reconcile_refreshes_drifted_bootstrap_rows() {
        let repo = InMemoryPolicy::new();
        // Seed an old bootstrap rule with the wrong scope.
        let stale = PolicyRule::new("cto", Resource::job_kind(), Action::Read, Scope::Self_);
        repo.upsert_rule(&stale, "bootstrap").await.unwrap();

        // Defaults now say Scope::All — reconcile should refresh.
        let defaults = vec![PolicyRule::new(
            "cto",
            Resource::job_kind(),
            Action::Read,
            Scope::All,
        )];
        let stats = repo.bootstrap_reconcile(&defaults).await.unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.refreshed, 1);
        assert_eq!(stats.preserved, 0);
        let live = repo.rule_for("cto:job-kind:read").await.unwrap().unwrap();
        assert_eq!(live.scope, Scope::All);
    }

    #[tokio::test]
    async fn bootstrap_reconcile_preserves_operator_edits() {
        let repo = InMemoryPolicy::new();
        // Operator-tuned rule (changed_by != "bootstrap").
        let custom = PolicyRule::new("cto", Resource::job_kind(), Action::Read, Scope::Self_);
        repo.upsert_rule(&custom, "emp-cto").await.unwrap();

        let defaults = vec![PolicyRule::new(
            "cto",
            Resource::job_kind(),
            Action::Read,
            Scope::All,
        )];
        let stats = repo.bootstrap_reconcile(&defaults).await.unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.refreshed, 0);
        assert_eq!(stats.preserved, 1);
        let live = repo.rule_for("cto:job-kind:read").await.unwrap().unwrap();
        assert_eq!(live.scope, Scope::Self_, "operator edit must survive");
    }

    #[tokio::test]
    async fn bootstrap_reconcile_no_op_when_already_matching() {
        let repo = InMemoryPolicy::new();
        let rule = PolicyRule::new("cto", Resource::job_kind(), Action::Read, Scope::All);
        repo.upsert_rule(&rule, "bootstrap").await.unwrap();

        let stats = repo.bootstrap_reconcile(&[rule]).await.unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.refreshed, 0);
        assert_eq!(stats.preserved, 0);
        assert_eq!(stats.unchanged, 1);
    }
}
