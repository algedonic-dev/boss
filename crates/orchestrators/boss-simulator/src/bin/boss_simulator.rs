//! boss-simulator — hosts the Simulator UX.
//!
//! Serves the `apps/simulator` SPA under `/simulator` (the gateway
//! reverse-proxies `/simulator/*` here WITHOUT stripping the prefix, so
//! we nest the whole sub-app under `/simulator`) plus the
//! `/simulator/api/*` control surface.
//!
//! This service is the single owner of sim CONTROL: the sim control plane
//! has no public path (clock-api isn't gateway-proxied; "configure
//! epoch/warp" has no other endpoint), so the controls live here behind
//! an operator gate, forwarding server-to-server to clock-api +
//! jobs-api's sim-clock endpoints. Read-only cockpit data (live jobs,
//! events) rides the existing gateway `/api/*` proxies, so it is NOT
//! re-hosted here.

use std::{net::SocketAddr, path::Path, sync::Arc};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use boss_policy_client::{CurrentUser, request_context_middleware};
use reqwest::Client;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

struct AppState {
    http: Client,
    role_guards: Arc<boss_policy_client::role_guard::RoleGuardReporter>,
    jobs_url: String,
    clock_url: String,
    /// The brewery-sim DAEMON's localhost control+telemetry server
    /// (boss-ports `sim-control`). Read endpoints (telemetry, config GET)
    /// are open; config writes are operator-gated + forwarded here.
    sim_control_url: String,
}

const STUB_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>BOSS Simulator</title></head>\
<body style=\"font-family:system-ui;padding:40px;color:#1c1917\">\
<h1>BOSS Simulator</h1>\
<p>The simulator UI bundle is not installed yet. Build <code>apps/simulator</code> \
and deploy it to <code>BOSS_SIM_STATIC_DIR</code>.</p></body></html>";

/// Sim controls mutate the shared clock + trim audit_log, so they're for
/// signed-in operators only — never anonymous visitors (whose gateway
/// session carries a read-only-floor role: `visitor`, or `audit-readonly`
/// where the instance opts in — design 2830b6b7) or the headerless
/// `guest` fallback. Mirrors jobs-api's `operator_guard`, and asks the
/// same `boss_core::roles::is_read_only_floor` rather than a role name.
#[cfg(test)]
fn operator_guard(user: &CurrentUser) -> Option<Response> {
    operator_guard_with_reports(user, None)
}

fn operator_guard_with_reports(
    user: &CurrentUser,
    reporter: Option<&boss_policy_client::role_guard::RoleGuardReporter>,
) -> Option<Response> {
    let role = user.0.role.as_str();
    let original = !(boss_core::roles::is_read_only_floor(role) || role == "guest");
    let allowed = reporter.map_or(original, |reporter| {
        reporter.observe_captured(
            "simulator-control",
            "admission",
            &user.0,
            original,
            |candidate| {
                Some(
                    !(boss_core::roles::is_read_only_floor(&candidate.role)
                        || candidate.role == "guest"),
                )
            },
        )
    });
    if !allowed {
        return Some(
            (
                StatusCode::FORBIDDEN,
                "sim controls require a signed-in operator",
            )
                .into_response(),
        );
    }
    None
}

/// Forward an operator-gated POST to an upstream control endpoint,
/// re-attaching the caller's `x-boss-user` so the upstream's own operator
/// gate (jobs-api) also passes, and relaying the upstream status + body.
async fn forward_post(
    state: &AppState,
    url: String,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Response {
    let mut req = state.http.post(&url);
    if let Some(u) = headers.get("x-boss-user") {
        req = req.header("x-boss-user", u);
    }
    if let Some(b) = body {
        req = req
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(b);
    }
    match req.send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                status,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response(),
    }
}

/// Forward a plain read-only GET to a localhost upstream — the daemon's
/// control server (telemetry / config) or clock-api (clock state) — and
/// relay the JSON. No operator gate and no actor header needed; these
/// upstreams are localhost-only.
async fn forward_get(state: &AppState, url: String) -> Response {
    match state.http.get(&url).send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                status,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("upstream unreachable: {e}"),
        )
            .into_response(),
    }
}

/// GET /simulator/api/telemetry — how the daemon is engaging the public
/// API right now (Cockpit). Open read; proxies the daemon's /telemetry.
async fn get_telemetry(State(s): State<Arc<AppState>>) -> Response {
    forward_get(&s, format!("{}/telemetry", s.sim_control_url)).await
}

/// GET /simulator/api/clock — the authoritative clock state (sim time,
/// warp, paused) straight from clock-api, which owns it. The cockpit reads
/// its clock readouts here rather than off the daemon's /telemetry, so they
/// stay correct even while the daemon is stopped (e.g. mid seed-rebuild,
/// when its /telemetry is down). clock-api isn't gateway-proxied, so this is
/// the cockpit's only path to it. Open read.
async fn get_clock(State(s): State<Arc<AppState>>) -> Response {
    forward_get(&s, format!("{}/api/clock/now", s.clock_url)).await
}

/// GET /simulator/api/config — the daemon's effective behavior config
/// (the Controls editor loads this). Open read.
async fn get_config(State(s): State<Arc<AppState>>) -> Response {
    forward_get(&s, format!("{}/config", s.sim_control_url)).await
}

/// POST /simulator/api/config — operator-gated. Persists an operator-
/// edited behavior config to the daemon, which validates it, writes the
/// override, and restarts to apply it (the "edit + restart" model).
async fn post_config(
    State(s): State<Arc<AppState>>,
    user: CurrentUser,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = operator_guard_with_reports(&user, Some(&s.role_guards)) {
        return r;
    }
    forward_post(
        &s,
        format!("{}/config", s.sim_control_url),
        &headers,
        Some(body),
    )
    .await
}

async fn control_pause(
    State(s): State<Arc<AppState>>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = operator_guard_with_reports(&user, Some(&s.role_guards)) {
        return r;
    }
    forward_post(
        &s,
        format!("{}/api/jobs/sim-clock/pause", s.jobs_url),
        &headers,
        None,
    )
    .await
}

async fn control_resume(
    State(s): State<Arc<AppState>>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = operator_guard_with_reports(&user, Some(&s.role_guards)) {
        return r;
    }
    forward_post(
        &s,
        format!("{}/api/jobs/sim-clock/resume", s.jobs_url),
        &headers,
        None,
    )
    .await
}

async fn control_restart_epoch(
    State(s): State<Arc<AppState>>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = operator_guard_with_reports(&user, Some(&s.role_guards)) {
        return r;
    }
    forward_post(
        &s,
        format!("{}/api/jobs/sim-clock/restart-epoch", s.jobs_url),
        &headers,
        None,
    )
    .await
}

async fn control_configure(
    State(s): State<Arc<AppState>>,
    user: CurrentUser,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = operator_guard_with_reports(&user, Some(&s.role_guards)) {
        return r;
    }
    // The new capability with no public path: epoch_start / epoch_end /
    // warp_factor. clock-api validates the body; we just proxy it.
    forward_post(
        &s,
        format!("{}/api/clock/configure", s.clock_url),
        &headers,
        Some(body),
    )
    .await
}

fn simulator_http_router(sim: Router, inventory: Router) -> Router {
    Router::new()
        .nest("/simulator", sim)
        .merge(inventory)
        .layer(axum::middleware::from_fn(request_context_middleware))
        .layer(TraceLayer::new_for_http())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "boss-simulator" }))
}

async fn stub() -> Html<&'static str> {
    Html(STUB_HTML)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    // Loopback default: reached through the gateway's /simulator
    // proxy, never directly (SECURITY.md §Deployment trust model).
    // BOSS_SIM_BIND widens deliberately.
    let bind = std::env::var("BOSS_SIM_BIND")
        .unwrap_or_else(|_| format!("127.0.0.1:{}", boss_ports::prod("simulator")));
    let static_dir = std::env::var("BOSS_SIM_STATIC_DIR")
        .unwrap_or_else(|_| "/var/lib/boss-simulator/dist".to_string());

    let jobs_url = std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs"));
    let roles = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let mode = Arc::new(boss_policy_client::role_reader::MountedReportMode::mount());
    let source = Arc::new(boss_policy_client::role_reader::HttpRoleReader::new(
        jobs_url.clone(),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("simulator"),
    )?);
    let wiring = boss_policy_client::role_service::assemble(
        "simulator",
        "/simulator/api/actor-role-reports",
        Arc::new(boss_policy_client::ReqwestPolicyClient::new(
            "simulator",
            std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy")),
        )),
        roles.clone(),
        mode.clone(),
        Arc::new(boss_policy_client::role_reporting::ReportTally::new(
            boss_policy_client::role_service::REPORT_CAPACITY,
        )),
    );

    let state = Arc::new(AppState {
        http: Client::new(),
        role_guards: wiring.guards,
        jobs_url,
        clock_url: std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock")),
        sim_control_url: std::env::var("BOSS_SIM_CONTROL_URL")
            .unwrap_or_else(|_| boss_ports::url("sim-control")),
    });

    // The /simulator sub-app: control API routes + the SPA (or a stub
    // until apps/simulator is built). Nested under /simulator because the
    // gateway forwards that prefix unchanged.
    let api = Router::new()
        .route("/api/health", get(health))
        .route("/api/telemetry", get(get_telemetry))
        .route("/api/clock", get(get_clock))
        .route("/api/config", get(get_config).post(post_config))
        .route("/api/control/pause", post(control_pause))
        .route("/api/control/resume", post(control_resume))
        .route("/api/control/restart-epoch", post(control_restart_epoch))
        .route("/api/control/configure", post(control_configure))
        .with_state(state);

    let index = format!("{}/index.html", static_dir.trim_end_matches('/'));
    let sim = if Path::new(&index).exists() {
        let serve = ServeDir::new(&static_dir).fallback(ServeFile::new(index));
        api.fallback_service(serve)
    } else {
        info!(dir = %static_dir, "no SPA bundle found — serving stub page");
        api.fallback(stub)
    };

    let app = simulator_http_router(sim, wiring.inventory);

    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid bind `{bind}`"))?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding HTTP listener on {addr}"))?;
    info!(addr = %addr, static_dir = %static_dir, "boss-simulator listening");
    // No database here, so no outbox: this gate's facts reach no log,
    // and a clean window that names `simulator` is never clean (design
    // 21946380) — said at WARN by the mount.
    let app = boss_core::machine_gate::mount(app, "simulator", &["/simulator/api/health"], None);
    boss_policy_client::role_service::serve_with_refresh(
        listener,
        app,
        roles,
        source,
        mode,
        boss_policy_client::role_service::REFRESH_CADENCE,
        boss_policy_client::role_service::shutdown_signal(),
    )
    .await
    .context("serving HTTP")?;
    Ok(())
}

use tokio::net::TcpListener;

#[cfg(test)]
mod tests {
    use super::*;
    use boss_policy_client::{AccessTier, User};

    fn as_role(role: &str) -> CurrentUser {
        CurrentUser(User {
            id: "x".into(),
            role: role.into(),
            access_tier: AccessTier::User,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        })
    }

    /// Design 2830b6b7: the same floor as jobs-api's guard. The OSS
    /// guest's `visitor` is refused; keyed on the name `audit-readonly`
    /// it would have driven the engine.
    #[test]
    fn every_read_only_floor_role_and_the_headerless_guest_are_refused() {
        for role in boss_core::roles::READ_ONLY_FLOOR_ROLES
            .into_iter()
            .chain(["guest"])
        {
            let refused = operator_guard(&as_role(role));
            assert_eq!(
                refused.map(|r| r.status()),
                Some(StatusCode::FORBIDDEN),
                "{role} must not drive the simulator"
            );
        }
        assert!(operator_guard(&as_role(boss_core::roles::VISITOR_ROLE)).is_some());
        assert!(operator_guard(&as_role("platform-admin")).is_none());
    }
    #[test]
    fn reporting_keeps_the_original_floor_and_signed_in_control_decisions() {
        use boss_policy_client::role_guard::RoleGuardReporter;
        use boss_policy_client::role_reader::{
            MonotonicRoleSnapshotClock, RegistryRoles, SnapshotRoleReader,
        };
        use boss_policy_client::role_reporting::{ReportMode, ReportTally};
        let roles = Arc::new(SnapshotRoleReader::new(
            std::time::Duration::from_secs(30),
            Arc::new(MonotonicRoleSnapshotClock),
        ));
        let ticket = roles.begin_refresh();
        assert!(roles.finish_refresh(ticket, Ok(RegistryRoles::from_sources(
            serde_json::json!({"data":[{"id":"x","aliases":[],"role":"platform-admin"}],"total":1}),
            serde_json::json!({"data":[],"total":0}), serde_json::json!([])).unwrap())));
        let tally = Arc::new(ReportTally::new(8));
        let reporter = RoleGuardReporter::new(roles, tally.clone(), Arc::new(ReportMode::Report));
        for role in ["visitor", "guest", "platform-admin"] {
            let original = operator_guard(&as_role(role)).map(|response| response.status());
            let reported = operator_guard_with_reports(&as_role(role), Some(&reporter))
                .map(|response| response.status());
            assert_eq!(reported, original);
        }
        let report = tally.snapshot();
        assert_eq!(report.rows.len(), 3);
        assert_eq!(
            report
                .rows
                .iter()
                .filter(|row| row.observation.asserted_allowed == Some(false)
                    && row.observation.recorded_allowed == Some(true))
                .count(),
            2
        );
    }
    #[tokio::test]
    async fn simulator_inventory_is_mounted_once_at_its_public_prefix() {
        let inventory = Router::new().route(
            "/simulator/api/actor-role-reports",
            get(|| async { "fixture-report" }),
        );
        let app = simulator_http_router(Router::new(), inventory);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/simulator/api/actor-role-reports",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let response = Client::new().get(url).send().await.unwrap();
        server.abort();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.text().await.unwrap(), "fixture-report");
    }
    #[tokio::test]
    async fn report_mode_preserves_all_simulator_forwards_and_refused_zero_effects() {
        use boss_policy_client::role_guard::RoleGuardReporter;
        use boss_policy_client::role_reader::{
            MonotonicRoleSnapshotClock, RegistryRoles, SnapshotRoleReader,
        };
        use boss_policy_client::role_reporting::{ReportMode, ReportTally};
        use std::sync::atomic::{AtomicUsize, Ordering};
        for (asserted, recorded, original_allowed) in [
            ("visitor", "platform-admin", false),
            ("platform-admin", "visitor", true),
        ] {
            let user = as_role(asserted);
            let identity = serde_json::to_string(&user.0).unwrap();
            let roles = Arc::new(SnapshotRoleReader::new(
                std::time::Duration::from_secs(30),
                Arc::new(MonotonicRoleSnapshotClock),
            ));
            let ticket = roles.begin_refresh();
            assert!(roles.finish_refresh(ticket,Ok(RegistryRoles::from_sources(
                serde_json::json!({"data":[{"id":"x","aliases":[],"role":recorded}],"total":1}),
                serde_json::json!({"data":[],"total":0}),serde_json::json!([])).unwrap())));
            let tally = Arc::new(ReportTally::new(8));
            let reporter = Arc::new(RoleGuardReporter::new(
                roles,
                tally.clone(),
                Arc::new(ReportMode::Report),
            ));
            let calls = Arc::new(AtomicUsize::new(0));
            let counted = calls.clone();
            let expected = identity.clone();
            let upstream = Router::new().route(
                "/{*path}",
                post(move |headers: HeaderMap, body: Bytes| {
                    let counted = counted.clone();
                    let expected = expected.clone();
                    async move {
                        assert_eq!(
                            headers.get("x-boss-user").unwrap().to_str().unwrap(),
                            expected
                        );
                        assert!(body.is_empty() || body.as_ref() == b"{\"warp\":2}");
                        counted.fetch_add(1, Ordering::SeqCst);
                        (StatusCode::ACCEPTED, "{\"forwarded\":true}")
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(listener, upstream).await.unwrap();
            });
            let state = Arc::new(AppState {
                http: Client::new(),
                role_guards: reporter,
                jobs_url: base.clone(),
                clock_url: base.clone(),
                sim_control_url: base,
            });
            let mut headers = HeaderMap::new();
            headers.insert("x-boss-user", identity.parse().unwrap());
            for control in 0..5 {
                let user = as_role(asserted);
                let body = Bytes::from_static(b"{\"warp\":2}");
                let response = match control {
                    0 => post_config(State(state.clone()), user, headers.clone(), body).await,
                    1 => control_pause(State(state.clone()), user, headers.clone()).await,
                    2 => control_resume(State(state.clone()), user, headers.clone()).await,
                    3 => control_restart_epoch(State(state.clone()), user, headers.clone()).await,
                    _ => control_configure(State(state.clone()), user, headers.clone(), body).await,
                };
                assert_eq!(
                    response.status(),
                    if original_allowed {
                        StatusCode::ACCEPTED
                    } else {
                        StatusCode::FORBIDDEN
                    }
                );
                let body = axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap();
                assert_eq!(
                    body.as_ref(),
                    if original_allowed {
                        b"{\"forwarded\":true}".as_slice()
                    } else {
                        b"sim controls require a signed-in operator".as_slice()
                    }
                );
            }
            server.abort();
            assert_eq!(
                calls.load(Ordering::SeqCst),
                if original_allowed { 5 } else { 0 }
            );
            let report = tally.snapshot();
            assert!(!report.rows.is_empty());
            assert_eq!(report.rows.iter().map(|row| row.count).sum::<u64>(), 5);
            for row in report.rows {
                assert_eq!(row.observation.asserted_allowed, Some(original_allowed));
                assert_eq!(row.observation.recorded_allowed, Some(!original_allowed));
            }
        }
    }
}
