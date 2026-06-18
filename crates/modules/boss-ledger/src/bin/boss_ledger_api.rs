//! `boss-ledger-api` — read-only HTTP surface over the GL projection.
//!
//! Posting happens inside the domain write transactions (boss-commerce +
//! boss-inventory call `boss_ledger::post_fact_in_tx`). This binary is for
//! queries: chart, trial balance, entry lookups for drill-down.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_ledger::config::LedgerApiConfig;
use boss_ledger::http::{LedgerApiState, router};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-ledger-api", about = "Boss Ledger API", version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-ledger-api.toml")]
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
    let cfg = LedgerApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        postgres_url = %boss_core::startup::mask_password(&cfg.postgres_url),
        http_bind = %cfg.http_bind,
        "boss-ledger-api starting"
    );

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    // Seed the executive-role cache from the Class registry so the
    // IT-providers gate's `has_global_read` recognises tenant-defined
    // executives. Skip on missing config or transport failure —
    // platform-admin + audit-readonly still grant global read.
    if let Some(url) = &cfg.classes_api_url {
        let client = boss_classes_client::ReqwestClassesClient::new(url.clone());
        match boss_classes_client::seed_executive_role_cache(&client).await {
            Ok(n) => info!(count = n, classes_api_url = %url, "executive role cache seeded"),
            Err(e) => {
                tracing::warn!(error = %e, "failed to seed executive roles from classes; falling back to platform-admin/audit-readonly only")
            }
        }
    } else {
        info!("classes_api_url unset; executive role cache disabled");
    }

    // Clock-api URL: env override (BOSS_CLOCK_URL) takes
    // precedence; default goes to the canonical port via
    // boss-ports. Production deploys point at a wall-mode
    // clock-api; demo deploys point at the sim-mode clock-api.
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            let p = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "ledger")
                .with_audit(Arc::new(boss_events::PgAuditWriter::new(pool.clone())))
                .with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
                    clock.clone(),
                )));
            info!(nats_url = %url, "domain event publishing + audit trail enabled (with sim probe)");
            Some(Arc::new(p))
        }
        None => {
            info!("no nats_url configured — ledger events will not be published");
            None
        }
    };

    let state = LedgerApiState {
        pool: pool.clone(),
        publisher,
        clock,
    };
    let app = router(state);
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
    info!(addr = %http_addr, "boss-ledger-api listening");
    axum::serve(listener, app).await?;
    Ok(())
}
