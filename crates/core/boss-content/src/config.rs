//! Configuration for `boss-content-api`.

use std::path::Path;

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
    /// boss-policy-api URL. Required when `BOSS_FILE_STORAGE_BUCKET`
    /// is set (i.e. file uploads are wired); optional otherwise.
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
    /// Object-storage bucket. One bucket per deployment per Q1.
    pub bucket: String,
    /// S3 endpoint URL. Defaults to AWS regional endpoint when unset.
    /// For GCS interop, set `https://storage.googleapis.com`.
    /// For MinIO, set `http://127.0.0.1:9000`.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// AWS-style region. Defaults to `us-east-1` when unset (GCS +
    /// MinIO accept this as a placeholder).
    #[serde(default)]
    pub region: Option<String>,
    /// Explicit access key. Optional — when both this and secret_key
    /// are set, they're passed directly to S3Storage::with_credentials
    /// (the GCS-interop path). When unset, the AWS SDK reads from
    /// environment / instance metadata.
    #[serde(default)]
    pub access_key: Option<String>,
    #[serde(default)]
    pub secret_key: Option<String>,
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
