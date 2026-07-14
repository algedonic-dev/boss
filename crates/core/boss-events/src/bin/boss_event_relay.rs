//! `boss-event-relay` — drains the transactional event outbox.
//!
//! The write half of Option B in
//! `docs/design/transactional-audit-log.md`: services INSERT their
//! events into `event_outbox` inside the same transaction as the
//! state change; this relay is the single mover from the outbox into
//! `audit_log` (where the chain-hash trigger runs, uncontended — one
//! writer) and onto NATS, in outbox order, at-least-once. Crash-safe
//! by construction: `delivered_at` is stamped only after both the
//! audit INSERT is committed and the publish succeeded, and the audit
//! side is idempotent by `event_id`, so a restart resumes exactly
//! where it left off (see `boss_events::outbox` for the per-crash-
//! point analysis).
//!
//! Inert until an emitter is migrated to `record_event_in_tx` — an
//! empty outbox costs one indexed probe per idle tick.
//!
//! Exit codes: 0 clean shutdown (never in service mode), 1
//! operational error at startup (DB/NATS unreachable, bad config).
//! Runtime storage errors log + back off; the relay never gives up —
//! an undrained outbox is unpublished truth.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use boss_core::port::EventBus;
use boss_core::rebuild::resolve_database_url;
use boss_events::outbox::drain_outbox_once;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-event-relay",
    about = "Drain the transactional event outbox into audit_log + NATS",
    version
)]
struct Cli {
    /// Postgres connection string. Falls back to the config file's
    /// `postgres_url`, then `BOSS_RELAY_DATABASE_URL`, then
    /// `DATABASE_URL`.
    #[arg(long)]
    database_url: Option<String>,

    /// NATS URL. Falls back to the config file's `nats_url`, then
    /// `BOSS_NATS_URL`. Required — publishing is this binary's job;
    /// there is no bus-less mode.
    #[arg(long)]
    nats_url: Option<String>,

    /// Optional config file (TOML with `postgres_url` / `nats_url`
    /// keys — the standard service-config shape) so the systemd unit
    /// can share an existing `/etc/boss-*-api.toml` instead of
    /// minting a new secrets file, the same way
    /// boss-ledger-replay-check shares boss-ledger-api.toml.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Max rows per drain batch. The audit inserts of one batch share
    /// one short transaction (and therefore one hold of the chain
    /// lock) — keep it modest so legacy direct writers are never
    /// blocked for long.
    #[arg(long, default_value_t = 100)]
    batch: i64,

    /// Idle sleep between drains when the outbox is empty.
    #[arg(long, default_value_t = 250)]
    idle_sleep_ms: u64,

    /// Drain until empty, then exit 0. For scripts and tests; the
    /// service unit runs without it.
    #[arg(long, default_value_t = false)]
    once: bool,
}

#[derive(serde::Deserialize, Default)]
struct ConfigFile {
    postgres_url: Option<String>,
    nats_url: Option<String>,
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
    let cfg: ConfigFile = match &cli.config {
        Some(path) => {
            let body = std::fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            toml::from_str(&body).with_context(|| format!("parsing config {}", path.display()))?
        }
        None => ConfigFile::default(),
    };

    let db_url = resolve_database_url(
        cli.database_url,
        cfg.postgres_url,
        &["BOSS_RELAY_DATABASE_URL", "DATABASE_URL"],
        "boss-event-relay",
    )?;
    let nats_url = cli
        .nats_url
        .or(cfg.nats_url)
        .or_else(|| std::env::var("BOSS_NATS_URL").ok())
        .context("NATS url required: --nats-url, config nats_url, or BOSS_NATS_URL")?;

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .context("connecting to Postgres")?;
    let bus: Arc<dyn EventBus> = Arc::new(
        boss_nats::NatsEventBus::connect(&nats_url)
            .await
            .map_err(|e| anyhow::anyhow!("connecting to NATS at {nats_url}: {e}"))?,
    );

    info!(
        batch = cli.batch,
        once = cli.once,
        "boss-event-relay started"
    );

    let mut total_delivered: u64 = 0;
    let mut since_heartbeat: u64 = 0;
    let mut last_heartbeat = std::time::Instant::now();
    loop {
        match drain_outbox_once(&pool, &bus, cli.batch).await {
            Ok(stats) => {
                total_delivered += stats.delivered;
                since_heartbeat += stats.delivered;
                if stats.delivered == 0 {
                    if cli.once {
                        info!(total_delivered, "outbox empty — exiting (--once)");
                        return Ok(());
                    }
                    tokio::time::sleep(Duration::from_millis(cli.idle_sleep_ms)).await;
                }
                // Busy: loop immediately — drain the backlog at full
                // speed rather than sleeping between full batches.
            }
            Err(e) => {
                error!(error = %e, "outbox drain failed — backing off and retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        if last_heartbeat.elapsed() >= Duration::from_secs(60) {
            info!(
                total_delivered,
                last_minute = since_heartbeat,
                "relay heartbeat"
            );
            since_heartbeat = 0;
            last_heartbeat = std::time::Instant::now();
        }
    }
}
