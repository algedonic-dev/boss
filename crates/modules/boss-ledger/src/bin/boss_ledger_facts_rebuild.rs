//! `boss-ledger-facts-rebuild` — project `audit_log` → `financial_facts`
//! via the `gl_fact_projection_rules` registry.
//!
//! Runs `boss_ledger::rebuild_facts` against the configured database.
//! UPSERT semantics, idempotent on the `(kind, source_table, source_id)`
//! natural key. To start from a clean slate, operators should
//! TRUNCATE `financial_facts` (and the dependent `gl_journal_entries`
//! / `gl_journal_lines`) externally first; the rebuilder itself does
//! not delete.

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_ledger::config::LedgerApiConfig;
use boss_ledger::rebuild_facts;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-ledger-facts-rebuild",
    about = "Project audit_log → financial_facts via gl_fact_projection_rules",
    version
)]
struct Cli {
    /// Config path. Shares `boss-ledger-api.toml` since the DB URL is
    /// the same — the rebuilder just reads the same pool.
    #[arg(short, long, default_value = "/etc/boss-ledger-api.toml")]
    config: PathBuf,
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
    let cfg = LedgerApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    info!("rebuilding financial_facts from audit_log");

    let report = rebuild_facts(&pool)
        .await
        .with_context(|| "rebuild_facts failed")?;

    info!(
        rules_loaded = report.rules_loaded,
        events_scanned = report.events_scanned,
        facts_written = report.facts_written,
        events_skipped_missing_field = report.events_skipped_missing_field,
        supersedes_applied = report.supersedes_applied,
        "rebuild_facts complete"
    );

    if report.events_skipped_missing_field > 0 {
        // Skipped events are usually a sign of upstream payload drift
        // (a field name changed without a matching rule update). Not
        // a hard failure, but worth surfacing for the operator.
        tracing::warn!(
            count = report.events_skipped_missing_field,
            "some audit_log events skipped — payload missing a projection-required field"
        );
    }

    Ok(())
}
