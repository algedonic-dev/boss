//! `boss-assets-api` service: assets domain + NATS event bus + HTTP API.
//!
//! Wires the assets repository (Postgres or in-memory) to NATS for
//! real-time event distribution and exposes an axum HTTP API with SSE.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_assets::asset_config::AssetsApiConfig;
use boss_assets::bridge::core_event_to_system;
use boss_assets::http::{AssetsApiState, InsightsClients, router};
use boss_assets::in_memory::InMemoryAssets;
use boss_assets::port::AssetsRepository;
use boss_assets::sse::{self, SseHub};
use boss_catalog_client::ReqwestCatalogClient;
use boss_classes_client::{ClassesClient, ReqwestClassesClient};
use boss_inventory_client::ReqwestInventoryClient;
use boss_jobs_client::ReqwestJobsClient;
use boss_nats::NatsEventBus;
use boss_people_client::{PeopleClient, ReqwestPeopleClient};
use clap::Parser;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-assets-api", about = "BOSS Assets API service", version)]
struct Cli {
    /// Path to the service config (TOML)
    #[arg(short, long, default_value = "/etc/boss-assets-api.toml")]
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
    let cfg = AssetsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        nats_url = %cfg.nats_url,
        http_bind = %cfg.http_bind,
        postgres = cfg.postgres_url.is_some(),
        people_api_url = %cfg.people_api_url,
        "boss-assets-api starting"
    );

    let people_client: Arc<dyn PeopleClient> =
        Arc::new(ReqwestPeopleClient::new(cfg.people_api_url.clone()));

    // Fail-loud: production always wires the Class registry. The asset
    // event taxonomy fields (source/coverage/condition) live in JSONB
    // with no schema CHECK, so the ingest gate is the only defense.
    let classes_client: Arc<dyn ClassesClient> =
        Arc::new(ReqwestClassesClient::new(cfg.classes_api_url.clone()));
    info!(classes_api_url = %cfg.classes_api_url, "classes client wired");

    let insights_clients = InsightsClients {
        catalog: Arc::new(ReqwestCatalogClient::new(cfg.catalog_api_url.clone())),
        jobs: Arc::new(ReqwestJobsClient::new(cfg.jobs_api_url.clone())),
        inventory: Arc::new(ReqwestInventoryClient::new(cfg.inventory_api_url.clone())),
    };
    info!(
        catalog = %cfg.catalog_api_url,
        jobs = %cfg.jobs_api_url,
        inventory = %cfg.inventory_api_url,
        "device-insights cross-service clients configured"
    );

    // Connect to NATS.
    let bus = Arc::new(
        NatsEventBus::connect(&cfg.nats_url)
            .await
            .with_context(|| format!("connecting to NATS at {}", cfg.nats_url))?,
    );

    let hub = SseHub::new();
    let (cancel_tx, cancel_rx) = watch::channel(false);

    // Build the publisher: bus + (optional) Postgres audit writer.
    #[allow(unused_mut)]
    let mut publisher = boss_core::publisher::DomainPublisher::new(
        bus.clone() as std::sync::Arc<dyn boss_core::port::EventBus>,
        "assets",
    );

    // Choose storage backend: Postgres when configured, in-memory otherwise.
    // The Postgres pool is shared across PgAssets and PgAuditWriter.
    #[cfg(feature = "postgres")]
    if let Some(ref pg_url) = cfg.postgres_url {
        info!("using Postgres assets storage");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(20)
            .connect(pg_url)
            .await
            .with_context(|| "connecting to Postgres")?;
        publisher = publisher.with_audit(std::sync::Arc::new(boss_events::PgAuditWriter::new(
            pool.clone(),
        )));
        info!("audit_log persistence enabled");
        let assets = Arc::new(boss_assets::PgAssets::new(pool.clone()));
        return run_server(
            assets,
            bus,
            publisher,
            people_client,
            classes_client,
            insights_clients,
            hub,
            cancel_tx,
            cancel_rx,
            &cfg.http_bind,
            Some(pool),
        )
        .await;
    }

    boss_core::startup::require_postgres_or_explicit_inmemory("boss-assets-api")?;
    info!("using in-memory assets storage (no postgres_url configured)");
    let assets = Arc::new(InMemoryAssets::new());
    run_server(
        assets,
        bus,
        publisher,
        people_client,
        classes_client,
        insights_clients,
        hub,
        cancel_tx,
        cancel_rx,
        &cfg.http_bind,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_server<R: AssetsRepository + 'static>(
    assets: Arc<R>,
    bus: Arc<NatsEventBus>,
    publisher: boss_core::publisher::DomainPublisher,
    people_client: Arc<dyn PeopleClient>,
    classes_client: Arc<dyn ClassesClient>,
    insights_clients: InsightsClients,
    hub: SseHub,
    cancel_tx: watch::Sender<bool>,
    cancel_rx: watch::Receiver<bool>,
    http_bind: &str,
    #[cfg(feature = "postgres")] pool: Option<sqlx::PgPool>,
    #[cfg(not(feature = "postgres"))] _pool: Option<()>,
) -> Result<()> {
    // Spawn NATS ingress: subscribe to asset.>, decode, append.
    let ingress_assets = assets.clone();
    let ingress_bus = bus.clone();
    let ingress_rx = cancel_rx.clone();
    let ingress_task = tokio::spawn(async move {
        if let Err(e) = run_nats_ingress(ingress_bus, ingress_assets, ingress_rx).await {
            error!(error = %e, "NATS ingress exited with error");
        }
    });

    // Spawn NATS -> SSE forwarder.
    let forwarder_hub = hub.clone();
    let forwarder_client = bus.client().clone();
    let forwarder_rx = cancel_rx.clone();
    let forwarder_task = tokio::spawn(async move {
        if let Err(e) = sse::run_nats_forwarder(forwarder_client, forwarder_hub, forwarder_rx).await
        {
            error!(error = %e, "SSE forwarder exited with error");
        }
    });

    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    // Wire the sim-mode probe into the publisher so every
    // emit_at automatically injects `_simulated: bool` into
    // the audit_log payload without per-handler changes.
    // `publisher` here is `DomainPublisher` (not Option).
    let publisher = publisher.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
        clock.clone(),
    )));

    // Start axum HTTP server.
    let state = AssetsApiState {
        assets,
        bus,
        publisher,
        people_client,
        classes_client: Some(classes_client),
        hub,
        policy: None,
        insights_clients: Some(insights_clients),
        clock,
    };
    let http_addr: SocketAddr = http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{http_bind}`"))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "assets HTTP API listening");
    // Without the `postgres` feature `app` is never reassigned, so the
    // `mut` binding is cfg-gated to match. Clippy otherwise fires
    // `unused_mut` on the default-feature build.
    #[cfg(not(feature = "postgres"))]
    let app = router(state);
    #[cfg(feature = "postgres")]
    let mut app = router(state);
    // Merge in the per-asset Parts router when Postgres is wired.
    // In-memory mode skips it (no backing store for parts tables yet);
    // the main assets endpoints still work.
    #[cfg(feature = "postgres")]
    if let Some(ref pool) = pool {
        app = app.merge(boss_assets::asset_parts::asset_parts_router(pool.clone()));
    }
    // Sim-origin middleware: extract x-sim-origin header and set the
    // per-request task-local so the publisher inherits the sim
    // marker. Closes the gap where a sim chain could trigger a
    // non-sim event on a service running with a wall clock.
    let app = app.layer(axum::middleware::from_fn(
        boss_policy_client::request_context_middleware,
    ));
    let mut http_rx = cancel_rx.clone();
    let http_task = tokio::spawn(async move {
        let shutdown = async move {
            let _ = http_rx.changed().await;
        };
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            error!(error = %e, "HTTP server exited with error");
        }
    });

    // Wait for Ctrl+C.
    tokio::signal::ctrl_c().await.ok();
    info!("shutdown signal received");
    let _ = cancel_tx.send(true);

    let _ = tokio::join!(ingress_task, forwarder_task, http_task);
    info!("boss-assets-api shut down cleanly");
    Ok(())
}

/// Subscribe to `asset.>` on the EventBus, decode into AssetEvents,
/// and append to the assets repository. Duplicate events are logged and skipped.
async fn run_nats_ingress<R: AssetsRepository + 'static>(
    bus: Arc<NatsEventBus>,
    assets: Arc<R>,
    mut cancel: watch::Receiver<bool>,
) -> Result<()> {
    use boss_core::port::EventBus;

    let mut stream = bus
        .subscribe("asset.>")
        .await
        .map_err(|e| anyhow::anyhow!("subscribing to asset.>: {e}"))?;

    loop {
        tokio::select! {
            _ = cancel.changed() => {
                if *cancel.borrow() { break; }
            }
            maybe_event = stream.next() => {
                let Some(core_event) = maybe_event else { break; };
                if let Some(system_event) = core_event_to_system(&core_event) {
                    match assets.append(system_event).await {
                        Ok(()) => {}
                        Err(boss_assets::port::AssetsError::DuplicateEvent(id)) => {
                            warn!(event_id = %id, "duplicate event from NATS, skipping");
                        }
                        Err(e) => {
                            error!(error = %e, "failed to append ingress event");
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
