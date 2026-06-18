//! TOML configuration for the `boss-cybernetics` service binary.
//!
//! Example:
//! ```toml
//! vm_id = "os-worker-1"
//! nats_url = "nats://os-manager-1:4222"
//! http_bind = "0.0.0.0:7700"
//!
//! [[agents]]
//! id = "develop"
//! display_name = "Develop"
//! system_prompt = "You write and refactor code."
//! model = "claude-sonnet-4-6"
//! hourly_budget_usd_micros = 2_000_000  # $2.00/hr
//! max_concurrent_runs = 1
//! ```

use boss_core::agent::{AgentId, AgentIdError, AgentSpec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub vm_id: String,
    pub nats_url: String,
    pub http_bind: String,
    /// Postgres URL for the audit_log writer. Can also be provided via
    /// BOSS_POSTGRES_URL env var; the binary falls back to env if
    /// absent in the TOML.
    #[serde(default)]
    pub postgres_url: Option<String>,
    #[serde(default)]
    pub agents: Vec<AgentEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub system_prompt: String,
    pub model: String,
    pub hourly_budget_usd_micros: u64,
    pub max_concurrent_runs: u32,
    /// Optional schedule: "every 6h", "every 30m", "every 24h", etc.
    /// Omit for agents that only run on-demand.
    #[serde(default)]
    pub schedule: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid agent id `{id}`: {source}")]
    AgentId { id: String, source: AgentIdError },
    #[error("vm_id must not be empty")]
    EmptyVmId,
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.vm_id.is_empty() {
            return Err(ConfigError::EmptyVmId);
        }
        for a in &self.agents {
            AgentId::try_new(a.id.clone()).map_err(|e| ConfigError::AgentId {
                id: a.id.clone(),
                source: e,
            })?;
        }
        Ok(())
    }

    pub fn to_specs(&self) -> Result<Vec<AgentSpec>, ConfigError> {
        self.agents
            .iter()
            .map(|a| {
                let id = AgentId::try_new(a.id.clone()).map_err(|e| ConfigError::AgentId {
                    id: a.id.clone(),
                    source: e,
                })?;
                Ok(AgentSpec {
                    id,
                    display_name: a.display_name.clone(),
                    system_prompt: a.system_prompt.clone(),
                    model: a.model.clone(),
                    hourly_budget_usd_micros: a.hourly_budget_usd_micros,
                    max_concurrent_runs: a.max_concurrent_runs,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
vm_id = "os-worker-1"
nats_url = "nats://os-manager-1:4222"
http_bind = "0.0.0.0:7700"

[[agents]]
id = "develop"
display_name = "Develop"
system_prompt = "You write and refactor code."
model = "claude-sonnet-4-6"
hourly_budget_usd_micros = 2000000
max_concurrent_runs = 1

[[agents]]
id = "deploy"
display_name = "Deploy"
model = "claude-sonnet-4-6"
hourly_budget_usd_micros = 1000000
max_concurrent_runs = 1
"#;

    #[test]
    fn parses_sample_config() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.vm_id, "os-worker-1");
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.agents[0].id, "develop");
        assert_eq!(cfg.agents[1].system_prompt, "");
    }

    #[test]
    fn to_specs_produces_agent_specs() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        let specs = cfg.to_specs().unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].id.as_str(), "develop");
        assert_eq!(specs[1].hourly_budget_usd_micros, 1_000_000);
    }

    #[test]
    fn rejects_invalid_agent_id() {
        let bad = r#"
vm_id = "x"
nats_url = "y"
http_bind = "z"
[[agents]]
id = "Develop"
display_name = "d"
model = "m"
hourly_budget_usd_micros = 0
max_concurrent_runs = 1
"#;
        let cfg: Config = toml::from_str(bad).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::AgentId { .. }));
    }

    #[test]
    fn rejects_empty_vm_id() {
        let bad = r#"
vm_id = ""
nats_url = "y"
http_bind = "z"
"#;
        let cfg: Config = toml::from_str(bad).unwrap();
        assert!(matches!(
            cfg.validate().unwrap_err(),
            ConfigError::EmptyVmId
        ));
    }
}
