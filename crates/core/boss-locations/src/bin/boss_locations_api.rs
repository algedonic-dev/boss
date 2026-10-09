//! `boss-locations-api` service: the Locations registry over Postgres —
//! open reads, plus the tenant seed door `POST /api/locations/batch`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_locations::http::{LocationsApiState, router};
use boss_locations::locations_config::LocationsApiConfig;
use boss_locations::port::LocationRepository;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-locations-api",
    about = "Boss Locations registry API service",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-locations-api.toml")]
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
    let cfg = LocationsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-locations-api starting");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;
    let locations: Arc<dyn LocationRepository> =
        Arc::new(boss_locations::PgLocations::new(pool.clone()));

    // The batch door asks policy (backlog 59deda40), wired the way
    // boss-classes-api wires its doors: the sim bypass is installed on a
    // sim instance only and admits only a sim caller there (85e7f10f).
    let policy: Arc<dyn boss_policy_client::PolicyClient> =
        boss_policy_client::SimBypassPolicyClient::from_env(Arc::new(
            boss_policy_client::ReqwestPolicyClient::new(
                "locations",
                std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy")),
            ),
        ));

    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("locations"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "locations",
        "/api/locations/actor-role-reports",
        policy,
        roles.clone(),
        mode.clone(),
        boss_events::role_tally::durable("locations", mode.clone(), Some(&pool)),
    );
    let policy = wiring.policy;

    let state = LocationsApiState { locations, policy };
    let app = router(state).merge(wiring.inventory);
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
    info!(addr = %http_addr, "locations HTTP API listening");

    let app = boss_core::machine_gate::mount(
        app,
        "locations",
        &["/api/locations/health"],
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
