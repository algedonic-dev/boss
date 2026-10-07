//! `boss-ml-api` service entry point.
//!
//! Wires the `boss-ml` library + the canonical inference plugin
//! set (`boss-ml-plugins`) into a runnable axum server. Tenant
//! deployments that need additional plugins ship their own
//! `boss-ml-api` binary that adds extra `register_plugin` calls
//! before the dispatcher is wrapped in `Arc`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

use boss_ml::bootstrap::seed_phase_two_candidates;
use boss_ml::config::MlApiConfig;
use boss_ml::http::{MlApiState, router};
use boss_ml::port::MlRepository;
use boss_ml::{InferenceDispatcher, PgMlRepo};
use boss_ml_plugins::AccountChurnRiskV1;

#[derive(Parser, Debug)]
#[command(name = "boss-ml-api", about = "Boss ML Platform API", version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-ml-api.toml")]
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
    let cfg = MlApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        postgres_url = %boss_core::startup::mask_password(&cfg.postgres_url),
        http_bind = %cfg.http_bind,
        "boss-ml-api starting"
    );

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;
    let repo: Arc<dyn MlRepository> = Arc::new(PgMlRepo::new(pool.clone()));

    // Route every "today" lookup in declarative rules + plugin
    // paths through ClockClient so sim-mode inferences see sim-today.
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    let mut dispatcher = InferenceDispatcher::new(pool.clone(), repo.clone(), clock);
    // Canonical plugin set. Tenant binaries can layer their own
    // plugins here before the dispatcher is wrapped in Arc below.
    dispatcher.register_plugin(Arc::new(AccountChurnRiskV1::new()));
    info!(
        plugins = ?dispatcher.plugin_names(),
        "inference plugins registered"
    );
    let dispatcher = Some(Arc::new(dispatcher));

    // Bootstrap: upsert the candidate models from embedded seeds.
    // Idempotent across restarts (docs/architecture-decisions.md
    // §ML platform).
    match seed_phase_two_candidates(repo.as_ref()).await {
        Ok(n) => info!(count = n, "bootstrap seed complete"),
        Err(e) => tracing::warn!(error = %e, "bootstrap seed failed — continuing anyway"),
    }

    let state = MlApiState { repo, dispatcher };
    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("ml"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "ml",
        "/api/ml/actor-role-reports",
        Arc::new(boss_policy_client::ReqwestPolicyClient::new(
            "ml",
            std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy")),
        )),
        roles.clone(),
        mode.clone(),
        Arc::new(boss_policy_client::role_reporting::ReportTally::new(
            boss_policy_client::role_service::REPORT_CAPACITY,
        )),
    );
    let app = ml_http_router(router(state), wiring.inventory);

    let http_addr: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "boss-ml-api listening");
    let app = boss_core::machine_gate::mount(
        app,
        "ml",
        &["/api/ml/health"],
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

fn ml_http_router(domain: axum::Router, inventory: axum::Router) -> axum::Router {
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
    async fn ml_mount_preserves_domain_and_protects_inventory() {
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
            "ml",
            "/api/ml/actor-role-reports",
            policy,
            roles,
            Arc::new(boss_policy_client::role_reporting::ReportMode::Report),
            tally.clone(),
        );
        let app = ml_http_router(
            router(MlApiState {
                repo: Arc::new(boss_ml::InMemoryMlRepo::new()),
                dispatcher: None,
            }),
            wiring.inventory,
        );
        let domain = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/ml/health")
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
        for path in [
            "/api/ml/models/mdl-test/infer/entity-test",
            "/api/ml/models/mdl-test/infer-batch",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap(),
                "inference dispatcher not wired"
            );
        }
        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/ml/actor-role-reports")
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
                    .uri("/api/ml/actor-role-reports")
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
        assert_eq!(body["service"], "ml");
        assert_eq!(body["snapshot"]["state"], "never-loaded");
        assert_eq!(body["report"]["durable_window"], false);
        assert!(tally.snapshot().rows.is_empty());
    }
}
