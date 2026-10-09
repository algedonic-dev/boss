//! `boss-events-api` — audit_log read surface.
//!
//! Hosts `/api/events/tail`, `/api/events/stream`,
//! `/api/events/export`, and `/api/events/public-tail` from
//! `boss_events::tail_http::audit_tail_router`. Pre-2026-06 these
//! routes were mounted in `boss-people-api` for convenience
//! (people-api already had the Postgres pool wired up). Splitting
//! them into a dedicated service makes audit_log access a
//! first-class tier-1 surface — same shape every other core domain
//! takes (boss-jobs-api, boss-ledger-api, boss-classes-api).
//!
//! Auth: same as the original router — operator/auditor tier or
//! has_global_read role for tail/stream/export; public-tail is
//! unauth (curated topic allowlist).
//!
//! `/api/events/gate-window?gate=machine-gate|policy-check&hours=72`
//! joins the durable gate log and all required live tallies behind the
//! same protected read door. It reports coverage and named gaps;
//! it neither authorizes a mode change nor supplies a rotation verdict.
//!
//! Also the one write it serves: `POST /api/events/outbox/{id}/redeliver`
//! and `/resolve`, the dead-letter door `boss events redeliver` speaks
//! to (`boss_events::outbox_http`, Operator tier only).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_events::events_api_config::EventsApiConfig;
use boss_events::outbox_http::outbox_router_with_reports;
use boss_events::tail_http::audit_tail_router_with_reports;
use boss_events_api::gate_window_http::{LocalTallies, gate_window_router_with_reports};
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-events-api",
    about = "Boss audit_log read surface (tail / stream / export)",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-events-api.toml")]
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
    let cfg = EventsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-events-api starting");

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres for audit_log reads")?;

    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("events"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "events",
        "/api/events/actor-role-reports",
        Arc::new(boss_policy_client::ReqwestPolicyClient::new(
            "events",
            std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy")),
        )),
        roles.clone(),
        mode.clone(),
        boss_events::role_tally::durable("events", mode.clone(), Some(&pool)),
    );

    // The dead-letter door (backlog e22b692e): this service owns
    // event_outbox, so the act on one of its rows happens here, behind
    // the signed caller and the Operator tier — `outbox_http`'s header.
    let app = audit_tail_router_with_reports(pool.clone(), Some(wiring.guards.clone()))
        .merge(outbox_router_with_reports(
            pool.clone(),
            Some(wiring.guards.clone()),
        ))
        .merge(gate_window_router_with_reports(
            std::sync::Arc::new(boss_events::PgGateEvidence::new(pool.clone())),
            std::sync::Arc::new(LocalTallies::from_ports().context("building gate tally reader")?),
            std::sync::Arc::new(boss_core::clock::WallClock),
            Some(wiring.guards),
        ))
        .merge(wiring.inventory);
    // Sim-origin middleware: extract x-sim-origin header and set the
    // per-request task-local so the publisher inherits the sim
    // marker. Closes the gap where a sim chain could trigger a
    // non-sim event on a service running with a wall clock.
    let app = app.layer(axum::middleware::from_fn(
        boss_policy_client::request_context_middleware,
    ));

    let http_addr: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "boss-events-api listening");

    let app = boss_core::machine_gate::mount(
        app,
        "events",
        &["/api/events/health"],
        Some(boss_events::outbox::PgOutboxRecorder::shared(&pool)),
    );
    boss_policy_client::role_service::serve_with_refresh(
        listener,
        app,
        roles,
        source,
        mode,
        boss_policy_client::role_service::REFRESH_CADENCE,
        boss_policy_client::role_service::shutdown_signal(),
    )
    .await?;
    Ok(())
}
