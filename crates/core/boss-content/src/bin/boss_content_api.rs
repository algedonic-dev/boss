//! `boss-content-api` — HR content service. Bulletins today; manual in v1c.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::http::StatusCode;
use axum::routing::any;
use boss_content::config::ContentApiConfig;
use boss_content::http::{ContentApiState, router as content_router};
use boss_content::{ContentRepository, PgContent};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-content-api", about = "Boss HR Content API", version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-content-api.toml")]
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
    let cfg = ContentApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        postgres_url = %boss_core::startup::mask_password(&cfg.postgres_url),
        http_bind = %cfg.http_bind,
        "boss-content-api starting"
    );

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let repo: Arc<dyn ContentRepository> = Arc::new(PgContent::new(pool.clone()));

    // Seed starter manual sections — idempotent, only inserts what's
    // missing. Lets HR edit the outline on day one instead of an empty
    // tree.
    match boss_content::seed::seed_starter_sections(repo.as_ref()).await {
        Ok(0) => info!("manual: no seeding needed"),
        Ok(n) => info!(inserted = n, "manual: seeded starter sections"),
        Err(e) => tracing::warn!(error = %e, "manual: seeding failed"),
    }

    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            let pub_ = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "content")
                .with_audit(Arc::new(boss_events::PgAuditWriter::new(pool.clone())));
            info!(nats_url = %url, "domain event publishing + audit trail enabled");
            Some(pub_)
        }
        None => {
            info!("no nats_url configured — content events will not be published");
            None
        }
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

    let policy: Arc<dyn boss_policy_client::PolicyClient> = if cfg.files.is_some() {
        files_policy(cfg.policy_api_url.as_deref())
    } else {
        Arc::new(boss_policy_client::PermissivePolicyClient)
    };
    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("content"),
    )?);
    let wiring = content_role_wiring(
        policy,
        Arc::new(boss_policy_client::ReqwestPolicyClient::new(
            "content",
            cfg.policy_api_url.clone().unwrap_or_else(|| {
                std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy"))
            }),
        )),
        roles.clone(),
        mode.clone(),
        boss_events::role_tally::durable("content", mode.clone(), Some(&pool)),
    );
    let repo: Arc<dyn ContentRepository> = Arc::new(
        PgContent::new(pool.clone()).with_audience_observer(Arc::new(
            boss_content::role_reports::RoleAudienceObserver::new(wiring.guards.clone()),
        )),
    );
    let policy = wiring.policy;

    let state = ContentApiState {
        repo: repo.clone(),
        publisher: publisher.clone(),
        clock: clock.clone(),
    };
    let mut app = content_router(state).merge(wiring.inventory);

    // File-references surface — mounted only when the config wires
    // a bucket. Keeps the binary boot path simple for deployments
    // that haven't set up object storage yet. When unconfigured,
    // mount a fallback that returns 503 with a clear body so the
    // SPA can render an honest "not available in this deployment"
    // message instead of a generic 404 (which reads as broken).
    if let Some(files_cfg) = &cfg.files {
        let files_app = build_files_router(
            policy,
            files_cfg,
            pool.clone(),
            publisher.clone(),
            clock.clone(),
        )
        .await
        .with_context(|| "wiring file-references HTTP surface")?;
        app = app.merge(files_app);
        info!(root = %files_cfg.root.display(), "file-references surface enabled");
    } else {
        info!("file-references surface not configured (no [files] block); mounting 503 fallback");
        app = app.merge(unconfigured_files_router());
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
    info!(addr = %http_addr, "boss-content-api listening");
    let app = boss_core::machine_gate::mount(
        app,
        "content",
        &["/api/content/health"],
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

/// Mounted when the `[files]` config block is absent. Returns 503
/// on every `/api/files*` path so the SPA can render an honest
/// "not available in this deployment" message — a 404 reads to
/// operators like a broken endpoint, but the surface is just
/// intentionally unconfigured.
fn unconfigured_files_router() -> Router {
    // Return 200 with an `{kind: "unconfigured"}` envelope rather than
    // 503. The 503 was visible to auditor-role browsing sessions as a
    // genuine error in the network tab, even though the surface is
    // designed-off, not broken. SPA's `listFilesFor()` detects the
    // envelope and renders the same "not available in this deployment"
    // callout as before; the difference is the response is now a
    // 200 OK from the auditor's vantage point.
    //
    // The body is read to its end BEFORE answering (backlog 1fe351e8,
    // 2026-09-23). Answering first let hyper close a connection whose
    // upload was still arriving, so `boss attach` of a file larger than
    // one socket buffer saw Broken pipe instead of this refusal — 40 of
    // 40 runs of the 16 MiB test below. It is streamed and discarded
    // rather than taken as `Bytes`: that extractor stops at axum's 2 MB
    // DefaultBodyLimit and answers 413, which would change the status
    // and still leave the rest of the upload unread.
    let handler = any(|body: axum::body::Body| async move {
        drain(body).await;
        (
            StatusCode::OK,
            [("content-type", "application/json")],
            r#"{"kind":"unconfigured","reason":"file-references surface not configured (no [files] block in boss-content-api config); see infra/oss-quickstart/generate-configs.sh"}"#,
        )
    });
    Router::new()
        .route("/api/files", handler.clone())
        .route("/api/files/{*rest}", handler)
}

/// Read a request body to its end, keeping none of it. A body that
/// errors (the client went away) ends the read; the answer that
/// follows then has nowhere to go, which is the client's choice.
async fn drain(mut body: axum::body::Body) {
    use axum::body::HttpBody;
    while let Some(Ok(_)) =
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
    {}
}

fn content_role_wiring(
    files_policy: Arc<dyn boss_policy_client::PolicyClient>,
    report_authorizer: Arc<dyn boss_policy_client::PolicyClient>,
    roles: Arc<boss_policy_client::role_reader::SnapshotRoleReader>,
    mode: Arc<dyn boss_policy_client::role_reporting::ReportModeSource>,
    tally: Arc<boss_policy_client::role_reporting::ReportTally>,
) -> boss_policy_client::role_service::RoleReportWiring {
    let wiring = boss_policy_client::role_service::assemble(
        "content",
        "/api/content/actor-role-reports",
        files_policy,
        roles.clone(),
        mode.clone(),
        tally.clone(),
    );
    // The optional file client preserves its existing permissive fallback.
    // Inventory always asks its own enforcing, undecorated authorizer.
    boss_policy_client::role_service::RoleReportWiring {
        inventory: boss_policy_client::role_inventory::router(
            "content",
            "/api/content/actor-role-reports",
            report_authorizer,
            roles,
            mode,
            tally,
        ),
        ..wiring
    }
}

fn files_policy(url: Option<&str>) -> Arc<dyn boss_policy_client::PolicyClient> {
    match url {
        Some(url) => Arc::new(boss_policy_client::ReqwestPolicyClient::new(
            "content",
            url.to_owned(),
        )),
        None => {
            tracing::warn!(
                "no policy_api_url configured — file-references operate without policy enforcement \
                 (gateway cookie auth only). Set policy_api_url in /etc/boss-content-api.toml \
                 to gate uploads/downloads by role."
            );
            Arc::new(boss_policy_client::PermissivePolicyClient)
        }
    }
}

async fn build_files_router(
    policy: Arc<dyn boss_policy_client::PolicyClient>,
    files_cfg: &boss_content::config::FilesConfig,
    pool: sqlx::PgPool,
    publisher: Option<boss_core::publisher::DomainPublisher>,
    clock: Arc<dyn boss_clock_client::ClockClient>,
) -> Result<axum::Router> {
    use boss_content::files::{
        FileRepository, FileStorage, LocalDiskStorage, PgFileRepository,
        http::{FilesApiState, router as files_router_fn},
    };
    use std::sync::Arc;

    let repo: Arc<dyn FileRepository> = Arc::new(PgFileRepository::new(pool.clone()));
    let storage: Arc<dyn FileStorage> = Arc::new(
        LocalDiskStorage::new(&files_cfg.root)
            .await
            .with_context(|| format!("opening file storage root {}", files_cfg.root.display()))?,
    );

    let state = FilesApiState {
        repo,
        storage,
        publisher,
        policy,
        bucket: files_cfg.root.display().to_string(),
        pool: Some(pool),
        clock,
    };
    Ok(files_router_fn(state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn optional_file_policy_cannot_authorize_the_role_inventory() {
        use boss_policy_client::{Action, FakePolicyClient, Resource, Scope, User};
        use tower::ServiceExt;
        let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
            boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
            Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
        ));
        let tally = Arc::new(boss_policy_client::role_reporting::ReportTally::new(8));
        let report_authorizer = Arc::new(
            FakePolicyClient::builder()
                .allow(
                    "report-reader",
                    Action::Read,
                    Resource::policy_rule(),
                    Scope::All,
                )
                .build(),
        );
        let wiring = content_role_wiring(
            files_policy(None),
            report_authorizer,
            roles,
            Arc::new(boss_policy_client::role_reporting::ReportMode::Report),
            tally,
        );
        assert!(
            wiring
                .policy
                .check(
                    &User::service("file-reader"),
                    Action::Read,
                    Resource::class()
                )
                .await
                .unwrap()
                .is_allowed()
        );
        let denied = wiring
            .inventory
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/content/actor-role-reports")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let mut user = User::service("report-reader");
        user.role = "report-reader".into();
        let allowed = wiring
            .inventory
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/content/actor-role-reports")
                    .header("x-boss-user", serde_json::to_string(&user).unwrap())
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn optional_files_policy_preserves_configured_outage_and_unconfigured_allow() {
        use boss_policy_client::{Action, PolicyClientError, Resource, User};
        let user = User::service("content-fixture");
        assert!(
            files_policy(None)
                .check(&user, Action::Read, Resource::class())
                .await
                .unwrap()
                .is_allowed()
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/api/policy/check",
            axum::routing::post(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let result = files_policy(Some(&url))
            .check(&user, Action::Read, Resource::class())
            .await;
        assert!(
            matches!(result, Err(PolicyClientError::Unreachable(_))),
            "configured policy outage must not become permissive: {result:?}"
        );
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    /// Backlog 1fe351e8 (found by the builder of cef615f6, 2026-09-23):
    /// the switched-off store answered before reading the upload, so a
    /// `boss attach` whose file is larger than one socket buffer could
    /// see Broken pipe instead of the named refusal. Sixteen MiB is far
    /// past any socket buffer the kernel grants, so the client is still
    /// writing when an early answer goes out — the race is forced, not
    /// sampled.
    #[tokio::test]
    async fn an_upload_larger_than_a_socket_buffer_gets_the_named_refusal() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, unconfigured_files_router())
                .await
                .unwrap();
        });

        const BODY_LEN: usize = 16 * 1024 * 1024;
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut rd, mut wr) = stream.into_split();
        let writer = tokio::spawn(async move {
            let head = format!(
                "POST /api/files HTTP/1.1\r\nhost: {addr}\r\n\
                 content-type: application/octet-stream\r\n\
                 content-length: {BODY_LEN}\r\n\r\n"
            );
            wr.write_all(head.as_bytes()).await?;
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..BODY_LEN / chunk.len() {
                wr.write_all(&chunk).await?;
            }
            wr.flush().await?;
            Ok::<_, std::io::Error>(wr)
        });
        let wrote = writer.await.unwrap();
        assert!(
            wrote.is_ok(),
            "the upload was cut off mid-body: {:?}",
            wrote.err()
        );

        let mut answer = Vec::new();
        let mut chunk = [0u8; 4096];
        while !String::from_utf8_lossy(&answer).contains("\"kind\":\"unconfigured\"") {
            match rd.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => answer.extend_from_slice(&chunk[..n]),
                Err(e) => panic!("reading the answer failed: {e}"),
            }
        }
        let answer = String::from_utf8_lossy(&answer);
        assert!(
            answer.starts_with("HTTP/1.1 200"),
            "status changed: {answer}"
        );
        assert!(
            answer.contains(
                r#"{"kind":"unconfigured","reason":"file-references surface not configured"#
            ),
            "the named refusal did not arrive: {answer}"
        );
    }
}
