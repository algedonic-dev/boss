//! Shared TOML config loader for service-API binaries.
//!
//! Every `boss-*-api` crate carries a `FooConfig` struct with the
//! same load + error boilerplate: read a TOML file, parse it, run a
//! per-crate `validate`, wrap the three failure modes (read / parse /
//! validation). The *struct* and its *validation rules* differ per
//! crate; the surrounding plumbing does not. This module owns the
//! plumbing so the service crates only declare their fields and their
//! checks.
//!
//! ```no_run
//! use std::path::Path;
//! use serde::Deserialize;
//! use boss_core::config::{ConfigError, Validate, load_toml};
//!
//! #[derive(Deserialize)]
//! struct FooConfig {
//!     postgres_url: String,
//!     http_bind: String,
//! }
//!
//! impl Validate for FooConfig {
//!     fn validate(&self) -> Result<(), ConfigError> {
//!         if self.postgres_url.is_empty() {
//!             return Err(ConfigError::Validation(
//!                 "postgres_url must not be empty".into(),
//!             ));
//!         }
//!         if self.http_bind.is_empty() {
//!             return Err(ConfigError::Validation(
//!                 "http_bind must not be empty".into(),
//!             ));
//!         }
//!         Ok(())
//!     }
//! }
//!
//! let cfg: FooConfig = load_toml(Path::new("/etc/boss/foo.toml"))?;
//! # Ok::<(), ConfigError>(())
//! ```

use std::path::Path;

/// The three ways loading a service config can fail. `Io` and `Parse`
/// carry the offending path (rendered via [`Path::display`]) plus the
/// underlying error rendered to a `String` — we keep the error as text
/// rather than the source type so the variant stays `Clone` and free of
/// a `toml`/`io` type in its public surface.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading config file {0}: {1}")]
    Io(String, String),
    #[error("parsing config file {0}: {1}")]
    Parse(String, String),
    #[error("config validation: {0}")]
    Validation(String),
}

/// Per-crate validation hook. The body is whatever non-empty / cross-field
/// checks the crate's `FooConfig` needs; it runs after a successful parse.
/// Signature matches the bespoke `fn validate(&self) -> Result<(), _>`
/// every service crate already had.
pub trait Validate {
    fn validate(&self) -> Result<(), ConfigError>;
}

/// Read `path`, parse it as TOML into `T`, then run `T::validate`.
/// The single source of truth for the read → parse → validate flow
/// every service config module shares.
pub fn load_toml<T>(path: &Path) -> Result<T, ConfigError>
where
    T: serde::de::DeserializeOwned + Validate,
{
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::Io(path.display().to_string(), e.to_string()))?;
    let cfg: T = toml::from_str(&text)
        .map_err(|e| ConfigError::Parse(path.display().to_string(), e.to_string()))?;
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Sample {
        postgres_url: String,
        http_bind: String,
    }

    impl Validate for Sample {
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

    fn write_tmp(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("boss-core-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn loads_and_validates() {
        let path = write_tmp(
            "valid.toml",
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7000""#,
        );
        let cfg: Sample = load_toml(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7000");
    }

    #[test]
    fn missing_file_is_io_error() {
        let path = std::path::PathBuf::from("/tmp/boss-core-does-not-exist.toml");
        let err = load_toml::<Sample>(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Io(..)));
    }

    #[test]
    fn bad_toml_is_parse_error() {
        let path = write_tmp("bad.toml", "this is not = valid = toml");
        let err = load_toml::<Sample>(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(..)));
    }

    #[test]
    fn validation_failure_propagates() {
        let path = write_tmp(
            "empty_pg.toml",
            r#"postgres_url = ""
http_bind = "0.0.0.0:7000""#,
        );
        let err = load_toml::<Sample>(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
        assert!(err.to_string().contains("postgres_url"));
    }
}
