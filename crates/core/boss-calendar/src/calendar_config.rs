//! Configuration for the `boss-calendar-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

/// Re-exported under the crate's historical name. The shared loader's
/// [`ConfigError`] now backs every service config; this alias keeps
/// `boss_calendar::CalendarConfigError` resolving for downstream users.
pub type CalendarConfigError = ConfigError;

#[derive(Debug, Clone, Deserialize)]
pub struct CalendarApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for domain event publishing. If omitted, events
    /// are not published. Production binaries always set this so
    /// reservation events land in `audit_log`.
    #[serde(default)]
    pub nats_url: Option<String>,
}

impl CalendarApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for CalendarApiConfig {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_valid_toml() {
        let dir = std::env::temp_dir().join("boss-calendar-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("valid.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7860""#
        )
        .unwrap();

        let cfg = CalendarApiConfig::load(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7860");
    }

    #[test]
    fn rejects_missing_file() {
        let path = std::path::PathBuf::from("/tmp/does-not-exist-calendar.toml");
        assert!(CalendarApiConfig::load(&path).is_err());
    }

    #[test]
    fn rejects_empty_postgres_url() {
        let dir = std::env::temp_dir().join("boss-calendar-config-test-empty");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.toml");
        std::fs::write(
            &path,
            r#"postgres_url = ""
http_bind = "0.0.0.0:7860""#,
        )
        .unwrap();
        assert!(CalendarApiConfig::load(&path).is_err());
    }
}
