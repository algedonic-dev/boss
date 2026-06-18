//! Configuration for `boss-content-api`.

use std::path::{Path, PathBuf};

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ContentApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for domain event publishing. Optional — production
    /// always sets it so bulletin events land in `audit_log`.
    #[serde(default)]
    pub nats_url: Option<String>,
    /// boss-policy-api URL. Required when a `[files]` block is set
    /// (i.e. file uploads are wired); optional otherwise.
    #[serde(default)]
    pub policy_api_url: Option<String>,
    /// File storage configuration. Optional — when absent, the file
    /// references endpoints aren't mounted (the rest of boss-content
    /// stays functional).
    #[serde(default)]
    pub files: Option<FilesConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilesConfig {
    /// Root directory for file-attachment bytes. One tree per
    /// deployment; created on startup if absent. Objects land at
    /// `<root>/sha256/<hex>`. Back this with durable storage (and
    /// include it in backups) — it holds every uploaded attachment.
    pub root: PathBuf,
}

impl ContentApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for ContentApiConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.postgres_url.is_empty() {
            return Err(ConfigError::Validation(
                "postgres_url must not be empty".into(),
            ));
        }
        if self.http_bind.is_empty() {
            return Err(ConfigError::Validation(
                "http_bind must not be empty".into(),
            ));
        }
        Ok(())
    }
}
