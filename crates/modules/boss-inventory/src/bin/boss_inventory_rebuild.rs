//! `boss-inventory-rebuild` — drop the four inventory projections
//! (`vendors`, `purchase_orders` + lines, `vendor_invoices`,
//! `inventory_items`) and reconstruct them from `audit_log` alone.
//!
//! See `docs/design/projection-rebuilders.md`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_core::rebuild::resolve_database_url;
use boss_inventory::inventory_config::InventoryApiConfig;
use boss_inventory::rebuild_inventory;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-inventory-rebuild",
    about = "Rebuild the inventory projections from audit_log",
    version
)]
struct Cli {
    #[arg(long)]
    database_url: Option<String>,
    #[arg(short, long, default_value = "/etc/boss-inventory-api.toml")]
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
        .then(|| InventoryApiConfig::load(&cli.config).ok())
        .flatten()
        .map(|cfg| cfg.postgres_url);
    let db_url = resolve_database_url(
        cli.database_url,
        config_url,
        &["DATABASE_URL"],
        "pass --database-url, point --config at a valid \
         boss-inventory-api.toml, or set DATABASE_URL",
    )?;

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let report = rebuild_inventory(&pool)
        .await
        .with_context(|| "rebuilding inventory projections")?;

    info!(
        events_processed = report.events_processed,
        events_skipped = report.events_skipped,
        vendors_upserted = report.vendors_upserted,
        vendors_deleted = report.vendors_deleted,
        purchase_orders_upserted = report.purchase_orders_upserted,
        vendor_invoices_upserted = report.vendor_invoices_upserted,
        items_upserted = report.items_upserted,
        "inventory rebuild complete"
    );
    Ok(())
}
