//! `boss-calendar-api` — production HTTP service for the calendar
//! primitive. Lives behind the gateway at `/api/calendar/*`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_calendar::{CalendarApiConfig, CalendarApiState, CalendarClient, router};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-calendar-api",
    about = "Boss Calendar API service",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-calendar-api.toml")]
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
    let cfg = CalendarApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-calendar-api starting");

    let (calendar, publisher, recorder): (
        Arc<dyn CalendarClient>,
        Option<boss_core::publisher::DomainPublisher>,
        Arc<dyn boss_core::port::EventRecorder>,
    ) = {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .connect(&cfg.postgres_url)
            .await
            .with_context(|| "connecting to Postgres")?;
        let calendar: Arc<dyn CalendarClient> =
            Arc::new(boss_calendar::PgCalendar::new(pool.clone()));
        let recorder = boss_events::outbox::PgOutboxRecorder::shared(&pool);
        let publisher = match &cfg.nats_url {
            Some(url) => {
                let bus = boss_nats::NatsEventBus::connect(url)
                    .await
                    .with_context(|| format!("connecting to NATS at {url}"))?;
                let pub_ = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "calendar")
                    .with_audit(Arc::new(boss_events::PgAuditWriter::new(pool)));
                info!(nats_url = %url, "domain event publishing + audit trail enabled");
                Some(pub_)
            }
            None => {
                info!("no nats_url configured — calendar events will not be published");
                None
            }
        };
        (calendar, publisher, recorder)
    };

    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    // Wire the sim-mode probe into the publisher so every stamp
    // injects `_simulated: bool` into the audit_log payload without
    // per-handler changes.
    let publisher = publisher.map(|p| {
        p.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
            clock.clone(),
        )))
    });

    // Every reservation write asks it (backlog 11721a25). Wrapped like
    // every service's: the sim-origin bypass exists only on a sim
    // instance.
    let policy_url = std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy"));
    let policy = boss_policy_client::SimBypassPolicyClient::from_env(Arc::new(
        boss_policy_client::ReqwestPolicyClient::new("calendar", policy_url.clone()),
    ));
    info!(%policy_url, "policy client wired");

    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("calendar"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "calendar",
        "/api/calendar/actor-role-reports",
        policy,
        roles.clone(),
        mode.clone(),
        Arc::new(boss_policy_client::role_reporting::ReportTally::new(
            boss_policy_client::role_service::REPORT_CAPACITY,
        )),
    );
    let policy = wiring.policy;

    let state = CalendarApiState {
        calendar,
        publisher,
        clock,
        policy,
    };
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
    info!(addr = %http_addr, "calendar HTTP API listening");
    let app =
        boss_core::machine_gate::mount(app, "calendar", &["/api/calendar/health"], Some(recorder));
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
