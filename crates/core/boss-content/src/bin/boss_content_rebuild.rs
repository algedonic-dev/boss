//! `boss-content-rebuild` — reconstruct content from audit_log, refusing
//! uncovered legacy manual rows before any content projection is cleared.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use boss_content::config::ContentApiConfig;
use boss_content::files::rebuild_file_refs;
use boss_content::rebuild_content;
use boss_core::rebuild::resolve_database_url;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-content-rebuild",
    about = "Rebuild content projections from audit_log",
    version
)]
struct Cli {
    #[arg(long)]
    database_url: Option<String>,
    #[arg(short, long, default_value = "/etc/boss-content-api.toml")]
    config: PathBuf,
    /// Seconds to wait, unlocked, for the event relay to drain before
    /// the rebuild's locked log check decides (design b046f510).
    #[arg(long, default_value_t = boss_events::outbox::DEFAULT_DRAIN_TIMEOUT_SECS)]
    drain_timeout: u64,
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
    let config_url = (cli.config.exists())
        .then(|| ContentApiConfig::load(&cli.config).ok())
        .flatten()
        .map(|cfg| cfg.postgres_url);
    let db_url = resolve_database_url(
        cli.database_url,
        config_url,
        &["DATABASE_URL"],
        "pass --database-url, point --config at a valid boss-content-api.toml, or set DATABASE_URL",
    )?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .with_context(|| "connecting to Postgres")?;
    boss_events::outbox::wait_for_drain(&pool, Duration::from_secs(cli.drain_timeout))
        .await
        .map_err(anyhow::Error::msg)?;
    let report = rebuild_content(&pool)
        .await
        .with_context(|| "rebuilding content projections")?;
    info!(
        events_processed = report.events_processed,
        events_skipped = report.events_skipped,
        bulletins_upserted = report.bulletins_upserted,
        bulletins_deleted = report.bulletins_deleted,
        dismissals_inserted = report.dismissals_inserted,
        manual_versions_replayed = report.manual_versions_replayed,
        "content rebuild complete"
    );

    // Files projection — see crates/boss-content/src/files/rebuild.rs.
    // Distinct advisory-lock key so this can run alongside the
    // bulletins rebuild without contention.
    let files_report = rebuild_file_refs(&pool)
        .await
        .with_context(|| "rebuilding file_refs projection")?;
    info!(
        events_processed = files_report.events_processed,
        events_skipped = files_report.events_skipped,
        refs_inserted = files_report.refs_inserted,
        refs_soft_deleted = files_report.refs_soft_deleted,
        "file_refs rebuild complete"
    );
    Ok(())
}
