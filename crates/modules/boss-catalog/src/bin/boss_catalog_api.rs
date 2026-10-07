//! `boss-catalog-api` service: device-catalog API backed by Postgres.
//!
//! Connects to a Postgres database and serves the device catalog —
//! system models, parts, consumables — over HTTP.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_assets_client::{AssetsClient, ReqwestAssetsClient};
use boss_catalog::http::{KbApiState, router};
use boss_catalog::kb_config::KbApiConfig;
use boss_classes_client::{ClassesClient, ReqwestClassesClient};
use boss_clock_client::{ClockClient, ReqwestClockClient};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-catalog-api",
    about = "Boss Knowledge Base API service",
    version
)]
struct Cli {
    /// Path to the service config (TOML)
    #[arg(short, long, default_value = "/etc/boss-catalog-api.toml")]
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
    let cfg = KbApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        postgres_url = %boss_core::startup::mask_password(&cfg.postgres_url),
        http_bind = %cfg.http_bind,
        assets_api_url = %cfg.assets_api_url,
        classes_api_url = %cfg.classes_api_url,
        "boss-catalog-api starting"
    );

    let assets_client: Arc<dyn AssetsClient> =
        Arc::new(ReqwestAssetsClient::new(cfg.assets_api_url.clone()));

    // Connect to Postgres.
    let pg_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let catalog = Arc::new(boss_catalog::PgKb::new(pg_pool.clone()));

    // Connect to NATS for domain event publishing + audit trail (optional).
    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            let pub_ = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "kb")
                .with_audit(Arc::new(boss_events::PgAuditWriter::new(pg_pool.clone())));
            info!(nats_url = %url, "domain event publishing + audit trail enabled");
            Some(pub_)
        }
        None => {
            info!("no nats_url configured — domain events will not be published");
            None
        }
    };

    // Class registry validation for the catalog's tenant-extensible
    // taxonomy codes — asset-model `category`, document `kind`, and
    // marketing-asset `kind`. Fail-loud (matching boss-people): the
    // URL is a required config field, so this is always wired —
    // app-layer validation is the only gate keeping a typo'd or
    // unregistered code out. The state field stays `Option` (tests
    // pass `None`); only the binary is fail-loud.
    let classes_client: Option<Arc<dyn ClassesClient>> = Some(Arc::new(ReqwestClassesClient::new(
        cfg.classes_api_url.clone(),
    )) as Arc<dyn ClassesClient>);
    info!(classes_api_url = %cfg.classes_api_url, "Class registry validation enabled");

    let clock_url =
        std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| "http://localhost:7060".to_string());
    let clock: Arc<dyn ClockClient> = Arc::new(ReqwestClockClient::new(clock_url.clone()));
    info!(%clock_url, "clock client wired");

    // Wire the sim-mode probe into the publisher so every
    // every stamp automatically injects `_simulated: bool` into
    // the audit_log payload without per-handler changes.
    let publisher = publisher.map(|p| {
        p.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
            clock.clone(),
        )))
    });

    // Share the same Class-registry handle with the marketing-assets
    // sub-router so its `kind` codes validate against
    // `subject_kind='marketing-asset'` exactly as the device-catalog
    // router validates categories + document kinds. Cloned because
    // `KbApiState` takes ownership below.
    let marketing_classes_client = classes_client.clone();

    let state = KbApiState {
        catalog,
        publisher,
        assets_client,
        classes_client,
        clock,
    };
    // Merge the Marketing Asset KB sub-router. Postgres-only: marketing_assets
    // is backed by Postgres via PgMarketingAssets, no in-memory
    // fallback since the InMemoryKb is device-catalog specific.
    let app = {
        use boss_catalog::marketing_assets::http::{
            MarketingAssetsApiState, router as marketing_assets_router,
        };
        router(state).merge(marketing_assets_router(MarketingAssetsApiState {
            pool: pg_pool.clone(),
            classes_client: marketing_classes_client,
        }))
    };
    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("catalog"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "catalog",
        "/api/catalog/actor-role-reports",
        Arc::new(boss_policy_client::ReqwestPolicyClient::new(
            "catalog",
            std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy")),
        )),
        roles.clone(),
        mode.clone(),
        Arc::new(boss_policy_client::role_reporting::ReportTally::new(
            boss_policy_client::role_service::REPORT_CAPACITY,
        )),
    );
    let app = catalog_http_router(app, wiring.inventory);
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
    info!(addr = %http_addr, "kb HTTP API listening");

    let app = boss_core::machine_gate::mount(
        app,
        "catalog",
        &["/api/catalog/health"],
        Some(boss_events::outbox::PgOutboxRecorder::shared(&pg_pool)),
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

#[cfg(test)]
mod tests {
    use boss_core::startup::mask_password;

    #[test]
    fn masks_password() {
        assert_eq!(
            mask_password("postgres://boss:secret@localhost/boss"),
            "postgres://boss:***@localhost/boss"
        );
    }

    #[test]
    fn no_password_unchanged() {
        assert_eq!(
            mask_password("postgres://localhost/boss"),
            "postgres://localhost/boss"
        );
    }
}

fn catalog_http_router(domain: axum::Router, inventory: axum::Router) -> axum::Router {
    domain.merge(inventory)
}

#[cfg(test)]
mod role_inventory_tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use boss_policy_client::{Action, FakePolicyClient, Resource, Scope, User};
    use tower::ServiceExt;

    #[tokio::test]
    async fn catalog_mount_preserves_domain_and_protects_inventory() {
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
            "catalog",
            "/api/catalog/actor-role-reports",
            policy,
            roles,
            Arc::new(boss_policy_client::role_reporting::ReportMode::Report),
            tally.clone(),
        );
        let app = catalog_http_router(
            router(KbApiState {
                catalog: Arc::new(boss_catalog::InMemoryKb::new(vec![])),
                publisher: None,
                assets_client: Arc::new(ReqwestAssetsClient::new("http://127.0.0.1:1")),
                classes_client: None,
                clock: Arc::new(boss_clock_client::WallClockClient),
            }),
            wiring.inventory,
        );
        let domain = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/catalog/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(domain.status(), StatusCode::OK);
        let health: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(domain.into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(health["status"], "ok");
        let models = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/catalog/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(models.into_body(), 4096)
                .await
                .unwrap(),
            "[]"
        );
        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/catalog/actor-role-reports")
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
                    .uri("/api/catalog/actor-role-reports")
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
        assert_eq!(body["service"], "catalog");
        assert_eq!(body["snapshot"]["state"], "never-loaded");
        assert_eq!(body["report"]["durable_window"], false);
        assert!(tally.snapshot().rows.is_empty());
    }
}
