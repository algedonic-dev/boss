//! Configuration for the `boss-people-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PeopleApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for domain event publishing. If omitted, events are not published.
    #[serde(default)]
    pub nats_url: Option<String>,
    /// Base URL for the boss-assets HTTP API. Required: the account delete
    /// path queries assets for open tickets before removing a account, and
    /// we fail fast at startup rather than quietly skipping the check.
    pub assets_api_url: String,
    /// Base URL for the boss-classes HTTP API. Every employee write
    /// validates `role` against the Class registry before commit.
    /// Required: registry validation is the only gate keeping a
    /// typo'd or unregistered role code out of the table, so the
    /// startup-time validate() rejects an empty value rather than
    /// silently no-op'ing the check.
    pub classes_api_url: String,
    /// Base URL for the boss-locations HTTP API. Every employee
    /// write validates `location` against the Locations registry
    /// before commit. Required for the same reason as
    /// `classes_api_url`.
    pub locations_api_url: String,
    /// Base URL for the boss-calendar HTTP API. Optional —
    /// boss-people-api runs without it, but the PTO endpoint
    /// (`POST /api/people/pto`) returns 503 when calendar isn't
    /// configured. Set this once the calendar service is deployed.
    #[serde(default)]
    pub calendar_api_url: Option<String>,
    /// Base URL for the boss-subject-kinds HTTP API. Optional —
    /// SubjectKind validation is opt-in (see
    /// `http.rs::check_custom_subject`). When set, write paths that
    /// accept Subject::Custom validate the custom_kind against the
    /// registry.
    #[serde(default)]
    pub subject_kinds_api_url: Option<String>,
}

impl PeopleApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for PeopleApiConfig {
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
                 validation is mandatory; role validation lives in \
                 the app layer)"
                    .into(),
            ));
        }
        if self.locations_api_url.is_empty() {
            return Err(ConfigError::Validation(
                "locations_api_url must not be empty (Locations \
                 registry validation is mandatory; location \
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
        let dir = std::env::temp_dir().join("boss-people-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("valid.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7500"
assets_api_url = "http://127.0.0.1:7600"
classes_api_url = "http://127.0.0.1:7800"
locations_api_url = "http://127.0.0.1:7820""#
        )
        .unwrap();

        let cfg = PeopleApiConfig::load(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7500");
        assert_eq!(cfg.classes_api_url, "http://127.0.0.1:7800");
    }

    #[test]
    fn rejects_missing_classes_api_url() {
        // Class registry validation is the only defense for role
        // values now that the schema CHECK is gone. Loading a config
        // without classes_api_url must fail at startup, not later.
        let dir = std::env::temp_dir().join("boss-people-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("missing-classes.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7500"
assets_api_url = "http://127.0.0.1:7600"
locations_api_url = "http://127.0.0.1:7820""#
        )
        .unwrap();

        let err = PeopleApiConfig::load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("classes_api_url"),
            "expected classes_api_url error, got: {msg}"
        );
    }

    #[test]
    fn rejects_missing_locations_api_url() {
        // Locations registry validation is the only defense for
        // employees.location now that the schema CHECK is gone.
        let dir = std::env::temp_dir().join("boss-people-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("missing-locations.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7500"
assets_api_url = "http://127.0.0.1:7600"
classes_api_url = "http://127.0.0.1:7800""#
        )
        .unwrap();

        let err = PeopleApiConfig::load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("locations_api_url"),
            "expected locations_api_url error, got: {msg}"
        );
    }

    #[test]
    fn rejects_missing_file() {
        let path = std::path::PathBuf::from("/tmp/does-not-exist-people.toml");
        assert!(PeopleApiConfig::load(&path).is_err());
    }
}
