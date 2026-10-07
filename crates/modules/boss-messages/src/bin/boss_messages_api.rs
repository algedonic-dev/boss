//! `boss-messages-api` service: direct messages and system signals backed by Postgres.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_classes_client::{ClassesClient, ReqwestClassesClient};
use boss_messages::http::{MessageApiState, router};
use boss_messages::messages_config::MessagesApiConfig;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-messages-api",
    about = "Boss Messages API service",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-messages-api.toml")]
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
    let cfg = MessagesApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-messages-api starting");

    // One pool per service. PgPool is internally Arc'd, so cloning is
    // cheap and every sub-router/audit-writer shares the same slots.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let messages = Arc::new(boss_messages::PgMessages::new(pool.clone()));

    // Connect to NATS for domain event publishing (optional).
    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            // Messages get their OWN immutable event log
            // (`messages_events`) instead of riding the
            // compliance-grade audit_log. That lets operators
            // expire old messages without breaking the rest of
            // the audit chain. See PgMessagesEventWriter docs.
            let pub_ = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "messages")
                .with_audit(std::sync::Arc::new(
                    boss_events::PgMessagesEventWriter::new(pool.clone()),
                ));
            info!(nats_url = %url, "domain event publishing + messages_events trail enabled");
            Some(pub_)
        }
        None => {
            info!("no nats_url configured — domain events will not be published");
            None
        }
    };

    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    // Wire the sim-mode probe into the publisher so every
    // every stamp automatically injects `_simulated: bool` into
    // the audit_log payload without per-handler changes.
    let publisher = publisher.map(|p| {
        p.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
            clock.clone(),
        )))
    });

    // Class registry validation for MessageKind under
    // subject_kind='message'. Required and fail-loud: the URL comes
    // from config (validated non-empty at startup), so the gate is
    // always wired in production — mirrors the boss-shipping carrier
    // wiring. The state field stays `Option` only so tests can pass
    // `None`; production always passes `Some`.
    let classes_client = Some(
        Arc::new(ReqwestClassesClient::new(cfg.classes_api_url.clone())) as Arc<dyn ClassesClient>,
    );
    info!(classes_url = %cfg.classes_api_url, "MessageKind validation enabled");

    let state = MessageApiState {
        messages,
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
        boss_policy_client::User::service("messages"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "messages",
        "/api/messages/actor-role-reports",
        Arc::new(boss_policy_client::ReqwestPolicyClient::new(
            "messages",
            std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy")),
        )),
        roles.clone(),
        mode.clone(),
        Arc::new(boss_policy_client::role_reporting::ReportTally::new(
            boss_policy_client::role_service::REPORT_CAPACITY,
        )),
    );
    let app = messages_http_router(router(state), wiring.inventory);
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
    info!(addr = %http_addr, "messages HTTP API listening");

    let app = boss_core::machine_gate::mount(
        app,
        "messages",
        &["/api/messages/health"],
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

fn messages_http_router(domain: axum::Router, inventory: axum::Router) -> axum::Router {
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
    async fn messages_mount_preserves_domain_and_protects_inventory() {
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
            "messages",
            "/api/messages/actor-role-reports",
            policy,
            roles,
            Arc::new(boss_policy_client::role_reporting::ReportMode::Report),
            tally.clone(),
        );
        let app = messages_http_router(
            router(MessageApiState {
                messages: Arc::new(boss_messages::InMemoryMessages::new(vec![])),
                publisher: None,
                clock: Arc::new(boss_clock_client::WallClockClient),
                classes_client: None,
            }),
            wiring.inventory,
        );
        let domain = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/messages/health")
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
        let mut unrelated = User::service("unrelated");
        unrelated.role = "platform-admin".into();
        unrelated.access_tier = boss_policy_client::AccessTier::User;
        let inbox = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/messages/inbox/other")
                    .header("x-boss-user", serde_json::to_string(&unrelated).unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(inbox.status(), StatusCode::FORBIDDEN);
        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/messages/actor-role-reports")
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
                    .uri("/api/messages/actor-role-reports")
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
        assert_eq!(body["service"], "messages");
        assert_eq!(body["snapshot"]["state"], "never-loaded");
        assert_eq!(body["report"]["durable_window"], false);
        assert!(tally.snapshot().rows.is_empty());
    }
}
