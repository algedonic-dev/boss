//! Configuration for the `boss-events-api` binary.
//!
//! Same shape as every other tier-1 service config — Postgres URL +
//! HTTP bind. The audit_log router has no upstream service deps; it
//! reads `audit_log` from Postgres and writes nothing.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct EventsApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
}

impl EventsApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for EventsApiConfig {
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
