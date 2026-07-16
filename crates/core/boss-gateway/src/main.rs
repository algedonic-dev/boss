use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::State;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

mod api;
mod demo_session;
mod perf;
mod plugin_files;
mod proxy;
mod role_headers;
mod static_files;
mod timing;

use perf::PerfCollector;

use boss_gateway::local_auth::{self, CredentialStore, LocalAuthState};

/// Auth provider — picks which middleware mints the
/// `boss_session` cookie.
///
/// - `local-auth` (default): file-backed email/password.
///   Login routes are mounted under `/api/auth/*`.
/// - `none`: bypass — no auth provider mounted. Test only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthProvider {
    LocalAuth,
    None,
}

impl AuthProvider {
    fn from_env() -> Self {
        match std::env::var("BOSS_AUTH_PROVIDER").as_deref() {
            Ok("none") => Self::None,
            Ok("local-auth") | Ok("") | Err(_) => Self::LocalAuth,
            Ok(other) => {
                tracing::warn!(provider = %other, "unknown BOSS_AUTH_PROVIDER; defaulting to local-auth");
                Self::LocalAuth
            }
        }
    }
}

pub(crate) struct AppState {
    pub session_key: Vec<u8>,
    pub proxy_client: reqwest::Client,
    pub perf: Arc<PerfCollector>,
    /// Demo mode (`BOSS_DEMO_MODE=1`). When true, anonymous visitors
    /// (no valid `boss_session` cookie) get a synthetic
    /// `audit-readonly` session — read access to every projection,
    /// no writes — so the playground deployment shows live data and
    /// the PersonaSwitcher "View As" flow works without forcing
    /// signup.
    pub demo_mode: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let listen = std::env::var("BOSS_LISTEN").unwrap_or_else(|_| "127.0.0.1:4443".into());
    let session_key_path: std::path::PathBuf = std::env::var("BOSS_SESSION_KEY")
        .unwrap_or_else(|_| "/var/lib/boss-gateway/session.key".into())
        .into();
    let session_key = load_or_create_session_key(&session_key_path)
        .with_context(|| format!("loading session key from {}", session_key_path.display()))?;

    // Seed the executive-role cache from the Class registry so the
    // gateway's admin-ish gates recognise tenant-defined executives
    // via `has_global_read`. URL is the public proxy lookup since
    // the gateway is in front of itself. Skip on missing config or
    // transport failure — platform-admin + audit-readonly still
    // grant global read.
    let classes_url =
        std::env::var("BOSS_CLASSES_URL").unwrap_or_else(|_| boss_ports::url("classes"));
    let classes_client = boss_classes_client::ReqwestClassesClient::new(classes_url.clone());
    match boss_classes_client::seed_executive_role_cache(&classes_client).await {
        Ok(n) => {
            tracing::info!(count = n, classes_url = %classes_url, "executive role cache seeded")
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to seed executive roles from classes; gateway admin gates will skip executive checks")
        }
    }

    let proxy_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building reverse-proxy http client")?;

    // Auth provider — local-auth (file-backed credentials)
    // is the v1 OSS default.
    let auth_provider = AuthProvider::from_env();
    tracing::info!(provider = ?auth_provider, "auth provider selected");

    let local_auth_state = if auth_provider == AuthProvider::LocalAuth {
        let auth_file = std::env::var("BOSS_AUTH_FILE")
            .unwrap_or_else(|_| "/var/lib/boss/auth/credentials.toml".into());
        let store = CredentialStore::load(&auth_file)
            .with_context(|| format!("loading credentials from {auth_file}"))?;
        tracing::info!(
            path = %auth_file,
            users = store.list_emails().len(),
            "local-auth credential store loaded"
        );
        Some(Arc::new(LocalAuthState {
            store,
            session_key: session_key.clone(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .context("local-auth http client")?,
        }))
    } else {
        None
    };

    let demo_mode = std::env::var("BOSS_DEMO_MODE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if demo_mode {
        tracing::info!(
            "BOSS_DEMO_MODE=1: anonymous visitors will be granted audit-readonly sessions"
        );
    }

    let state = Arc::new(AppState {
        session_key,
        proxy_client,
        perf: Arc::new(PerfCollector::new()),
        demo_mode,
    });

    let app = axum::Router::new()
        .route("/health", axum::routing::get(handle_health))
        .route("/api/session", axum::routing::get(api::session))
        .route(
            "/api/tenant/manifest",
            axum::routing::get(api::tenant_manifest),
        )
        .route(
            "/api/finance/revenue-categories",
            axum::routing::get(api::revenue_categories),
        )
        .route("/api/gateway/perf", axum::routing::get(handle_perf))
        .route(
            "/api/gateway/perf/reset",
            axum::routing::post(handle_perf_reset),
        )
        // Dashboard: auth-gated SPA served from static files.
        .route("/dashboard", axum::routing::get(static_files::handle))
        .route("/dashboard/", axum::routing::get(static_files::handle))
        .route(
            "/dashboard/{*rest}",
            axum::routing::get(static_files::handle),
        )
        // Domain-service reverse proxies, all cookie-gated. Each entry
        // pairs a path prefix with a ProxyConfig in proxy.rs (which holds
        // the default upstream URL + any BOSS_<NAME>_UPSTREAM override).
        // Bare `/api/assets` (the SPA lists devices via
        // `/api/assets?account_id=…`, no sub-path) AND `/api/assets/{*rest}`
        // — same dual registration as /api/jobs + /api/people/accounts.
        // Without the bare route, `/api/assets?…` misses the proxy and
        // falls through to the SPA static handler (HTML, not JSON).
        .route(
            "/api/assets",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ASSETS)),
        )
        .route(
            "/api/assets/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ASSETS)),
        )
        // Dispatcher rule-registry surface (read-only) — backs the
        // /system/dispatcher cascade visualization.
        .route(
            "/api/dispatcher/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::DISPATCHER)),
        )
        // Public read surface for the unauth landing page (`/`) —
        // live fetch from /api/jobs/kinds/{kind}, no session
        // required. Strict path matchers win over `/api/jobs/{*rest}`
        // in axum's router.
        // Writes / step metadata / detail routes stay auth-gated.
        // The expanded list — `/api/jobs/summary` and the bare
        // `/api/jobs` GET — turns the landing page from a static
        // workflow-diagram preview into a live window into the
        // brewery's running operating company.
        //
        // For `/api/jobs/kinds` and `/api/jobs/kinds/{*rest}` we
        // pin GET to the public handler so the landing page can
        // read without auth, AND chain the other methods through
        // the auth-gated handler on the same MethodRouter — without
        // the chain, POST/PUT/DELETE would return 405 because axum
        // picks the most-specific matching path first and these
        // strict matchers shadow the wildcard `/api/jobs/{*rest}`.
        .route(
            "/api/jobs/kinds",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::JOBS))
                .post(|s, r| proxy::handle(s, r, &proxy::JOBS))
                .put(|s, r| proxy::handle(s, r, &proxy::JOBS))
                .delete(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/jobs/kinds/{*rest}",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::JOBS))
                .post(|s, r| proxy::handle(s, r, &proxy::JOBS))
                .put(|s, r| proxy::handle(s, r, &proxy::JOBS))
                .delete(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/jobs/summary",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/jobs/live",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/jobs",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/jobs/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // Scheduling routes live alongside jobs on the same upstream.
        // Auth-gated like the rest of /api/*.
        .route(
            "/api/scheduling/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // Public calendar-feed endpoint: /ics/{token}.ics. The token in
        // the URL is the authentication — calendar clients can't carry
        // auth cookies, so we proxy this one path without the cookie
        // gate. Upstream (boss-jobs-api) validates the token.
        .route(
            "/ics/{*rest}",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/catalog/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CATALOG)),
        )
        // Six /api/people/* route families served by accounts-api, not
        // people-api. These more-specific routes MUST come before the
        // /api/people/{*rest} catch-all so axum's longest-prefix
        // match routes them to the right upstream.
        .route(
            "/api/people/accounts",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/accounts/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/account-notes/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/account-account-team/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/support-cases",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/support-cases/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/my-day/actions",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::PEOPLE)),
        )
        .route(
            "/api/people/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::PEOPLE)),
        )
        // Public companion to /api/events/tail — unauth, restricted
        // to a curated demo-friendly topic set. Powers the public
        // landing page's right-rail event tail. Upstream (boss-events)
        // returns the curated allow-list shape; the gateway just
        // proxies unauth so visitors see it.
        .route(
            "/api/events/public-tail",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::EVENTS)),
        )
        .route(
            "/api/events/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::EVENTS)),
        )
        .route(
            "/api/commerce/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::COMMERCE)),
        )
        .route(
            "/api/content/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CONTENT)),
        )
        // File references (docs/architecture-decisions.md §Content,
        // files, knowledge). Lives on
        // boss-content-api alongside bulletins/manual; gateway routes
        // /api/files/* through the same upstream so the SPA's
        // <FileAttachments /> component just hits /api/files without
        // knowing where it terminates.
        .route(
            "/api/files",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CONTENT)),
        )
        .route(
            "/api/files/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CONTENT)),
        )
        .route(
            "/api/inventory/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::INVENTORY)),
        )
        .route(
            "/api/messages/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::MESSAGES)),
        )
        .route(
            "/api/shipping/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::SHIPPING)),
        )
        .route(
            "/api/design/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::DESIGN)),
        )
        // No /api/sim route: the sim runs in-process in the
        // boss-brewery-sim daemon, not behind an HTTP surface.
        .route(
            "/api/ml/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ML)),
        )
        .route(
            "/api/ledger/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::LEDGER)),
        )
        // IT-panel provider status — lives in the ledger binary
        // because 3 of its 4 last-sync data sources are ledger-owned
        // tables. Same upstream, different path prefix.
        .route(
            "/api/it/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::LEDGER)),
        )
        // Policy proxy carries a graceful-fallback for my-scope POST so
        // pages don't log 502s if the policy upstream is down.
        .route(
            "/api/policy/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::POLICY)),
        )
        // Read-only registry services — Class taxonomies (per
        // subject_kind), Location entities, and SubjectKind rows.
        // The auth surface around them is unchanged: same
        // boss_session cookie gate as the rest of /api/*.
        //
        // Each needs BOTH a bare matcher (the list endpoint:
        // `GET /api/classes?subject_kind=…`, `GET /api/subject-kinds`,
        // `GET /api/locations`) AND a wildcard for per-row detail.
        // axum's `{*rest}` requires at least one segment, so without
        // the bare route the list call falls through to the SPA
        // fallback and the client parses index.html as JSON. Same
        // shape as /api/products + /api/people + /api/jobs.
        .route(
            "/api/classes",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CLASSES)),
        )
        .route(
            "/api/classes/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CLASSES)),
        )
        .route(
            "/api/locations",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::LOCATIONS)),
        )
        .route(
            "/api/locations/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::LOCATIONS)),
        )
        .route(
            "/api/subject-kinds",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::SUBJECT_KINDS)),
        )
        .route(
            "/api/subject-kinds/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::SUBJECT_KINDS)),
        )
        // Two matchers: bare for the list endpoint (GET /api/products)
        // + wildcard for per-sku detail and on-hand/by-location. axum's
        // {*rest} requires at least one segment, so the bare path
        // needs its own route or the list endpoint 404s. Same shape
        // as /api/people + /api/jobs.
        .route(
            "/api/products",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::PRODUCTS)),
        )
        .route(
            "/api/products/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::PRODUCTS)),
        )
        .route(
            "/api/campaigns",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CAMPAIGNS)),
        )
        .route(
            "/api/campaigns/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CAMPAIGNS)),
        )
        .route(
            "/api/calendar/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CALENDAR)),
        )
        // Observability aggregator — the SPA's Operations page reads
        // /api/snapshot for the cybernetics rollup. Two strict matchers
        // (no trailing path) plus a wildcard for any future sub-paths.
        // Public read so the unauth landing-page mode keeps working;
        // there's no per-tenant data here yet, just synthetic agent
        // activity from the demo_agents config (or real cross-VM
        // rollups when [[vms]] is populated).
        .route(
            "/api/snapshot",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::OBSERVABILITY)),
        )
        .route(
            "/api/snapshot/{*rest}",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::OBSERVABILITY)),
        )
        // The IT Monitoring page probes /api/<port-name>/health for
        // every PORTS entry. boss-observability and boss-docs both
        // expose their routes under different prefixes (/api/events,
        // /api/snapshot, /api/agents for observability; /api/design/*
        // for docs), so without these aliases the monitoring page
        // shows them as 'down' even when running.
        .route(
            "/api/observability/health",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::OBSERVABILITY)),
        )
        .route(
            "/api/docs/health",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::DESIGN)),
        )
        // Simulator UX — boss-simulator hosts both the /simulator SPA
        // bundle and its /simulator/api/* control+status surface. The
        // whole prefix is proxied (not stripped); the service nests its
        // sub-app under /simulator. Cookie-gated like the dashboard so the
        // demo session + persona flow apply (the service's own operator
        // gate refuses control writes for audit-readonly). These specific
        // routes win over the /{*rest} SPA fallback below.
        .route(
            "/simulator",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::SIMULATOR)),
        )
        .route(
            "/simulator/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::SIMULATOR)),
        )
        // Step UX plugin bundles — served from the plugins dir on
        // disk. See docs/architecture-decisions.md §Step UX & frontend.
        .route("/plugins/{*rest}", axum::routing::get(plugin_files::handle))
        // Root-level: SPA static files. Auth-gated like /dashboard.
        .route("/", axum::routing::get(static_files::handle))
        .route("/{*rest}", axum::routing::get(static_files::handle));

    // Local-auth routes — only mounted when BOSS_AUTH_PROVIDER=
    // local-auth. These handlers carry their own state (the
    // CredentialStore + the session_key + an http client for
    // bootstrap_email lookups against boss-people-api).
    let app = if let Some(la) = local_auth_state.clone() {
        app.route(
            "/api/auth/login",
            axum::routing::post(local_auth::login).with_state(la.clone()),
        )
        .route("/api/auth/logout", axum::routing::post(local_auth::logout))
        .route(
            "/api/auth/me",
            axum::routing::get(local_auth::me).with_state(la.clone()),
        )
        .route(
            "/api/auth/onboard",
            axum::routing::post(local_auth::onboard).with_state(la.clone()),
        )
        .route(
            "/api/auth/issue-reset",
            axum::routing::post(local_auth::issue_reset).with_state(la.clone()),
        )
        .route(
            "/api/auth/reset",
            axum::routing::post(local_auth::reset).with_state(la),
        )
    } else {
        app
    };

    let app = app
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            timing::request_timer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            role_headers::inject_role_headers,
        ))
        // Outermost: when BOSS_DEMO_MODE=1, mint a synthetic
        // `audit-readonly` session for anonymous visitors so the
        // playground shows live data without forcing signup. No-op
        // when demo_mode=false or when a valid cookie is already
        // present.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            demo_session::session_minter,
        ))
        .with_state(state);

    tracing::info!(listen = %listen, static_dir = %static_files::static_dir(), "boss-gateway starting");
    let listener = TcpListener::bind(&listen).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn handle_health() -> &'static str {
    "ok"
}

/// Returns a JSON snapshot of per-endpoint latency percentiles
/// recorded since gateway startup (or last reset).
async fn handle_perf(State(state): State<Arc<AppState>>) -> axum::Json<perf::PerfSnapshot> {
    axum::Json(state.perf.snapshot())
}

/// Clears all recorded histograms. Useful before/after a specific
/// benchmark or fix so percentiles aren't diluted by old data.
async fn handle_perf_reset(State(state): State<Arc<AppState>>) -> &'static str {
    state.perf.reset();
    "ok"
}

/// Load the HMAC session key from disk, or generate one on first run.
/// File is 32 random bytes stored as hex; perms 0600.
fn load_or_create_session_key(path: &Path) -> Result<Vec<u8>> {
    use std::io::Write;
    if path.exists() {
        let hex = std::fs::read_to_string(path)?;
        let bytes = hex_decode(hex.trim())
            .ok_or_else(|| anyhow::anyhow!("session key file is not valid hex"))?;
        if bytes.len() < 32 {
            anyhow::bail!("session key must be at least 32 bytes");
        }
        return Ok(bytes);
    }

    tracing::info!(path = %path.display(), "generating new session key");
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let hex = hex_encode(&bytes);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = f.metadata()?.permissions();
        perms.set_mode(0o600);
        f.set_permissions(perms)?;
    }
    f.write_all(hex.as_bytes())?;
    Ok(bytes.to_vec())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let bytes = [0x00, 0x01, 0xaf, 0xff, 0x7e];
        let hex = hex_encode(&bytes);
        assert_eq!(hex, "0001afff7e");
        assert_eq!(hex_decode(&hex), Some(bytes.to_vec()));
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert_eq!(hex_decode("abc"), None);
    }

    #[test]
    fn hex_decode_rejects_non_hex() {
        assert_eq!(hex_decode("zz"), None);
    }

    #[test]
    fn load_or_create_generates_new_key_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.key");
        let key = load_or_create_session_key(&path).unwrap();
        assert_eq!(key.len(), 32);
        assert!(path.exists());
    }

    #[test]
    fn load_or_create_reuses_existing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.key");
        let first = load_or_create_session_key(&path).unwrap();
        let second = load_or_create_session_key(&path).unwrap();
        assert_eq!(first, second);
    }
}
