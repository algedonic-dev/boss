//! boss-policy-api — row-level authorization service.
//!
//! Serves check / admin endpoints over the PolicyRepository.
//! On startup, seeds DEFAULT_RULES (per D8) — idempotent, operator
//! edits survive restarts.
//!
//! Wires the Postgres-backed `PgPolicy` adapter so rules persist
//! across restarts. The `postgres` feature is required at build
//! time (Cargo `required-features`); the in-memory path remains
//! available only for tests via the library API.

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use tracing::{info, warn};

use boss_policy::port::PolicyRepository;
use boss_policy::{PgPolicy, default_rules};
use boss_policy_client::role_reader::{
    HttpRoleReader, MonotonicRoleSnapshotClock, MountedReportMode, SnapshotRoleReader,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "boss_policy=info,info".into()),
        )
        .init();

    let postgres_url = std::env::var("BOSS_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://boss:boss@127.0.0.1/boss".to_string());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&postgres_url)
        .await
        .with_context(|| format!("connecting to Postgres at {postgres_url}"))?;

    // Where the policy check and the machine gate state what they would
    // refuse: the transactional outbox, which the relay carries to the
    // audit log — the log, not this process, is the clean window
    // (design 21946380).
    let recorder: Arc<dyn boss_core::port::EventRecorder> =
        Arc::new(boss_events::outbox::PgOutboxRecorder::new(pool.clone()));
    // The actor-role report's outbox and its window's log (e0bdba74).
    let role_pool = pool.clone();
    let repo: Arc<PgPolicy> = Arc::new(PgPolicy::new(pool));

    // Reconcile the default rules (per D8). Insert rules that don't
    // exist; refresh bootstrap-owned rows whose scope or active flag
    // drifted from the current code default; preserve any row whose
    // `updated_by != 'bootstrap'` (operator-tuned). This is the
    // explicit self-heal path: widened scopes, corrected typos, and
    // renamed-resource ids in the code defaults propagate to the live
    // DB instead of silently diverging.
    let defaults = default_rules();
    let stats = repo.bootstrap_reconcile(&defaults).await?;
    info!(
        inserted = stats.inserted,
        refreshed = stats.refreshed,
        preserved = stats.preserved,
        unchanged = stats.unchanged,
        total = defaults.len(),
        "reconciled default policy rules"
    );
    // The one rule every service's policy check is answered on (backlog
    // 0028804f). Reconcile inserted it if it was missing and refreshed it
    // if bootstrap owned it; an operator-owned row it preserved, and the
    // rule doors now refuse to leave one narrowed. So anything found here
    // is residue or a hand edit: SAID, with the request that restores it,
    // never repaired and never a reason not to start — a boot that
    // refuses takes the policy service, and every door behind it, down.
    match boss_policy::service_read::found(repo.as_ref()).await {
        Ok(None) => {}
        Ok(Some(report)) => tracing::error!("{report}"),
        Err(e) => warn!(%e, "could not read the service read rule at boot; starting anyway"),
    }

    let role_mode = Arc::new(MountedReportMode::mount());
    let roles = Arc::new(SnapshotRoleReader::new(
        boss_policy_client::role_service::SNAPSHOT_MAX_AGE,
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let role_source = Arc::new(HttpRoleReader::new(
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
        boss_policy_client::User::service("policy"),
    )?);
    // The coverage read (design 1c4e42e1): the roster and its passkeys
    // from the people API, the active workflows from the jobs API — the
    // same env-over-port-table defaults every consumer takes.
    // The write doors' lockout guard reads the same three sources (car 3
    // of design 1c4e42e1) — per write, never at boot, so the reconcile
    // above never waits on the people or jobs API.
    let sources: Arc<dyn boss_policy::coverage::CoverageSources> =
        Arc::new(boss_policy::coverage::HttpCoverageSources::new(
            std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people")),
            std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        ));
    let coverage = boss_policy::coverage::router(boss_policy::coverage::CoverageApiState {
        repo: repo.clone(),
        sources: sources.clone(),
    });
    // F7 of backlog b8e75382 (design b08725c2 row D): what `/check` does
    // with an unsigned caller and a refused service is a mounted word,
    // re-read in seconds, so turning a refusal back is one edit and not a
    // deploy. Absent (every pod today) is `off`.
    let check_mode = boss_policy::check_mode::CheckMode::mount(Arc::clone(&recorder));
    let app: Router = boss_policy::role_reports::mount(
        repo,
        check_mode,
        sources,
        roles.clone(),
        role_mode.clone(),
        boss_events::role_tally::durable("policy", role_mode.clone(), Some(&role_pool)),
        std::time::Duration::from_millis(100),
    )
    .merge(coverage);
    // A write a policy door refuses is said on the log line AND as a
    // `policy.write.refused` event through the outbox (review 1a73d5ce
    // F2): until backlog 0028804f a refused attempt left no trace at all.
    let app = boss_policy::refusals::recorded(app, Arc::clone(&recorder));

    // Default port pulled from boss_ports — single source of truth
    // shared with the config generator + every BOSS_POLICY_URL
    // default. The 7060/7250 collision once lived right here; the
    // table now makes drift impossible.
    let port = std::env::var("BOSS_POLICY_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or_else(|| boss_ports::prod("policy"));
    // Loopback: the gateway is the sole trust boundary and is
    // co-located in every deployment (SECURITY.md §Deployment
    // trust model). Set BOSS_POLICY_BIND_HOST to widen deliberately.
    let host = std::env::var("BOSS_POLICY_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let bind = format!("{host}:{port}");

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            warn!(?e, "failed to bind {bind}; exiting");
            return Err(e.into());
        }
    };
    info!(%bind, "boss-policy-api listening (postgres-backed)");
    let app =
        boss_core::machine_gate::mount(app, "policy", &["/api/policy/health"], Some(recorder));
    let (role_stop, stop) = tokio::sync::watch::channel(false);
    let role_refresh = tokio::spawn(roles.run_refresh_loop(
        role_source,
        role_mode,
        boss_policy_client::role_service::REFRESH_CADENCE,
        stop,
    ));
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let interrupt = async {
                if let Err(error) = tokio::signal::ctrl_c().await {
                    warn!(%error, "policy interrupt signal unavailable");
                    std::future::pending::<()>().await;
                }
            };
            // Gate evidence owns process termination; this path owns interrupts.
            interrupt.await;
        })
        .await;
    let _ = role_stop.send(true);
    role_refresh
        .await
        .context("joining policy actor-role refresh")??;
    served?;
    Ok(())
}
