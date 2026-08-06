//! `boss-views-catchup` — keep `event_facts` level with the log.
//!
//! `event_facts` is a projection of `audit_log`, and until this ran on
//! a timer it was refreshed only by a full `boss-rebuild-all`. On a
//! live box that meant it fell behind continuously: measured at 53,495
//! events behind mid-session, climbing for as long as the sim ran. The
//! two surfaces built on it — Views and the per-Subject event history
//! — were therefore correct only in the minutes after a manual
//! rebuild, which is not a property a read surface can be built on.
//!
//! This is deliberately the *incremental* path, not `--only
//! event-facts`. A full rebuild TRUNCATEs and replays 1.4M rows in
//! ~90s; catch-up inserts only what is past the watermark, which is
//! nothing at all on most ticks. That difference is what makes a
//! five-minute cadence reasonable.
//!
//! Oneshot, fired by `boss-views-catchup.timer`. Exits non-zero on
//! failure so a broken projection surfaces in `systemctl
//! list-units --failed` rather than silently freezing at a watermark.

use anyhow::{Context, Result};
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-views-catchup",
    about = "Project audit_log rows past the event_facts watermark",
    version
)]
struct Cli {
    #[arg(long, env = "BOSS_POSTGRES_URL")]
    postgres_url: String,
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

    // Two connections is plenty for one INSERT…SELECT loop, and this
    // runs alongside every API service on the same Postgres.
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&cli.postgres_url)
        .await
        .context("connecting to Postgres")?;

    let started = std::time::Instant::now();
    let report = boss_views::catch_up_event_facts(&pool)
        .await
        .context("catching up event_facts")?;

    tracing::info!(
        rows_projected = report.rows_projected,
        high_water = report.high_water,
        elapsed_ms = started.elapsed().as_millis(),
        "event_facts catch-up complete"
    );
    Ok(())
}
