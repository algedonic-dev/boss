//! Configuration for the `boss-messages-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct MessagesApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for domain event publishing. If omitted, events are not published.
    #[serde(default)]
    pub nats_url: Option<String>,
    /// How long to keep rows in `messages_events` before the daily
    /// purge sweep deletes them. `None` disables the sweep.
    /// The canonical source-of-truth value is `[messages]
    /// events_retention_days` in `tenant.toml`; this field is the
    /// per-deployment knob the purge binary reads at runtime.
    #[serde(default)]
    pub events_retention_days: Option<i64>,
    /// Base URL for the boss-classes HTTP API. A `send` carrying an
    /// explicit `kind` validates it against the Class registry under
    /// `(subject_kind='message')`. Required: `MessageKind` is a
    /// free-text wrapper, so registry validation is the only gate
    /// keeping a typo'd or unregistered kind out of the table. The
    /// startup-time validate() rejects an empty value rather than
    /// silently no-op'ing the check.
    pub classes_api_url: String,
}

impl MessagesApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for MessagesApiConfig {
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
                "classes_api_url must not be empty (Class registry \
                 validation is mandatory; message-kind validation lives \
                 in the app layer)"
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
        let dir = std::env::temp_dir().join("boss-messages-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("valid.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7200"
classes_api_url = "http://127.0.0.1:7800""#
        )
        .unwrap();

        let cfg = MessagesApiConfig::load(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7200");
        assert_eq!(cfg.classes_api_url, "http://127.0.0.1:7800");
    }

    #[test]
    fn rejects_missing_classes_api_url() {
        // Class registry validation is the only defense for message
        // kind now that the schema CHECK is gone. Loading a config
        // without classes_api_url must fail at startup, not later.
        let dir = std::env::temp_dir().join("boss-messages-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("missing-classes.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7200""#
        )
        .unwrap();

        let err = MessagesApiConfig::load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("classes_api_url"),
            "expected classes_api_url error, got: {msg}"
        );
    }

    #[test]
    fn rejects_missing_file() {
        let path = std::path::PathBuf::from("/tmp/does-not-exist-messages.toml");
        assert!(MessagesApiConfig::load(&path).is_err());
    }
}
