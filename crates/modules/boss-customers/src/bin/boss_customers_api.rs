//! `boss-customers-api` — HTTP service for DTC customers.
//!
//! No NATS publisher: the create path stages its event on the
//! transactional outbox (#118) inside the same transaction as the
//! domain write; boss-event-relay moves it to audit_log + NATS.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_customers::http::{CustomersApiState, router};
use boss_customers::postgres::PgCustomers;
use clap::Parser;
use serde::Deserialize;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-customers-api", about = "Boss Customers API", version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-customers-api.toml")]
    config: PathBuf,
}

#[derive(Deserialize)]
struct Config {
    http_bind: String,
    postgres_url: String,
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
    let cfg: Config = toml::from_str(
        &std::fs::read_to_string(&cli.config)
            .with_context(|| format!("reading config {}", cli.config.display()))?,
    )
    .with_context(|| format!("parsing config {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-customers-api starting");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .context("connecting to Postgres")?;

    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    let state = CustomersApiState {
        customers: Arc::new(PgCustomers::new(pool.clone())),
        clock,
    };

    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("customers"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "customers",
        "/api/customers/actor-role-reports",
        Arc::new(boss_policy_client::ReqwestPolicyClient::new(
            "customers",
            std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy")),
        )),
        roles.clone(),
        mode.clone(),
        boss_events::role_tally::durable("customers", mode.clone(), Some(&pool)),
    );
    let app = customers_http_router(router(state), wiring.inventory);

    let http_addr: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "boss-customers-api listening");
    let app = boss_core::machine_gate::mount(
        app,
        "customers",
        &["/api/customers/health"],
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

fn customers_http_router(domain: axum::Router, inventory: axum::Router) -> axum::Router {
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
    async fn customers_mount_preserves_domain_and_protects_inventory() {
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
            "customers",
            "/api/customers/actor-role-reports",
            policy,
            roles,
            Arc::new(boss_policy_client::role_reporting::ReportMode::Report),
            tally.clone(),
        );
        let app = customers_http_router(
            router(CustomersApiState {
                customers: Arc::new(boss_customers::InMemoryCustomers::new()),
                clock: Arc::new(boss_clock_client::WallClockClient),
            }),
            wiring.inventory,
        );
        let domain = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/customers/health")
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
            r#"{"status":"ok"}"#
        );
        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/customers/actor-role-reports")
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
                    .uri("/api/customers/actor-role-reports")
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
        assert_eq!(body["service"], "customers");
        assert_eq!(body["snapshot"]["state"], "never-loaded");
        assert_eq!(body["report"]["durable_window"], false);
        assert!(tally.snapshot().rows.is_empty());
    }
}
