//! `boss-messages-events-purge` — daily retention sweep over
//! `messages_events`.
//!
//! Invoked by a systemd timer (see
//! `infra/boss-messages-events-purge.timer`). Reads
//! `events_retention_days` from `/etc/boss-messages-api.toml`
//! (sourced from `[messages] events_retention_days` in
//! `tenant.toml`) and deletes rows older than that window.
//!
//! Why messages_events has a retention sweep but `audit_log`
//! doesn't: messages carry user content (PII, business detail
//! someone might want forgotten) so they ride their own
//! immutable trail rather than the compliance-grade audit_log.
//! The per-tenant retention horizon expires that trail without
//! breaking the rest of the audit chain. The writer lives at
//! `crates/boss-events/src/messages_events.rs`.
//!
//! Exit codes:
//! - `0` — sweep succeeded (including "0 rows" runs)
//! - non-zero — operational error (config missing, DB unreachable, …)

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_messages::messages_config::MessagesApiConfig;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-messages-events-purge",
    about = "Daily retention sweep over messages_events",
    version
)]
struct Cli {
    /// Path to a messages-api config file. The sweep reads
    /// `postgres_url` and `events_retention_days` from it.
    #[arg(short, long, default_value = "/etc/boss-messages-api.toml")]
    config: PathBuf,

    /// Override the retention window from the command line. Useful
    /// for one-off backfill runs against an unconfigured deployment.
    #[arg(long)]
    retention_days: Option<i64>,
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

    let cfg = MessagesApiConfig::load(&cli.config)
        .with_context(|| format!("loading config {}", cli.config.display()))?;

    let retention_days = match cli.retention_days.or(cfg.events_retention_days) {
        Some(d) if d > 0 => d,
        Some(d) => {
            warn!(retention_days = d, "non-positive retention; nothing to do");
            return Ok(());
        }
        None => {
            info!("events_retention_days not set; sweep is a no-op for this deployment");
            return Ok(());
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&cfg.postgres_url)
        .await
        .context("connecting to postgres")?;

    let result = sqlx::query(
        "DELETE FROM messages_events \
         WHERE timestamp < NOW() - make_interval(days => $1::int)",
    )
    .bind(retention_days)
    .execute(&pool)
    .await
    .context("running purge query")?;

    info!(
        retention_days,
        rows_deleted = result.rows_affected(),
        "messages_events purge complete"
    );
    Ok(())
}
