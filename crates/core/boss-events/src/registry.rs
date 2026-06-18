//! In-memory [`AgentRegistry`] adapter.

use async_trait::async_trait;
use boss_core::agent::{AgentId, AgentSpec};
use boss_core::port::{AgentRegistry, RegistryError};
use std::collections::HashMap;
use std::sync::Arc;

/// Immutable in-memory registry. Built once at startup from config.
#[derive(Clone)]
pub struct InMemoryAgentRegistry {
    specs: Arc<HashMap<AgentId, AgentSpec>>,
}

impl InMemoryAgentRegistry {
    pub fn new(specs: impl IntoIterator<Item = AgentSpec>) -> Self {
        let map: HashMap<_, _> = specs.into_iter().map(|s| (s.id.clone(), s)).collect();
        Self {
            specs: Arc::new(map),
        }
    }
}

#[async_trait]
impl AgentRegistry for InMemoryAgentRegistry {
    async fn list(&self) -> Result<Vec<AgentSpec>, RegistryError> {
        Ok(self.specs.values().cloned().collect())
    }

    async fn get(&self, agent: &AgentId) -> Result<Option<AgentSpec>, RegistryError> {
        Ok(self.specs.get(agent).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(slug: &str) -> AgentSpec {
        AgentSpec {
            id: AgentId::try_new(slug).unwrap(),
            display_name: slug.into(),
            system_prompt: String::new(),
            model: "test".into(),
            hourly_budget_usd_micros: 1_000,
            max_concurrent_runs: 1,
        }
    }

    #[tokio::test]
    async fn list_returns_all_specs() {
        let r = InMemoryAgentRegistry::new([spec("planner"), spec("doctor")]);
        let listed = r.list().await.unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[tokio::test]
    async fn get_returns_known_spec_and_none_for_unknown() {
        let r = InMemoryAgentRegistry::new([spec("planner")]);
        let known = AgentId::try_new("planner").unwrap();
        let unknown = AgentId::try_new("missing").unwrap();
        assert!(r.get(&known).await.unwrap().is_some());
        assert!(r.get(&unknown).await.unwrap().is_none());
    }
}
