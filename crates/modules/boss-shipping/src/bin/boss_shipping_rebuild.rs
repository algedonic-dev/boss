//! `boss-shipping-rebuild` — drop the `shipments` +
//! `shipment_systems` projections and reconstruct them from
//! `audit_log` alone.
//!
//! See `docs/design/projection-rebuilders.md`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_core::rebuild::resolve_database_url;
use boss_shipping::rebuild_shipping;
use boss_shipping::shipping_config::ShippingApiConfig;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-shipping-rebuild",
    about = "Rebuild the shipping projections from audit_log",
    version
)]
struct Cli {
    #[arg(long)]
    database_url: Option<String>,
    #[arg(short, long, default_value = "/etc/boss-shipping-api.toml")]
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
        .then(|| ShippingApiConfig::load(&cli.config).ok())
        .flatten()
        .map(|cfg| cfg.postgres_url);
    let db_url = resolve_database_url(
        cli.database_url,
        config_url,
        &["DATABASE_URL"],
        "pass --database-url, point --config at a valid \
         boss-shipping-api.toml, or set DATABASE_URL",
    )?;

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let report = rebuild_shipping(&pool)
        .await
        .with_context(|| "rebuilding shipping projections")?;

    info!(
        events_processed = report.events_processed,
        events_skipped = report.events_skipped,
        shipments_upserted = report.shipments_upserted,
        shipments_deleted = report.shipments_deleted,
        "shipping rebuild complete"
    );
    Ok(())
}
