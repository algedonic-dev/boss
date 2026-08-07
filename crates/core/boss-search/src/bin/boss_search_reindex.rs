//! `boss-search-reindex` — keep `search_index` from going stale.
//!
//! `search_index` is a projection of the identity tables, and until
//! this ran on a timer the ONLY thing that refreshed it was a full
//! `boss-rebuild-all`. On a live box that means global search is
//! correct for a few minutes after a manual rebuild and wrong
//! thereafter. Measured before writing this: the index held 25 jobs
//! against 4,782 real ones (0.5%), 725 subjects against 20,122
//! (3.6%), and nothing at all newer than two hours — while the sim
//! adds roughly forty Jobs a minute. A search surface that cannot
//! find 99% of the corpus is not a slow search, it is a wrong answer.
//!
//! Unlike `boss-views-catchup` this is a FULL rebuild, not an
//! incremental one, and that is a property of the index rather than a
//! shortcut: events are capped per Subject via `ROW_NUMBER`, so "the
//! most recent 50 events for this Subject" changes as new events
//! arrive and there is no watermark that makes an append correct. A
//! periodic TRUNCATE-and-replay is the honest interim. The rebuild
//! takes an advisory lock and runs in one transaction, so a tick that
//! overlaps a `boss-rebuild-all` waits rather than corrupting.
//!
//! Oneshot, fired by `boss-search-reindex.timer`. Exits non-zero on
//! failure so a broken index surfaces in `systemctl list-units
//! --failed` rather than silently freezing at whatever it last held.

use anyhow::{Context, Result};
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-search-reindex",
    about = "Rebuild search_index from the identity tables",
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

    // Two connections is plenty for one rebuild transaction, and this
    // runs alongside every API service on the same Postgres.
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&cli.postgres_url)
        .await
        .context("connecting to Postgres")?;

    let started = std::time::Instant::now();
    let report = boss_search::rebuild_search(&pool)
        .await
        .context("rebuilding search_index")?;

    tracing::info!(
        subjects_indexed = report.subjects_indexed,
        jobs_indexed = report.jobs_indexed,
        events_indexed = report.events_indexed,
        elapsed_ms = started.elapsed().as_millis(),
        "search_index rebuild complete"
    );
    Ok(())
}
