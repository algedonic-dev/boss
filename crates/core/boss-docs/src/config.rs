//! Configuration for the `boss-docs-api` binary.

use std::path::{Path, PathBuf};

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct DocsApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// Root of the git working directory. The service scans
    /// `{repo_root}/docs/design/*.md` for files. Defaults to the
    /// current working directory at launch time (`.`).
    #[serde(default = "default_repo_root")]
    pub repo_root: PathBuf,
}

fn default_repo_root() -> PathBuf {
    PathBuf::from(".")
}

impl DocsApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for DocsApiConfig {
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
