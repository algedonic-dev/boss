//! `boss-messages-rebuild` — drop the `messages` projection and
//! reconstruct it from `audit_log` alone.
//!
//! See `docs/design/projection-rebuilders.md`. Pilot for the
//! "audit_log is canonical, projections are derived" arc.
//!
//! Exit codes:
//! - `0` — rebuild succeeded
//! - other — operational error

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_core::rebuild::resolve_database_url;
use boss_messages::messages_config::MessagesApiConfig;
use boss_messages::rebuild_messages;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-messages-rebuild",
    about = "Rebuild the messages projection from audit_log",
    version
)]
struct Cli {
    /// Postgres connection string. If omitted, falls back to the
    /// `database_url` in the messages-api config or DATABASE_URL.
    #[arg(long)]
    database_url: Option<String>,

    /// Path to a messages-api config file (default
    /// `/etc/boss-messages-api.toml`). Used only to discover the
    /// postgres URL when `--database-url` isn't passed.
    #[arg(short, long, default_value = "/etc/boss-messages-api.toml")]
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
    let config_url = (cli.config.exists())
        .then(|| MessagesApiConfig::load(&cli.config).ok())
        .flatten()
        .map(|cfg| cfg.postgres_url);
    let db_url = resolve_database_url(
        cli.database_url,
        config_url,
        &["DATABASE_URL"],
        "pass --database-url, point --config at a valid \
         boss-messages-api.toml, or set DATABASE_URL",
    )?;

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let report = rebuild_messages(&pool)
        .await
        .with_context(|| "rebuilding messages projection")?;

    info!(
        events_processed = report.events_processed,
        events_skipped = report.events_skipped,
        rows_inserted = report.rows_inserted,
        rows_marked_read = report.rows_marked_read,
        rows_archived = report.rows_archived,
        rows_deleted = report.rows_deleted,
        "messages rebuild complete"
    );
    Ok(())
}
