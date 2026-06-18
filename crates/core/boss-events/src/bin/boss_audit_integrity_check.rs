//! `boss-audit-integrity-check` — daily scan over `audit_log`.
//!
//! Layer 1 of the immutable-audit-log story
//! (`docs/architecture-decisions.md` §Correctness protocol & the
//! audit log). Schema-level triggers
//! reject UPDATE / DELETE / TRUNCATE; this binary is the second-line
//! check that surfaces evidence of someone bypassing the trigger
//! (DROP TRIGGER, ALTER TABLE DISABLE TRIGGER, restore-from-backup).
//!
//! Wired up via a systemd timer (`infra/boss-audit-integrity-check.timer`),
//! cron, or any scheduler. Exit codes:
//!
//! - `0` — clean, no anomalies
//! - `2` — anomalies found (id gaps or created_at regressions)
//! - other — operational error (DB unreachable, bad config)
//!
//! `systemctl is-failed` flips on exit code 2, so the timer's
//! `OnFailure=` hook fires the alert.

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_core::rebuild::resolve_database_url;
use boss_events::check_audit_log_integrity;
use chrono::{DateTime, Utc};
use clap::Parser;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

/// The current chain head — what an auditor compares against on
/// their next handoff. `audit_log` is empty before the first event.
async fn current_chain_head(
    pool: &PgPool,
) -> Result<Option<(i64, Vec<u8>, Vec<u8>, DateTime<Utc>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, row_hash, prev_hash, created_at \
         FROM audit_log ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
}

#[derive(Parser, Debug)]
#[command(
    name = "boss-audit-integrity-check",
    about = "Scan audit_log for id gaps and created_at regressions",
    version
)]
struct Cli {
    /// Postgres connection string. Falls back to `BOSS_AUDIT_DATABASE_URL`
    /// then `DATABASE_URL`.
    #[arg(long)]
    database_url: Option<String>,

    /// Optional config file (TOML with a single `database_url` key).
    /// Provided so the timer unit can point at the same on-disk
    /// secrets file the API services use.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Print the full report as JSON on stdout regardless of result.
    /// The systemd journal-friendly default is one summary line.
    #[arg(long)]
    json: bool,
}

#[derive(serde::Deserialize)]
struct ConfigFile {
    database_url: String,
}

/// Read the `--config` file's `database_url`, when one was passed.
///
/// Unlike the projection-rebuild twins (which swallow a bad config and
/// fall through to env vars), an explicitly-supplied `--config` that is
/// unreadable or malformed is a hard error here — the operator asked for
/// that file specifically.
fn config_database_url(cli: &Cli) -> Result<Option<String>> {
    let Some(path) = &cli.config else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading config from {}", path.display()))?;
    let cfg: ConfigFile =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(cfg.database_url))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let cli = Cli::parse();
    let config_url = config_database_url(&cli)?;
    let db_url = resolve_database_url(
        cli.database_url,
        config_url,
        &["BOSS_AUDIT_DATABASE_URL", "DATABASE_URL"],
        "pass --database-url, --config <file>, \
         or set BOSS_AUDIT_DATABASE_URL / DATABASE_URL",
    )?;

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let report = check_audit_log_integrity(&pool)
        .await
        .with_context(|| "running integrity scan")?;

    // Layer 3: the daily checkpoint — log the current chain head so
    // each run leaves a journal entry an auditor can compare against
    // on a future DB snapshot. Cheap, no external service. Empty log
    // (fresh deployment) just logs an empty checkpoint.
    match current_chain_head(&pool).await? {
        Some((id, row_hash, prev_hash, created_at)) => info!(
            chain_head_id = id,
            chain_head_row_hash = %hex::encode(&row_hash),
            chain_head_prev_hash = %hex::encode(&prev_hash),
            chain_head_created_at = %created_at,
            total_rows = report.total_rows,
            checkpoint_at = %Utc::now(),
            "audit_log checkpoint"
        ),
        None => info!(checkpoint_at = %Utc::now(), "audit_log checkpoint (empty log)"),
    }

    if cli.json {
        let body = serde_json::json!({
            "total_rows": report.total_rows,
            "gap_count": report.gaps.len(),
            "regression_count": report.regressions.len(),
            "chain_break_count": report.chain_breaks.len(),
            "gaps": report.gaps.iter().map(|g| serde_json::json!({
                "prev_id": g.prev_id,
                "id": g.id,
                "missing_count": g.missing_count(),
            })).collect::<Vec<_>>(),
            "regressions": report.regressions.iter().map(|r| serde_json::json!({
                "prev_id": r.prev_id,
                "prev_created_at": r.prev_created_at,
                "id": r.id,
                "created_at": r.created_at,
            })).collect::<Vec<_>>(),
            "chain_breaks": report.chain_breaks.iter().map(|b| serde_json::json!({
                "id": b.id,
                "stored_hash": hex::encode(&b.stored_hash),
                "computed_hash": hex::encode(&b.computed_hash),
            })).collect::<Vec<_>>(),
            "dangling_ref_count": report.dangling_refs.len(),
            "dangling_refs": report.dangling_refs.iter().map(|r| serde_json::json!({
                "id": r.id,
                "kind": r.kind,
                "field": r.field,
                "foreign_id": r.foreign_id,
                "expected_parent_kind": r.expected_parent_kind,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
    }

    if report.is_clean() {
        info!(total_rows = report.total_rows, "audit_log integrity clean");
        return Ok(());
    }

    warn!(
        total_rows = report.total_rows,
        gaps = report.gaps.len(),
        regressions = report.regressions.len(),
        chain_breaks = report.chain_breaks.len(),
        dangling_refs = report.dangling_refs.len(),
        "audit_log integrity anomalies"
    );
    for gap in &report.gaps {
        error!(
            prev_id = gap.prev_id,
            id = gap.id,
            missing = gap.missing_count(),
            "id gap"
        );
    }
    for r in &report.regressions {
        error!(
            prev_id = r.prev_id,
            prev_created_at = %r.prev_created_at,
            id = r.id,
            created_at = %r.created_at,
            "created_at regression"
        );
    }
    for b in &report.chain_breaks {
        error!(
            id = b.id,
            stored = hex::encode(&b.stored_hash),
            computed = hex::encode(&b.computed_hash),
            "chain break"
        );
    }
    for d in &report.dangling_refs {
        error!(
            id = d.id,
            kind = %d.kind,
            field = %d.field,
            foreign_id = %d.foreign_id,
            expected_parent_kind = %d.expected_parent_kind,
            "dangling foreign ref"
        );
    }
    std::process::exit(2);
}
