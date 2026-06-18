//! `boss-files-gc` — bytes garbage-collection sweep for file
//! references.
//!
//! A file_ref soft-deleted
//! past the 30-day grace window is eligible for byte deletion only
//! when no other live ref shares its sha256. Refcount-at-GC strategy
//! lets operators recover within the window from a mistaken Detach.
//!
//! Invoked by a systemd timer (see `infra/boss-files-gc.timer`).
//! Reads the file-storage root from the same
//! `/etc/boss-content-api.toml` that the API binary consumes — no
//! split-config to keep in sync.
//!
//! Exit codes:
//! - `0` — sweep ran (including "0 candidates" runs)
//! - non-zero — operational error (config missing, DB unreachable,
//!   storage root unreadable, …)

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_content::config::ContentApiConfig;
use boss_content::files::{
    DEFAULT_GC_GRACE_DAYS, FileStorage, LocalDiskStorage, gc_orphan_objects,
};
use chrono::Utc;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-files-gc",
    about = "Nightly bytes-GC sweep for file_refs",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-content-api.toml")]
    config: PathBuf,

    /// Override the grace window. Defaults to
    /// `boss_content::files::DEFAULT_GC_GRACE_DAYS` (30) — useful for
    /// tests + admin one-shot runs that want immediate eviction.
    #[arg(long)]
    grace_days: Option<i64>,
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
    let cfg = ContentApiConfig::load(&cli.config)
        .with_context(|| format!("loading config {}", cli.config.display()))?;

    let Some(files_cfg) = cfg.files else {
        warn!("no [files] block in config; nothing to GC");
        return Ok(());
    };

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&cfg.postgres_url)
        .await
        .context("connecting to postgres")?;

    let storage: Arc<dyn FileStorage> = Arc::new(
        LocalDiskStorage::new(&files_cfg.root)
            .await
            .with_context(|| format!("opening file storage root {}", files_cfg.root.display()))?,
    );

    let grace_days = cli.grace_days.unwrap_or(DEFAULT_GC_GRACE_DAYS);
    let grace = chrono::Duration::days(grace_days);
    let now = Utc::now();
    let report = gc_orphan_objects(&pool, storage, now, grace)
        .await
        .context("running gc sweep")?;

    info!(
        grace_days,
        examined = report.examined,
        kept_dedup = report.kept_dedup,
        bytes_deleted = report.bytes_deleted,
        delete_failures = report.delete_failures,
        "files-gc sweep complete"
    );
    Ok(())
}
