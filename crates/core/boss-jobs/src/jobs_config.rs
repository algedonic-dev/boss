//! Configuration for the `boss-jobs-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct JobsApiConfig {
    pub nats_url: String,
    pub http_bind: String,
    /// When set, the jobs API uses Postgres for durable storage.
    /// When absent, falls back to in-memory (jobs lost on restart).
    pub postgres_url: Option<String>,
    /// Base URL for the boss-calendar HTTP API. Optional —
    /// boss-jobs-api runs without it, and the calendar reservation
    /// hook on step transitions becomes a no-op. Set this once the
    /// calendar service is deployed.
    #[serde(default)]
    pub calendar_api_url: Option<String>,
    /// Base URL for the boss-subject-kinds HTTP API. Optional —
    /// when unset, Job creates accept any subject kind string. When
    /// set, the kind is validated against the registry and missing
    /// kinds are rejected with 400.
    #[serde(default)]
    pub subject_kinds_api_url: Option<String>,
    /// boss-classes HTTP API used at startup to seed the executive
    /// role cache (`metadata.is_executive = true`). Read by the
    /// escalation router so a `critical` Job on a platinum/gold
    /// account pages the tenant-defined executives. When absent,
    /// no roles are treated as executive.
    #[serde(default)]
    pub classes_api_url: Option<String>,
    /// Base URLs for the four upstream services the Subject
    /// existence checker needs. All four must be set for the
    /// checker to come up; missing any one leaves the checker
    /// disabled and the create-Job handler accepts any subject id.
    #[serde(default)]
    pub people_api_url: Option<String>,
    #[serde(default)]
    pub assets_api_url: Option<String>,
    #[serde(default)]
    pub locations_api_url: Option<String>,
    #[serde(default)]
    pub inventory_api_url: Option<String>,
}

impl JobsApiConfig {
    /// Load from a TOML file and validate non-empty fields.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for JobsApiConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.nats_url.is_empty() {
            return Err(ConfigError::Validation("nats_url must not be empty".into()));
        }
        if self.http_bind.is_empty() {
            return Err(ConfigError::Validation(
                "http_bind must not be empty".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_valid_toml() {
        let dir = std::env::temp_dir().join("boss-jobs-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("valid.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"nats_url = "nats://127.0.0.1:4222"
http_bind = "0.0.0.0:7900""#
        )
        .unwrap();

        let cfg = JobsApiConfig::load(&path).unwrap();
        assert_eq!(cfg.nats_url, "nats://127.0.0.1:4222");
        assert_eq!(cfg.http_bind, "0.0.0.0:7900");
    }

    #[test]
    fn rejects_empty_nats_url() {
        let dir = std::env::temp_dir().join("boss-jobs-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty_nats.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"nats_url = ""
http_bind = "0.0.0.0:7900""#
        )
        .unwrap();

        let err = JobsApiConfig::load(&path).unwrap_err();
        assert!(err.to_string().contains("nats_url"));
    }

    #[test]
    fn rejects_missing_file() {
        let path = std::path::PathBuf::from("/tmp/does-not-exist-jobs.toml");
        assert!(JobsApiConfig::load(&path).is_err());
    }
}
