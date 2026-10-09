//! `boss-subject-kinds-api` service: the SubjectKind registry over
//! Postgres — open reads, and one policy-gated, evented write (a kind's
//! metadata merge, backlog abc2e9d5).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_subject_kinds::http::{SubjectKindsApiState, router};
use boss_subject_kinds::port::SubjectKindRepository;
use boss_subject_kinds::subject_kinds_config::SubjectKindsApiConfig;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-subject-kinds-api",
    about = "Boss SubjectKind registry API service",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-subject-kinds-api.toml")]
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
    let cfg = SubjectKindsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-subject-kinds-api starting");

    let subjects_pool: Option<sqlx::PgPool>;
    let subject_kinds: Arc<dyn SubjectKindRepository> = {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .connect(&cfg.postgres_url)
            .await
            .with_context(|| "connecting to Postgres")?;
        subjects_pool = Some(pool.clone());
        Arc::new(boss_subject_kinds::PgSubjectKinds::new(pool))
    };

    // The metadata write door asks policy (backlog abc2e9d5), wired the
    // way boss-people-api wires it: the sim bypass is installed on a sim
    // instance only and admits only a sim caller there (85e7f10f).
    let policy: Arc<dyn boss_policy_client::PolicyClient> =
        boss_policy_client::SimBypassPolicyClient::from_env(Arc::new(
            boss_policy_client::ReqwestPolicyClient::new(
                "subject-kinds",
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
        boss_policy_client::User::service("subject-kinds"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "subject-kinds",
        "/api/subject-kinds/actor-role-reports",
        policy,
        roles.clone(),
        mode.clone(),
        boss_events::role_tally::durable("subject-kinds", mode.clone(), subjects_pool.as_ref()),
    );
    let policy = wiring.policy;

    let state = SubjectKindsApiState {
        subject_kinds,
        policy,
    };
    let mut app = router(state).merge(wiring.inventory);
    // The subjects identity surface (R1): mint + existence probe.
    // Postgres-only — the identity table has no in-memory twin.
    if let Some(pool) = subjects_pool.clone() {
        app = app.merge(boss_subject_kinds::subjects::subjects_router(pool));
    }
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
    info!(addr = %http_addr, "subject-kinds HTTP API listening");

    let app = boss_core::machine_gate::mount(
        app,
        "subject-kinds",
        &["/api/subject-kinds/health"],
        subjects_pool
            .as_ref()
            .map(boss_events::outbox::PgOutboxRecorder::shared),
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
