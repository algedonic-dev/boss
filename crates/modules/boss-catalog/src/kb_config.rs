//! Configuration for the `boss-catalog-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct KbApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for domain event publishing. If omitted, events are not published.
    #[serde(default)]
    pub nats_url: Option<String>,
    /// Base URL for the boss-assets HTTP API. Required: the kb
    /// model delete path queries assets for active devices that
    /// reference the SKU before removing the model, and we fail
    /// fast at startup rather than quietly skipping the check.
    pub assets_api_url: String,
    /// Base URL for the boss-classes HTTP API. Every catalog write
    /// that carries a tenant-extensible taxonomy code validates it
    /// against the Class registry before commit: asset-model
    /// `category` (subject_kind='asset'), document `kind`
    /// (subject_kind='asset'), and marketing-asset `kind`
    /// (subject_kind='marketing-asset'). Required: registry
    /// validation is the only gate keeping a typo'd or unregistered
    /// code out of the tables, so the startup-time validate() rejects
    /// an empty value rather than silently no-op'ing the checks
    /// (fail-loud, matching boss-people).
    pub classes_api_url: String,
}

impl KbApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for KbApiConfig {
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
        if self.assets_api_url.is_empty() {
            return Err(ConfigError::Validation(
                "assets_api_url must not be empty".into(),
            ));
        }
        if self.classes_api_url.is_empty() {
            return Err(ConfigError::Validation(
                "classes_api_url must not be empty (Class registry \
                 validation is mandatory; category / document-kind / \
                 marketing-kind validation lives in the app layer)"
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
        let dir = std::env::temp_dir().join("boss-catalog-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("valid.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7700"
assets_api_url = "http://127.0.0.1:7600"
classes_api_url = "http://127.0.0.1:7800""#
        )
        .unwrap();

        let cfg = KbApiConfig::load(&path).unwrap();
        assert_eq!(cfg.postgres_url, "postgres://localhost/boss");
        assert_eq!(cfg.http_bind, "0.0.0.0:7700");
        assert_eq!(cfg.classes_api_url, "http://127.0.0.1:7800");
    }

    #[test]
    fn rejects_empty_postgres_url() {
        let dir = std::env::temp_dir().join("boss-catalog-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty_pg.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = ""
http_bind = "0.0.0.0:7700"
assets_api_url = "http://127.0.0.1:7600"
classes_api_url = "http://127.0.0.1:7800""#
        )
        .unwrap();

        let err = KbApiConfig::load(&path).unwrap_err();
        assert!(err.to_string().contains("postgres_url"));
    }

    #[test]
    fn rejects_missing_classes_api_url() {
        // Class registry validation is the only defense for catalog
        // taxonomy codes (asset category, document kind, marketing
        // kind) now that the schema CHECKs are gone. Loading a config
        // without classes_api_url must fail at startup, not later.
        let dir = std::env::temp_dir().join("boss-catalog-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("missing-classes.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7700"
assets_api_url = "http://127.0.0.1:7600""#
        )
        .unwrap();

        let err = KbApiConfig::load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("classes_api_url"),
            "expected classes_api_url error, got: {msg}"
        );
    }

    #[test]
    fn rejects_missing_file() {
        let path = std::path::PathBuf::from("/tmp/does-not-exist-kb.toml");
        assert!(KbApiConfig::load(&path).is_err());
    }
}
