//! `boss-classes-api` service: the Class registry over Postgres — open
//! reads, and three policy-gated, evented writes (backlog 553cf479).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_classes::classes_config::ClassesApiConfig;
use boss_classes::http::ClassesApiState;
use boss_classes::port::ClassRepository;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-classes-api",
    about = "Boss Class registry API service",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-classes-api.toml")]
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
    let cfg = ClassesApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-classes-api starting");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;
    let classes: Arc<dyn ClassRepository> = Arc::new(boss_classes::PgClasses::new(pool.clone()));

    // The three write doors ask policy (backlog 553cf479), wired the way
    // boss-subject-kinds-api wires its door: the sim bypass is installed
    // on a sim instance only and admits only a sim caller there
    // (85e7f10f).
    let policy: Arc<dyn boss_policy_client::PolicyClient> =
        boss_policy_client::SimBypassPolicyClient::from_env(Arc::new(
            boss_policy_client::ReqwestPolicyClient::new(
                "classes",
                std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy")),
            ),
        ));

    // Request-time comparison reads the complete local projection, never
    // the registry services whose own authorization now reports roles.
    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let role_source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("classes"),
    )?);
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let tally = boss_events::role_tally::durable("classes", mode.clone(), Some(&pool));
    let app = boss_classes::role_reports::mount_snapshot(
        ClassesApiState { classes, policy },
        roles.clone(),
        mode.clone(),
        tally,
    );
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
    info!(addr = %http_addr, "classes HTTP API listening");

    let app = boss_core::machine_gate::mount(
        app,
        "classes",
        &["/api/classes/health"],
        Some(boss_events::outbox::PgOutboxRecorder::shared(&pool)),
    );
    let (role_stop, stop) = tokio::sync::watch::channel(false);
    let role_refresh = tokio::spawn(roles.run_refresh_loop(
        role_source,
        mode,
        boss_policy_client::role_service::REFRESH_CADENCE,
        stop,
    ));
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let interrupt = async {
                if let Err(error) = tokio::signal::ctrl_c().await {
                    tracing::warn!(%error, "classes interrupt signal unavailable");
                    std::future::pending::<()>().await;
                }
            };
            // Gate evidence owns process termination; this path owns interrupts.
            interrupt.await;
        })
        .await;
    let _ = role_stop.send(true);
    role_refresh
        .await
        .context("joining classes actor-role refresh")??;
    served?;
    Ok(())
}
