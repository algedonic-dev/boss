//! `boss-calendar-rebuild` — drop the calendar_reservations
//! projection and reconstruct from `audit_log`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use boss_calendar::CalendarApiConfig;
use boss_calendar::rebuild_calendar;
use boss_core::rebuild::resolve_database_url;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-calendar-rebuild",
    about = "Rebuild calendar projection from audit_log",
    version
)]
struct Cli {
    #[arg(long)]
    database_url: Option<String>,
    #[arg(short, long, default_value = "/etc/boss-calendar-api.toml")]
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
        .then(|| CalendarApiConfig::load(&cli.config).ok())
        .flatten()
        .map(|cfg| cfg.postgres_url);
    let db_url = resolve_database_url(
        cli.database_url,
        config_url,
        &["DATABASE_URL"],
        "pass --database-url, point --config at a valid boss-calendar-api.toml, or set DATABASE_URL",
    )?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .with_context(|| "connecting to Postgres")?;
    boss_events::outbox::wait_for_drain(&pool, Duration::from_secs(cli.drain_timeout))
        .await
        .map_err(anyhow::Error::msg)?;
    let report = rebuild_calendar(&pool)
        .await
        .with_context(|| "rebuilding calendar projection")?;
    info!(
        events_processed = report.events_processed,
        events_skipped = report.events_skipped,
        reservations_inserted = report.reservations_inserted,
        reservations_cancelled = report.reservations_cancelled,
        "calendar rebuild complete"
    );
    Ok(())
}
