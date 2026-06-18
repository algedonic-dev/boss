//! `boss-commerce-rebuild` — drop the invoices + invoice_line_items
//! projections and reconstruct from `audit_log`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_commerce::commerce_config::CommerceApiConfig;
use boss_commerce::rebuild_commerce;
use boss_core::rebuild::resolve_database_url;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-commerce-rebuild",
    about = "Rebuild commerce projections from audit_log",
    version
)]
struct Cli {
    #[arg(long)]
    database_url: Option<String>,
    #[arg(short, long, default_value = "/etc/boss-commerce-api.toml")]
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
        .then(|| CommerceApiConfig::load(&cli.config).ok())
        .flatten()
        .map(|cfg| cfg.postgres_url);
    let db_url = resolve_database_url(
        cli.database_url,
        config_url,
        &["DATABASE_URL"],
        "pass --database-url, point --config at a valid boss-commerce-api.toml, or set DATABASE_URL",
    )?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .with_context(|| "connecting to Postgres")?;
    let report = rebuild_commerce(&pool)
        .await
        .with_context(|| "rebuilding commerce projections")?;
    info!(
        events_processed = report.events_processed,
        events_skipped = report.events_skipped,
        invoices_upserted = report.invoices_upserted,
        "commerce rebuild complete"
    );
    Ok(())
}
