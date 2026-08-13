//! `boss-jobs-api` service: jobs domain + NATS event bus + HTTP API.
//!
//! Wires the jobs repository (Postgres or in-memory) to NATS for
//! event distribution and exposes an axum HTTP API.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::in_memory::InMemoryJobs;
use boss_jobs::jobs_config::JobsApiConfig;
use boss_jobs::port::JobsRepository;
use boss_nats::NatsEventBus;
use clap::Parser;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-jobs-api", about = "Boss Jobs API service", version)]
struct Cli {
    /// Path to the service config (TOML)
    #[arg(short, long, default_value = "/etc/boss-jobs-api.toml")]
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
    let cfg = JobsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        nats_url = %cfg.nats_url,
        http_bind = %cfg.http_bind,
        postgres = cfg.postgres_url.is_some(),
        "boss-jobs-api starting"
    );

    // Connect to NATS.
    let bus = Arc::new(
        NatsEventBus::connect(&cfg.nats_url)
            .await
            .with_context(|| format!("connecting to NATS at {}", cfg.nats_url))?,
    );

    // Seed the executive-role cache from the Class registry — the
    // escalation router below filters recipients by `is_executive`.
    // Skip on missing config or transport failure; the router still
    // runs but won't page anyone in that state.
    if let Some(url) = &cfg.classes_api_url {
        let client = boss_classes_client::ReqwestClassesClient::new(url.clone());
        match boss_classes_client::seed_executive_role_cache(&client).await {
            Ok(n) => info!(count = n, classes_api_url = %url, "executive role cache seeded"),
            Err(e) => {
                tracing::warn!(error = %e, "failed to seed executive roles from classes; escalation router will skip executive paging")
            }
        }
    } else {
        info!("classes_api_url unset; executive role cache disabled");
    }

    // Spawn the escalation router as a background subscriber. It
    // listens for jobs.job.created events and pages executives when
    // a critical ticket lands on a platinum/gold account. Running
    // inside the jobs-api service keeps the subscriber close to the
    // publisher without a new deploy unit.
    // Concrete bus, not the port: the router opens a JetStream durable
    // consumer (at-least-once) and only falls back to the port-shaped
    // core subscription when JetStream is absent.
    let _escalation_handle = boss_jobs::escalation::spawn_router(
        bus.clone(),
        boss_jobs::escalation::EscalationConfig::default(),
    );

    let (cancel_tx, cancel_rx) = watch::channel(false);

    // Build the publisher: bus + (optional) Postgres audit writer.
    #[allow(unused_mut)]
    let mut publisher = boss_core::publisher::DomainPublisher::new(
        bus.clone() as std::sync::Arc<dyn boss_core::port::EventBus>,
        "jobs",
    );

    // Optional cross-service calendar client. None → step
    // reservation hook is a no-op; rollout-friendly across the
    // calendar service deploy.
    let calendar: Option<Arc<dyn boss_calendar_client::CalendarClient>> =
        cfg.calendar_api_url.as_deref().map(|url| {
            info!(calendar_api_url = %url, "calendar client wired up — step reservation hook live");
            Arc::new(boss_calendar_client::ReqwestCalendarClient::new(url))
                as Arc<dyn boss_calendar_client::CalendarClient>
        });
    if calendar.is_none() {
        info!("calendar_api_url unset — step reservation hook disabled");
    }

    // Optional SubjectKind registry client. None → Subject writes
    // accept any kind string. Same opt-in shape as `calendar`.
    let subject_kinds: Option<Arc<dyn boss_subject_kinds_client::SubjectKindsClient>> =
        cfg.subject_kinds_api_url.as_deref().map(|url| {
            info!(subject_kinds_api_url = %url, "subject-kinds client wired up — Custom subject validation live");
            Arc::new(boss_subject_kinds_client::ReqwestSubjectKindsClient::new(url))
                as Arc<dyn boss_subject_kinds_client::SubjectKindsClient>
        });
    if subject_kinds.is_none() {
        info!(
            "subject_kinds_api_url unset — Custom subject validation disabled (Phase A behaviour)"
        );
    }

    // Authoritative clock — every audit_log row + every "now" the
    // jobs API stamps comes from clock-api. Default URL pulled
    // from boss-ports; override via BOSS_CLOCK_URL. Constructed
    // here (not in run_server) because the boot-time registry
    // reconcile below already needs a clock-routed "now" for the
    // events its writes record.
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    info!(%clock_url, "clock client wired");
    let clock: Arc<dyn boss_clock_client::ClockClient> =
        Arc::new(boss_clock_client::ReqwestClockClient::new(clock_url));

    // Choose storage backend: Postgres when configured, in-memory otherwise.
    #[cfg(feature = "postgres")]
    if let Some(ref pg_url) = cfg.postgres_url {
        info!("using Postgres jobs storage");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(20)
            .connect(pg_url)
            .await
            .with_context(|| "connecting to Postgres")?;
        // Subject existence validator: one indexed lookup against the
        // `subjects` identity table, uniform for every kind — platform,
        // tenant-defined, all of them (subject-model design R1, approved
        // 2026-07-15). Replaces the four-upstream HTTP prober and its
        // fall-through kinds; the create handler fails CLOSED when the
        // check is unavailable (Q2: abort by default).
        let subject_existence: Option<
            Arc<dyn boss_jobs::subject_existence::SubjectExistenceCheck>,
        > = {
            info!("subject existence gate wired to the subjects identity table (all kinds)");
            Some(
                Arc::new(boss_jobs::subject_existence::PgSubjectExistence::new(
                    pool.clone(),
                )) as Arc<dyn boss_jobs::subject_existence::SubjectExistenceCheck>,
            )
        };
        publisher = publisher.with_audit(std::sync::Arc::new(boss_events::PgAuditWriter::new(
            pool.clone(),
        )));
        info!("audit_log persistence enabled");
        // Pass the URLs alongside the pool so the demo-loop
        // restart-epoch endpoint can spawn boss-rebuild-all against the
        // same DB, and purge the JetStream delivery buffer on the same
        // broker, without re-parsing config. The NATS URL comes from
        // config rather than the environment — this service has never
        // set `BOSS_NATS_URL`, so reading one would skip the purge.
        let jobs = Arc::new(boss_jobs::PgJobs::with_urls(
            pool.clone(),
            pg_url.to_string(),
            cfg.nats_url.clone(),
        ));
        let kind_registry: Arc<dyn boss_jobs::WorkflowRegistry> =
            Arc::new(boss_jobs::PgWorkflows::new(pool.clone()));
        reconcile_platform_workflows(kind_registry.as_ref(), &clock).await;
        let plugin_registry: Arc<dyn boss_jobs::StepPluginRegistry> =
            Arc::new(boss_jobs::PgStepPlugins::new(pool.clone()));
        let scheduling: Arc<dyn boss_jobs::scheduling::SchedulingRepository> =
            Arc::new(boss_jobs::scheduling::PgScheduling::new(pool.clone()));
        // Q7: human job-owner resolution over the people roster.
        let people_url =
            std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people"));
        let roster: Option<Arc<dyn boss_jobs::owner_resolution::RosterLookup>> = Some(Arc::new(
            boss_jobs::owner_resolution::ReqwestRosterLookup::new(people_url.clone()),
        ));
        info!(%people_url, "human job-owner resolution wired (Q7)");
        return run_server(
            Some(
                std::sync::Arc::new(boss_jobs::job_edges::PgJobEdges::new(pool.clone()))
                    as std::sync::Arc<dyn boss_jobs::job_edges::JobEdgesRegistry>,
            ),
            Some(Arc::new(boss_jobs::PgStations::new(pool.clone()))
                as Arc<dyn boss_jobs::StationRegistry>),
            jobs,
            bus,
            publisher,
            Some(kind_registry),
            Some(plugin_registry),
            Some(scheduling),
            calendar,
            subject_kinds,
            subject_existence,
            roster,
            clock.clone(),
            cancel_tx,
            cancel_rx,
            &cfg.http_bind,
        )
        .await;
    }

    boss_core::startup::require_postgres_or_explicit_inmemory("boss-jobs-api")?;
    info!("using in-memory jobs storage (no postgres_url configured)");
    let jobs = Arc::new(InMemoryJobs::new());
    let kind_registry: Arc<dyn boss_jobs::WorkflowRegistry> =
        Arc::new(boss_jobs::InMemoryWorkflows::new());
    let plugin_registry: Arc<dyn boss_jobs::StepPluginRegistry> =
        Arc::new(boss_jobs::InMemoryStepPlugins::new());
    reconcile_platform_workflows(kind_registry.as_ref(), &clock).await;
    // No subjects table without Postgres — the in-memory spike path
    // skips the existence gate, same as before.
    let subject_existence: Option<Arc<dyn boss_jobs::subject_existence::SubjectExistenceCheck>> =
        None;
    run_server(
        Some(std::sync::Arc::new(boss_jobs::job_edges::InMemoryJobEdges)
            as std::sync::Arc<dyn boss_jobs::job_edges::JobEdgesRegistry>),
        Some(Arc::new(boss_jobs::InMemoryStations::new()) as Arc<dyn boss_jobs::StationRegistry>),
        jobs,
        bus,
        publisher,
        Some(kind_registry),
        Some(plugin_registry),
        None,
        calendar,
        subject_kinds,
        subject_existence,
        // In-memory spike path: no people stack to resolve against.
        None,
        clock,
        cancel_tx,
        cancel_rx,
        &cfg.http_bind,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_server<R: JobsRepository + 'static>(
    job_edges: Option<std::sync::Arc<dyn boss_jobs::job_edges::JobEdgesRegistry>>,
    stations: Option<Arc<dyn boss_jobs::StationRegistry>>,
    jobs: Arc<R>,
    bus: Arc<NatsEventBus>,
    publisher: boss_core::publisher::DomainPublisher,
    kind_registry: Option<Arc<dyn boss_jobs::WorkflowRegistry>>,
    plugin_registry: Option<Arc<dyn boss_jobs::StepPluginRegistry>>,
    scheduling: Option<Arc<dyn boss_jobs::scheduling::SchedulingRepository>>,
    calendar: Option<Arc<dyn boss_calendar_client::CalendarClient>>,
    subject_kinds: Option<Arc<dyn boss_subject_kinds_client::SubjectKindsClient>>,
    subject_existence: Option<Arc<dyn boss_jobs::subject_existence::SubjectExistenceCheck>>,
    roster: Option<Arc<dyn boss_jobs::owner_resolution::RosterLookup>>,
    clock: Arc<dyn boss_clock_client::ClockClient>,
    cancel_tx: watch::Sender<bool>,
    cancel_rx: watch::Receiver<bool>,
    http_bind: &str,
) -> Result<()> {
    // Start axum HTTP server.
    let step_registry = Arc::new(boss_jobs::step_registry::StepRegistry::v1());
    info!(
        step_types = step_registry.all().len(),
        "step type registry loaded"
    );

    // Policy client: wires boss-policy-api for row-level authorization.
    // Default URL pulled from the boss-ports table — single source
    // of truth shared with `infra/deploy-services.sh`. Override via
    // BOSS_POLICY_URL. The 7060/7250 collision (`bb60c58` +
    // `8bf0f0a`) that motivated boss-ports lived right here.
    let policy_url = std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy"));
    // Banner so a port-collision misconfiguration surfaces in
    // journalctl immediately — not 30 minutes later when the
    // landing page won't load — a guard against the historical
    // 7060/7250 port collision.
    tracing::info!(policy_url = %policy_url, "policy client configured");
    // Wrap the prod client in the sim-origin bypass: simulator traffic
    // (x-sim-origin, already stamped _simulated) is authorized at the
    // boundary on the trusted box; real traffic is enforced per-role by
    // the inner ReqwestPolicyClient.
    let policy: Arc<dyn boss_policy_client::PolicyClient> =
        Arc::new(boss_policy_client::SimBypassPolicyClient::new(Arc::new(
            boss_policy_client::ReqwestPolicyClient::new(policy_url),
        )));

    // Wire the sim-mode probe into the publisher so every emit_at
    // injects `_simulated: bool` into the audit_log payload without
    // per-handler changes. `publisher` here is `DomainPublisher`
    // (not Option) — wire directly.
    let publisher = publisher.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
        clock.clone(),
    )));
    let scheduling_publisher = publisher.clone();

    let state = JobsApiState {
        job_edges,
        stations,
        jobs,
        bus,
        publisher,
        step_registry,
        policy,
        kind_registry,
        plugin_registry,
        calendar,
        subject_kinds,
        subject_existence,
        roster,
        clock: clock.clone(),
    };
    let mut app = router(state);
    if let Some(repo) = scheduling {
        info!("scheduling routes mounted at /api/scheduling/*");
        app = app.merge(boss_jobs::scheduling::http::router(
            boss_jobs::scheduling::http::SchedulingApiState {
                repo,
                publisher: Some(scheduling_publisher),
                clock: clock.clone(),
            },
        ));
    }
    // Sim-origin middleware: extract x-sim-origin header and set the
    // per-request task-local so the publisher inherits the sim
    // marker. Closes the gap where a sim chain could trigger a
    // non-sim event on a service running with a wall clock.
    let app = app.layer(axum::middleware::from_fn(
        boss_policy_client::request_context_middleware,
    ));
    let http_addr: SocketAddr = http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{http_bind}`"))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "jobs HTTP API listening");

    let mut http_rx = cancel_rx.clone();
    let http_task = tokio::spawn(async move {
        let shutdown = async move {
            let _ = http_rx.changed().await;
        };
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            error!(error = %e, "HTTP server exited with error");
        }
    });

    // Wait for Ctrl+C.
    tokio::signal::ctrl_c().await.ok();
    info!("shutdown signal received");
    let _ = cancel_tx.send(true);

    let _ = http_task.await;
    info!("boss-jobs-api shut down cleanly");
    Ok(())
}

/// Reconcile the platform-supplied Workflows (today: just
/// `workflow-design`) against the live registry. Insert if
/// missing, refresh bootstrap-owned drift, preserve operator
/// edits — same shape as
/// `boss_policy_client::PolicyRepository::bootstrap_reconcile`.
///
/// Logs the stats line on every boot so operators can see the
/// reconcile decision land in real time. The platform list is
/// short (just one kind in v1) so a missing default surfaces
/// instantly: the next boot logs `inserted=1` if someone
/// retired the meta-kind by hand.
///
/// Each inserted/republished row records `jobs.kind.published`
/// with the row (registry-events invariant), attributed to the
/// named `bootstrap-reconciler` automation at a clock-routed
/// "now" — a boot-time reconcile in sim mode stamps sim time.
async fn reconcile_platform_workflows(
    registry: &dyn boss_jobs::WorkflowRegistry,
    clock: &Arc<dyn boss_clock_client::ClockClient>,
) {
    use boss_jobs::registry::platform_workflows;
    let defaults = platform_workflows();
    let actor = boss_core::actor::ActorId::Automation("bootstrap-reconciler".into());
    let now = boss_clock_client::now_from(clock).await;
    match registry.bootstrap_reconcile(&defaults, &actor, now).await {
        Ok(stats) => {
            info!(
                inserted = stats.inserted,
                republished = stats.republished,
                preserved = stats.preserved,
                unchanged = stats.unchanged,
                total = defaults.len(),
                "reconciled platform Workflows"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "platform Workflow reconcile failed");
        }
    }
    verify_registry_viability(registry).await;
}

/// Boot-time viability re-verification: every active Workflow in the
/// registry must still pass the viability lint. A previously-valid
/// spec can become invalid if an upstream StepType's enum domain
/// changes; refuse to start rather than dispatch against a broken
/// graph (the audit_log is the system of record — we don't open for
/// writes we can't reason about).
async fn verify_registry_viability(registry: &dyn boss_jobs::WorkflowRegistry) {
    use boss_jobs::step_registry::StepRegistry;
    use boss_jobs::workflow_lint::validate_all;
    let kinds = match registry.list_active(None).await {
        Ok(k) => k,
        Err(e) => {
            tracing::error!(error = %e, "boot viability check: could not list active Workflows");
            std::process::exit(1);
        }
    };
    let errs = validate_all(&kinds, &StepRegistry::v1());
    if !errs.is_empty() {
        for e in &errs {
            tracing::error!("boot viability check: {e}");
        }
        tracing::error!(
            count = errs.len(),
            "refusing to start: active Workflow(s) fail the viability lint"
        );
        std::process::exit(1);
    }
    info!(active = kinds.len(), "boot viability check passed");
}
