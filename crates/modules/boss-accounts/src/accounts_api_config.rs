//! Configuration for the `boss-accounts-api` binary.
//!
//! Same shape as `boss-people-api`'s config since accounts shares
//! its cross-service dep tree (classes, assets, NATS). Pre-2026-06
//! these routes were mounted into people-api; this is the split-out
//! standalone service.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AccountsApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for domain event publishing. Optional — when
    /// omitted, accounts writes to the projection only and skips
    /// audit_log. Production wires this.
    #[serde(default)]
    pub nats_url: Option<String>,
    /// Class registry HTTP URL for `account_team_role` validation.
    /// When the team-router gets a write, the role code must resolve
    /// to an active Class in the registry.
    pub classes_api_url: String,
    /// Assets HTTP URL for open-ticket-count lookups on the
    /// account detail page.
    pub assets_api_url: String,
}

impl AccountsApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for AccountsApiConfig {
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
        if self.classes_api_url.is_empty() {
            return Err(ConfigError::Validation(
                "classes_api_url must not be empty".into(),
            ));
        }
        if self.assets_api_url.is_empty() {
            return Err(ConfigError::Validation(
                "assets_api_url must not be empty".into(),
            ));
        }
        Ok(())
    }
}
