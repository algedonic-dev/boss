//! `boss-search-api` — the global search read surface.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-search-api", about = "Global search API", version)]
struct Cli {
    #[arg(long, env = "BOSS_POSTGRES_URL")]
    postgres_url: String,
    #[arg(long, default_value_t = boss_ports::prod("search"))]
    http_port: u16,
    #[arg(long, env = "BOSS_POLICY_URL", default_value_t = boss_ports::url("policy"))]
    policy_url: String,
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

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&cli.postgres_url)
        .await
        .context("connecting to Postgres")?;

    // Same wrapping jobs-api uses: on a sim instance a sim caller is
    // authorized at the boundary; everything else is enforced per-role
    // by the inner client (backlog 85e7f10f).
    let policy: std::sync::Arc<dyn boss_policy_client::PolicyClient> =
        boss_policy_client::SimBypassPolicyClient::from_env(std::sync::Arc::new(
            boss_policy_client::ReqwestPolicyClient::new("search", cli.policy_url),
        ));

    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("search"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "search",
        "/api/search/actor-role-reports",
        policy,
        roles.clone(),
        mode.clone(),
        boss_events::role_tally::durable("search", mode.clone(), Some(&pool)),
    );
    let policy = wiring.policy;

    let app = boss_search::http::router(boss_search::http::SearchApiState {
        pool: pool.clone(),
        policy,
    })
    .merge(wiring.inventory);
    let addr = format!("127.0.0.1:{}", cli.http_port);
    tracing::info!(addr = %addr, "boss-search-api listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let app = boss_core::machine_gate::mount(
        app,
        "search",
        &["/api/search/health"],
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
