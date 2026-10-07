//! `boss-products-api` — HTTP service for the finished-product
//! catalog + per-location on-hand inventory.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_products::config::ProductsApiConfig;
use boss_products::http::{ProductsApiState, router};
use boss_products::postgres::PgProducts;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-products-api", about = "Boss Products API", version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-products-api.toml")]
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
    let cfg = ProductsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-products-api starting");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let products = Arc::new(PgProducts::new(pool.clone()));

    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    let classes_client = Some(Arc::new(boss_classes_client::ReqwestClassesClient::new(
        cfg.classes_api_url.clone(),
    )) as Arc<dyn boss_classes_client::ClassesClient>);
    info!(classes_url = %cfg.classes_api_url, "product taxonomy validation enabled");

    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            // with_sim_probe → publisher auto-injects
            // `_simulated: bool` on every audit_log row.
            let p = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "products")
                .with_audit(Arc::new(boss_events::PgAuditWriter::new(pool.clone())))
                .with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
                    clock.clone(),
                )));
            info!(nats_url = %url, "domain event publishing + audit trail enabled");
            Some(Arc::new(p))
        }
        None => {
            info!("no nats_url configured — products events will not be published");
            None
        }
    };

    let state = ProductsApiState {
        products,
        publisher,
        clock,
        classes_client,
    };
    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("products"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "products",
        "/api/products/actor-role-reports",
        Arc::new(boss_policy_client::ReqwestPolicyClient::new(
            "products",
            std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy")),
        )),
        roles.clone(),
        mode.clone(),
        Arc::new(boss_policy_client::role_reporting::ReportTally::new(
            boss_policy_client::role_service::REPORT_CAPACITY,
        )),
    );
    let app = products_http_router(router(state), wiring.inventory);
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
    info!(addr = %http_addr, "boss-products-api listening");
    let app = boss_core::machine_gate::mount(
        app,
        "products",
        &["/api/products/health"],
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

fn products_http_router(domain: axum::Router, inventory: axum::Router) -> axum::Router {
    domain.merge(inventory)
}

#[cfg(test)]
mod role_inventory_tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::get,
    };
    use boss_policy_client::{Action, FakePolicyClient, Resource, Scope, User};
    use tower::ServiceExt;

    #[tokio::test]
    async fn products_mount_preserves_domain_and_protects_inventory() {
        let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
            boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
            Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
        ));
        let tally = Arc::new(boss_policy_client::role_reporting::ReportTally::new(8));
        let policy = Arc::new(
            FakePolicyClient::builder()
                .allow(
                    "report-reader",
                    Action::Read,
                    Resource::policy_rule(),
                    Scope::All,
                )
                .build(),
        );
        let wiring = boss_policy_client::role_service::assemble(
            "products",
            "/api/products/actor-role-reports",
            policy,
            roles,
            Arc::new(boss_policy_client::role_reporting::ReportMode::Report),
            tally.clone(),
        );
        let app = products_http_router(
            Router::new().route("/api/products/health", get(|| async { "original-health" })),
            wiring.inventory,
        );
        let domain = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/products/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(domain.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(domain.into_body(), 4096)
                .await
                .unwrap(),
            "original-health"
        );
        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/products/actor-role-reports")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let mut user = User::service("reader");
        user.role = "report-reader".into();
        let allowed = app
            .oneshot(
                Request::builder()
                    .uri("/api/products/actor-role-reports")
                    .header("x-boss-user", serde_json::to_string(&user).unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(allowed.into_body(), 16384)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["service"], "products");
        assert_eq!(body["snapshot"]["state"], "never-loaded");
        assert_eq!(body["report"]["durable_window"], false);
        assert!(tally.snapshot().rows.is_empty());
    }
}
