//! `boss-people-rebuild` — drop the employees + employee_skills +
//! employee_certifications projections and reconstruct from
//! `audit_log`. See `docs/design/projection-rebuilders.md`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_core::rebuild::resolve_database_url;
use boss_people::people_config::PeopleApiConfig;
use boss_people::rebuild_people;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-people-rebuild",
    about = "Rebuild people projections from audit_log",
    version
)]
struct Cli {
    #[arg(long)]
    database_url: Option<String>,
    #[arg(short, long, default_value = "/etc/boss-people-api.toml")]
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
        .then(|| PeopleApiConfig::load(&cli.config).ok())
        .flatten()
        .map(|cfg| cfg.postgres_url);
    let db_url = resolve_database_url(
        cli.database_url,
        config_url,
        &["DATABASE_URL"],
        "pass --database-url, point --config at a valid \
         boss-people-api.toml, or set DATABASE_URL",
    )?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .with_context(|| "connecting to Postgres")?;
    let report = rebuild_people(&pool)
        .await
        .with_context(|| "rebuilding people projections")?;
    info!(
        events_processed = report.events_processed,
        events_skipped = report.events_skipped,
        employees_upserted = report.employees_upserted,
        employees_deleted = report.employees_deleted,
        "people rebuild complete"
    );
    Ok(())
}
