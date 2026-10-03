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
//! It also owns the outbox's retention: an hourly pass, on a task of
//! its own, deletes DELIVERED rows older than `--outbox-retention-hours`
//! (default a week) whose fact `audit_log` holds — until 2026-10-01
//! nothing deleted any, and every event was stored twice for good
//! (backlog eec0c1f3, incident d3c0a67c). See
//! `boss_events::outbox::prune_delivered_outbox`.
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
use boss_events::outbox::{
    DEFAULT_OUTBOX_RETENTION, DEFAULT_PRUNE_BATCH, DEFAULT_PRUNE_MAX_ROWS, drain_outbox_once,
    prune_delivered_outbox,
};
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
    /// service unit runs without it. No retention pass runs in this
    /// mode.
    #[arg(long, default_value_t = false)]
    once: bool,

    /// Keep a DELIVERED outbox row this many hours, then delete it —
    /// its fact is already in audit_log (backlog eec0c1f3). Undelivered
    /// and dead-lettered rows are never deleted. At least 1.
    #[arg(long, default_value_t = DEFAULT_OUTBOX_RETENTION.as_secs() / 3600,
          value_parser = clap::value_parser!(u64).range(1..))]
    outbox_retention_hours: u64,

    /// Seconds between retention passes. The first runs at start.
    #[arg(long, default_value_t = 3600,
          value_parser = clap::value_parser!(u64).range(60..))]
    prune_interval_secs: u64,
}

/// The outbox's retention, on its own task so a pass never stalls the
/// drain: delivery latency is what every step.ready waits on, and a
/// pass of many batches takes seconds. Every pass logs what it deleted,
/// including nothing; every batch that deleted a row staged its own
/// `events.outbox.pruned` fact with the rows. A failed pass is logged
/// and the next one runs on schedule — retention is never a reason for
/// the relay to stop relaying.
async fn retention_loop(pool: sqlx::PgPool, retention: Duration, every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match prune_delivered_outbox(
            &pool,
            retention,
            DEFAULT_PRUNE_BATCH,
            DEFAULT_PRUNE_MAX_ROWS,
        )
        .await
        {
            Ok(stats) => info!(
                deleted = stats.deleted,
                batches = stats.batches,
                retention_hours = retention.as_secs() / 3600,
                "outbox retention pass: delivered rows older than the window deleted"
            ),
            Err(e) => error!(error = %e, "outbox retention pass failed — retrying next interval"),
        }
    }
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
        outbox_retention_hours = cli.outbox_retention_hours,
        "boss-event-relay started"
    );

    if !cli.once {
        tokio::spawn(retention_loop(
            pool.clone(),
            Duration::from_secs(cli.outbox_retention_hours * 3600),
            Duration::from_secs(cli.prune_interval_secs),
        ));
    }

    let mut total_delivered: u64 = 0;
    let mut since_heartbeat: u64 = 0;
    let mut last_heartbeat = std::time::Instant::now();
    loop {
        match drain_outbox_once(&pool, &bus, cli.batch).await {
            Ok(stats) => {
                total_delivered += stats.delivered;
                since_heartbeat += stats.delivered;
                if stats.dead_lettered > 0 {
                    // Loud here too: the drain logged each row, and the
                    // alarm event it staged files the packet
                    // (backlog e4019cbc).
                    error!(
                        dead_lettered = stats.dead_lettered,
                        "outbox rows dead-lettered — the bus refused them"
                    );
                }
                // Idle only when nothing moved: a batch that
                // dead-lettered its only rows made progress.
                if stats.moved() == 0 {
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
