//! `boss-ml-generate` — prediction generator CLI.
//!
//! Runs the three deterministic generators (churn risk, device MTBF,
//! opportunity win-probability) in sequence and writes their
//! predictions through the MlRepository. Intended to be invoked on a
//! cron schedule; safe to run ad-hoc because create_prediction is
//! idempotent per-day.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_ml::config::MlApiConfig;
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-ml-generate",
    about = "Run Boss ML Phase 2 prediction generators",
    version
)]
struct Cli {
    /// Path to the service config (TOML). Shares the boss-ml-api
    /// config since the DB and upstream are identical.
    #[arg(short, long, default_value = "/etc/boss-ml-api.toml")]
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
    let cfg = MlApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let repo: Arc<dyn boss_ml::MlRepository> = Arc::new(boss_ml::PgMlRepo::new(pool.clone()));

    info!("running ML Phase 2 generators");
    let summaries = boss_ml::generators::run_all(repo, &pool).await;
    for s in &summaries {
        info!(
            model = %s.model_id,
            scored = s.entities_scored,
            written = s.predictions_written,
            skipped = s.predictions_skipped,
            "generator summary"
        );
    }
    info!("done");
    Ok(())
}
