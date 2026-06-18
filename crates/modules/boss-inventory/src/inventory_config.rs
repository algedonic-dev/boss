//! Configuration for the `boss-inventory-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct InventoryApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for domain event publishing. If omitted, events are not published.
    #[serde(default)]
    pub nats_url: Option<String>,
    /// Cross-service endpoints the warehouse-status projection fans out
    /// to. All three must be configured for the `/warehouse-status`
    /// route to respond; otherwise it returns 503.
    pub jobs_api_url: String,
    pub assets_api_url: String,
    pub shipping_api_url: String,
    /// Base URL for the boss-classes HTTP API. Every vendor-invoice
    /// write validates a *present* `discrepancy_kind` against the Class
    /// registry before commit. Required: registry validation is the
    /// only gate keeping a typo'd or unregistered discrepancy code out
    /// of the table, so the startup-time validate() rejects an empty
    /// value rather than silently no-op'ing the check. (A clean
    /// three-way match with no discrepancy_kind skips the lookup
    /// entirely — the gate is identity-first.)
    pub classes_api_url: String,
}

impl InventoryApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for InventoryApiConfig {
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
        for (name, val) in [
            ("jobs_api_url", &self.jobs_api_url),
            ("assets_api_url", &self.assets_api_url),
            ("shipping_api_url", &self.shipping_api_url),
        ] {
            if val.is_empty() {
                return Err(ConfigError::Validation(format!("{name} must not be empty")));
            }
        }
        if self.classes_api_url.is_empty() {
            return Err(ConfigError::Validation(
                "classes_api_url must not be empty (Class registry \
                 validation is mandatory; vendor_invoices.discrepancy_kind \
                 validation lives in the app layer)"
                    .into(),
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
        let dir = std::env::temp_dir().join("boss-inventory-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("valid.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7300"
jobs_api_url = "http://127.0.0.1:7900"
assets_api_url = "http://127.0.0.1:7600"
shipping_api_url = "http://127.0.0.1:7100"
classes_api_url = "http://127.0.0.1:7800""#
        )
        .unwrap();

        let cfg = InventoryApiConfig::load(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7300");
        assert_eq!(cfg.classes_api_url, "http://127.0.0.1:7800");
    }

    #[test]
    fn rejects_missing_classes_api_url() {
        // Class registry validation is the only defense for
        // vendor_invoices.discrepancy_kind now that the schema CHECK is
        // gone. Loading a config without classes_api_url must fail at
        // startup, not later.
        let dir = std::env::temp_dir().join("boss-inventory-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("missing-classes.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7300"
jobs_api_url = "http://127.0.0.1:7900"
assets_api_url = "http://127.0.0.1:7600"
shipping_api_url = "http://127.0.0.1:7100""#
        )
        .unwrap();

        let err = InventoryApiConfig::load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("classes_api_url"),
            "expected classes_api_url error, got: {msg}"
        );
    }

    #[test]
    fn rejects_missing_file() {
        let path = std::path::PathBuf::from("/tmp/does-not-exist-inventory.toml");
        assert!(InventoryApiConfig::load(&path).is_err());
    }
}
