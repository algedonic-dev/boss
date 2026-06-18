//! Boot-time guards every Boss service binary should use.
//!
//! The pattern: each service crate has an in-memory repo for tests
//! and a Postgres repo behind `#[cfg(feature = "postgres")]`. A binary
//! built **without** the `postgres` feature in error (e.g. the deploy
//! script forgot the flag) would otherwise silently serve in-memory
//! data — every write landing in a process-local map and disappearing
//! on restart, a degradation invisible to operators until something
//! observable breaks downstream.
//!
//! [`require_postgres_or_explicit_inmemory`] is the load-bearing
//! guard. Call it from each service's `main()` on the
//! `#[cfg(not(feature = "postgres"))]` branch — it errors out with a
//! clear remediation message unless the operator explicitly opts in
//! by setting `BOSS_ALLOW_INMEMORY=1` (test harnesses, dev shells).

/// Refuse to serve in-memory unless the operator explicitly opts in.
///
/// Intended call site:
///
/// ```ignore
/// #[cfg(not(feature = "postgres"))]
/// boss_core::startup::require_postgres_or_explicit_inmemory("boss-docs-api")?;
/// ```
///
/// Returns `Ok(())` if `BOSS_ALLOW_INMEMORY=1` is set in the
/// environment (the in-memory path is then taken with a `WARN`
/// announcing the override). Otherwise returns `Err` with the
/// remediation text — the binary is expected to propagate this and
/// exit non-zero.
pub fn require_postgres_or_explicit_inmemory(service_name: &str) -> anyhow::Result<()> {
    if std::env::var("BOSS_ALLOW_INMEMORY").is_ok() {
        tracing::warn!(
            service = service_name,
            "BOSS_ALLOW_INMEMORY=1 — serving in-memory only; writes will not persist"
        );
        return Ok(());
    }
    anyhow::bail!(
        "{service_name} was built without the `postgres` feature.\n\
         Writes would silently disappear into a process-local map.\n\
         Fix: rebuild with `cargo build --release -p <crate> --bin {service_name} --features postgres`,\n\
         or set BOSS_ALLOW_INMEMORY=1 to acknowledge the limitation (intended for tests / dev shells).",
    )
}

/// Redact the password in a database URL for safe logging.
///
/// `postgres://user:pass@host/db` → `postgres://user:***@host/db`.
/// Finds the **first** `:` after the `://` scheme separator, so a
/// password that itself contains `:` is masked in full (no prefix
/// leak). Returns the input unchanged when there's no userinfo
/// `password` segment to redact.
pub fn mask_password(url: &str) -> String {
    match (url.find("://"), url.find('@')) {
        (Some(scheme_end), Some(at)) => {
            let scheme_end = scheme_end + 3;
            if at > scheme_end
                && let Some(colon) = url[scheme_end..at].find(':')
            {
                let user_end = scheme_end + colon;
                return format!("{}:***{}", &url[..user_end], &url[at..]);
            }
            url.to_string()
        }
        _ => url.to_string(),
    }
}

/// Self-reported capability snapshot returned by every service's
/// `/health` endpoint.
///
/// The aggregator at `/api/snapshot/capabilities` (boss-observability)
/// fans out, collects these, and flags any service whose `storage`
/// is `"in-memory"` — the signature of a service accidentally built
/// without the `postgres` feature.
///
/// `infra/check-service-write-roundtrip.sh` reads the same field as
/// a defense-in-depth check.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    /// Service name for log/audit reference (e.g. `"boss-docs-api"`).
    pub service: &'static str,
    /// `"postgres"` when the binary was compiled with the `postgres`
    /// feature, `"in-memory"` otherwise. The latter is a yellow flag
    /// in production — see `require_postgres_or_explicit_inmemory`.
    pub storage: &'static str,
    /// Crate version (`CARGO_PKG_VERSION`).
    pub version: &'static str,
}

impl Capabilities {
    /// Build capabilities for the calling service. The `service` arg
    /// is the binary name (`"boss-docs-api"`); the `version` arg is
    /// usually `env!("CARGO_PKG_VERSION")` at the call site. The
    /// `storage` arg is the storage backend the caller actually
    /// wired up — typically `"postgres"` from a
    /// `#[cfg(feature = "postgres")]` arm, `"in-memory"` from the
    /// fallback arm.
    pub fn new(service: &'static str, version: &'static str, storage: &'static str) -> Self {
        Self {
            service,
            storage,
            version,
        }
    }
}

/// Standard `/health` payload every Boss `*-api` binary returns.
///
/// `status` is `"ok"` while the process is serving; `capabilities`
/// is the [`Capabilities`] snapshot the aggregator at
/// `/api/snapshot/capabilities` fans out to collect. Build it with
/// [`health_response`] — the handler is a pure const response, so a
/// service's whole health triplet collapses to one call:
///
/// ```ignore
/// async fn health() -> axum::Json<boss_core::startup::HealthResponse> {
///     axum::Json(boss_core::startup::health_response(
///         "boss-docs-api",
///         env!("CARGO_PKG_VERSION"),
///         STORAGE,
///     ))
/// }
/// ```
///
/// Services that carry extra health fields (boss-clock's `mode`,
/// boss-cybernetics' `vm_id`/`timestamp`) or a flatter wire shape
/// (boss-classes/locations/subject-kinds) keep their own structs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HealthResponse {
    /// `"ok"` while the process is serving requests.
    pub status: &'static str,
    /// Self-reported capability snapshot for this service.
    pub capabilities: Capabilities,
}

/// Build the standard [`HealthResponse`] for the calling service.
///
/// `status` is fixed to `"ok"`; the args feed straight into
/// [`Capabilities::new`]. See [`HealthResponse`] for the call-site
/// shape.
pub fn health_response(
    service: &'static str,
    version: &'static str,
    storage: &'static str,
) -> HealthResponse {
    HealthResponse {
        status: "ok",
        capabilities: Capabilities::new(service, version, storage),
    }
}
