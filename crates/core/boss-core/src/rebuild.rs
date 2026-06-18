//! Shared scaffold for the `boss-*-rebuild` (and seed) binaries.
//!
//! Every projection-rebuild binary carries the same `--database-url`
//! resolution ladder: an explicit CLI flag wins; failing that, a
//! postgres URL discovered from the service-API config file; failing
//! that, an environment variable; failing all three, a `bail!` with a
//! remediation hint. The *config struct* and its *field shape* differ
//! per crate (a `String` here, an `Option<String>` there); the
//! surrounding ladder does not. This module owns the ladder so each
//! binary keeps only the one genuinely per-crate line — pulling the URL
//! out of its own typed config — plus its rebuild call and report tail.
//!
//! Deliberately pure: no `sqlx`, no `clap`, no `tracing-subscriber`.
//! `boss-core` is the foundational domain crate (43 crates depend on
//! it) and the database/CLI/subscriber adapters stay at the edges. The
//! caller threads the config-derived URL in as an `Option<String>`, so
//! this helper never learns any concrete config type and the dependency
//! arrow keeps pointing inward.
//!
//! ```no_run
//! use boss_core::rebuild::resolve_database_url;
//!
//! # struct Cli { database_url: Option<String> }
//! # let cli = Cli { database_url: None };
//! // Caller does the one per-crate bit: pull the URL from its config.
//! let from_config: Option<String> = None; // e.g. cfg.postgres_url
//! let url = resolve_database_url(
//!     cli.database_url,
//!     from_config,
//!     &["DATABASE_URL"],
//!     "pass --database-url, point --config at a valid boss-foo-api.toml, \
//!      or set DATABASE_URL",
//! )?;
//! # Ok::<(), anyhow::Error>(())
//! ```

/// Resolve the database URL for a rebuild/seed binary.
///
/// Resolution order, first hit wins:
/// 1. `cli_url` — the explicit `--database-url` flag.
/// 2. `config_url` — a URL the caller pulled from its service config
///    (already `None` when the file is absent, fails to parse, or
///    carries an empty string; the caller owns that extraction since
///    only it knows the config type).
/// 3. The first environment variable in `env_keys` that is set and
///    non-empty, tried in order. Pass `&["DATABASE_URL"]` for the common
///    case, or e.g. `&["BOSS_AUDIT_DATABASE_URL", "DATABASE_URL"]` to
///    prefer a scoped override.
///
/// When none produce a value, returns an error reading
/// `"no database url: {hint}"`. `hint` is the binary's own remediation
/// guidance (which flags and config file to reach for), so each binary
/// keeps its exact wording.
pub fn resolve_database_url(
    cli_url: Option<String>,
    config_url: Option<String>,
    env_keys: &[&str],
    hint: &str,
) -> anyhow::Result<String> {
    if let Some(url) = cli_url {
        return Ok(url);
    }
    if let Some(url) = config_url.filter(|u| !u.is_empty()) {
        return Ok(url);
    }
    for key in env_keys {
        if let Ok(url) = std::env::var(key)
            && !url.is_empty()
        {
            return Ok(url);
        }
    }
    anyhow::bail!("no database url: {hint}")
}

/// Stable, collision-free advisory-lock key for a projection rebuild.
///
/// Every projection rebuilder takes a `pg_advisory_xact_lock` so two
/// rebuilds of the same projection never interleave (and the ledger's
/// replay-verifier serializes against the ledger rebuild the same way).
/// The key is derived from the projection name rather than hand-numbered:
/// distinct names get distinct keys (no accidental collisions that
/// needlessly serialize unrelated rebuilders), the same name yields the
/// same key in every process (so concurrent rebuilds still serialize),
/// and nobody has to pick the next free integer by hand.
///
/// FNV-1a (64-bit) over the name, shifted into the non-negative `i64`
/// range — Postgres advisory keys are signed 64-bit, and staying
/// positive keeps them legible in `pg_locks`. `const fn` so callers keep
/// the `const REBUILD_LOCK_KEY: i64 = lock_key("…")` form.
pub const fn lock_key(projection: &str) -> i64 {
    // FNV-1a: the standard 64-bit offset basis and prime.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let bytes = projection.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    (hash >> 1) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const HINT: &str = "pass --database-url or set DATABASE_URL";

    #[test]
    fn cli_url_wins_over_everything() {
        let url = resolve_database_url(
            Some("postgres://from-cli".into()),
            Some("postgres://from-config".into()),
            &["PATH"], // set in the env, but should never be consulted
            HINT,
        )
        .unwrap();
        assert_eq!(url, "postgres://from-cli");
    }

    #[test]
    fn config_url_used_when_no_cli() {
        let url =
            resolve_database_url(None, Some("postgres://from-config".into()), &["PATH"], HINT)
                .unwrap();
        assert_eq!(url, "postgres://from-config");
    }

    #[test]
    fn empty_config_url_is_skipped() {
        // An empty string from a present-but-blank config field must not
        // satisfy the ladder; it falls through to the env keys.
        let key = "BOSS_REBUILD_TEST_EMPTY_CONFIG";
        // SAFETY: single-threaded test, no other reader of this key.
        unsafe { std::env::set_var(key, "postgres://from-env") };
        let url = resolve_database_url(None, Some(String::new()), &[key], HINT).unwrap();
        unsafe { std::env::remove_var(key) };
        assert_eq!(url, "postgres://from-env");
    }

    #[test]
    fn env_keys_tried_in_order() {
        let first = "BOSS_REBUILD_TEST_FIRST";
        let second = "BOSS_REBUILD_TEST_SECOND";
        // SAFETY: single-threaded test, keys unique to this case.
        unsafe {
            std::env::remove_var(first);
            std::env::set_var(second, "postgres://second");
        }
        // `first` unset → falls through to `second`.
        let url = resolve_database_url(None, None, &[first, second], HINT).unwrap();
        unsafe { std::env::remove_var(second) };
        assert_eq!(url, "postgres://second");
    }

    #[test]
    fn bail_carries_the_hint_when_nothing_resolves() {
        let err =
            resolve_database_url(None, None, &["BOSS_REBUILD_TEST_UNSET_XYZ"], HINT).unwrap_err();
        let msg = err.to_string();
        assert!(msg.starts_with("no database url:"), "got: {msg}");
        assert!(msg.contains(HINT), "got: {msg}");
    }

    #[test]
    fn lock_key_is_deterministic_and_distinct_per_projection() {
        // Same name → same key, so concurrent rebuilds of one projection
        // still serialize on the advisory lock.
        assert_eq!(lock_key("people"), lock_key("people"));
        // Distinct names → distinct keys. These three pairs were the
        // historical hand-numbered collisions (unrelated projections that
        // happened to share a magic number).
        assert_ne!(lock_key("messages"), lock_key("ledger-facts"));
        assert_ne!(lock_key("accounts"), lock_key("products"));
        assert_ne!(lock_key("content"), lock_key("scheduling"));
    }

    #[test]
    fn lock_key_stays_non_negative() {
        for name in ["people", "ledger", "calendar", "jobs", "scheduling", ""] {
            assert!(lock_key(name) >= 0, "{name:?} produced a negative key");
        }
    }
}
