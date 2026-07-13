//! `boss-brewery-sim` — the 1-year-per-hour live tick daemon.
//!
//! Long-running service that drives `run_brewery_one_day`
//! against the live API stack. Every `tick_interval_seconds`,
//! advances the sim clock by `days_per_tick` sim-days. Default
//! 1 day × 10-second tick = 360 sim-days per real-time hour ≈
//! "1 year per hour" — the cadence the playground viewers see,
//! with state changes landing every ~10s instead of bursting
//! every minute.
//!
//! The daemon holds a long-lived `BreweryEngineState` (one
//! ShapeDrivenState + PeriodicEngine + CounterpartyEngine +
//! BatchEngine + Rng across the whole process lifetime). This
//! preserves counterparty pending-action state — the bank-ach
//! 30-business-day delay, the ar-aging chain, the keg-courier
//! 3-stage scan tracking — across tick boundaries. Without it,
//! 1-day chunks would lose every chained delay >1 day and the
//! brewery's economic loop would visibly stop firing.
//!
//! State persists in a single-row `sim_clock` table:
//!     id (always 1)
//!     current_sim_date
//!     days_per_tick           — operator-tunable
//!     tick_interval_seconds   — operator-tunable
//!     paused                  — flip to true to halt without
//!                                stopping the daemon
//!     updated_at
//!
//! Restarts pick up at `current_sim_date`; the daemon does NOT
//! replay. If you want a clean replay, regenerate the canonical
//! seed via `infra/postgres/validate-brewery-sim.sh`.
//!
//! See `docs/design/projection-rebuilders.md §G`.
//!
//! Env vars (all optional — sim_clock row defaults win once
//! initialized):
//!   BOSS_SIM_API_BASE    default direct://127.0.0.1
//!   BOSS_SIM_SEEDS_DIR   default /opt/boss/examples/brewery/seeds
//!   BOSS_SIM_INITIAL_DATE  YYYY-MM-DD — only consulted on first
//!                          boot (when sim_clock is empty). Default:
//!                          one day after the canonical seed's last
//!                          jobs.opened_on (so the live tick picks
//!                          up exactly where the seed left off).
//!   BOSS_SIM_DAYS_PER_TICK / BOSS_SIM_TICK_SECONDS — same first-
//!                          boot defaults. Operator can edit
//!                          sim_clock directly thereafter.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use boss_brewery_engine::sim_control;
use boss_brewery_engine::{
    BreweryEngineState, brewery_end_of_day, build_workforce, run_brewery_live,
    run_brewery_one_tick, start_callback_receiver,
};
use boss_sim::engines::SimBusEvent;
use boss_sim::event_routes::register_default_event_routes;
use boss_sim::output::live::LiveApiOutput;
use boss_sim::workforce::Workforce;
use chrono::NaiveDate;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Resolve a service's base URL for the pre-Go readiness probes. The daemon
/// default `direct://…` maps each service to its own localhost port via
/// boss_ports; a gateway base routes everything through that one URL.
fn readiness_base(api_base: &str, svc: &str) -> String {
    if api_base.starts_with("direct://") {
        boss_ports::url(svc)
    } else {
        api_base.trim_end_matches('/').to_string()
    }
}

/// One pass of the pre-Go readiness checks (all HTTP — the sim is a pure API
/// client). Returns (check, ok, detail) for EVERY check so the caller logs a
/// full report regardless of pass/fail.
async fn readiness_pass(client: &reqwest::Client, api_base: &str) -> Vec<(String, bool, String)> {
    let mut out: Vec<(String, bool, String)> = Vec::new();

    // 1. Core services answer /health — the APIs the sim + dispatcher write
    //    through. An unreachable one means a Job can't make progress.
    for svc in [
        "clock",
        "policy",
        "classes",
        "people",
        "accounts",
        "jobs",
        "inventory",
        "ledger",
        "products",
        "catalog",
        "assets",
        "messages",
    ] {
        let url = format!("{}/api/{}/health", readiness_base(api_base, svc), svc);
        let (ok, detail) = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => (true, "200".to_string()),
            Ok(r) => (false, format!("HTTP {}", r.status())),
            Err(e) => (false, format!("unreachable ({e})")),
        };
        out.push((format!("service:{svc}"), ok, detail));
    }

    // 2. The dispatcher's durable consumers are actually BOUND — the signal a
    //    /health 200 can't give. Without this the workforce never gets
    //    assigned Steps and Jobs never reach a terminal state.
    {
        let url = format!(
            "{}/api/dispatcher/readyz",
            readiness_base(api_base, "dispatcher")
        );
        let (ok, detail) = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
                Ok(v) => {
                    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
                    (
                        b("ready"),
                        format!(
                            "assigning={} rules_running={} assignment_events={} rules_events={}",
                            b("assigning"),
                            b("rules_running"),
                            v.get("assignment_events")
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0),
                            v.get("rules_events").and_then(|x| x.as_u64()).unwrap_or(0),
                        ),
                    )
                }
                Err(e) => (false, format!("bad json ({e})")),
            },
            Ok(r) => (false, format!("HTTP {}", r.status())),
            Err(e) => (false, format!("unreachable ({e})")),
        };
        out.push(("dispatcher:consumers".to_string(), ok, detail));
    }

    // 3. The clock is primed: sim mode, a non-empty epoch range, not paused. A
    //    wall-time / zero-length / paused clock means the sim won't advance.
    {
        let url = format!("{}/api/clock/now", readiness_base(api_base, "clock"));
        let (ok, detail) = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
                Ok(v) => {
                    let sim = v
                        .get("simulated")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                    let paused = v.get("paused").and_then(|x| x.as_bool()).unwrap_or(false);
                    let es = v.get("epoch_start").and_then(|x| x.as_str()).unwrap_or("");
                    let ee = v.get("epoch_end").and_then(|x| x.as_str()).unwrap_or("");
                    let ranged = !es.is_empty() && !ee.is_empty() && ee > es;
                    (
                        sim && ranged && !paused,
                        format!("simulated={sim} epoch={es}..{ee} paused={paused}"),
                    )
                }
                Err(e) => (false, format!("bad json ({e})")),
            },
            Ok(r) => (false, format!("HTTP {}", r.status())),
            Err(e) => (false, format!("unreachable ({e})")),
        };
        out.push(("clock:primed".to_string(), ok, detail));
    }

    out
}

/// Poll [`readiness_pass`] until every check is green, logging a full ✓/✗
/// report each pass, then return so the caller starts ticking. It does NOT
/// give up: a stack that can't carry a Job to closure must never be flooded
/// with Jobs, so the sim HOLDS here — creating nothing, keeping every service
/// + the SPA up for inspection — until the stack is actually ready. After
/// `soft_timeout_secs` the warning gets louder and the poll slows, but it
/// keeps waiting. Bypass entirely with BOSS_SIM_SKIP_READINESS=1.
async fn wait_until_ready(api_base: &str, soft_timeout_secs: u64) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("readiness gate: could not build HTTP client ({e}); skipping gate");
            return;
        }
    };
    let start = std::time::Instant::now();
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let checks = readiness_pass(&client, api_base).await;
        let failing = checks.iter().filter(|(_, ok, _)| !ok).count();
        let report = || {
            for (name, ok, detail) in &checks {
                if *ok {
                    info!("    ✓ {name}: {detail}");
                } else {
                    warn!("    ✗ {name}: {detail}");
                }
            }
        };
        if failing == 0 {
            info!(
                checks = checks.len(),
                "✓ pre-Go readiness: all checks passed — starting sim"
            );
            report();
            return;
        }
        let elapsed = start.elapsed().as_secs();
        if elapsed >= soft_timeout_secs {
            // Past the soft window the stack still can't carry a Job to closure.
            // Keep HOLDING (never flood) and say so loudly — the most common ✗
            // here is dispatcher:consumers (GET /api/dispatcher/readyz).
            warn!(
                attempt,
                elapsed_secs = elapsed,
                "pre-Go readiness: STILL not ready — sim is HOLDING (creating no Jobs). Fix the \
                 ✗ checks below; the stack stays up for inspection."
            );
        } else {
            warn!(
                attempt,
                elapsed_secs = elapsed,
                "pre-Go readiness: {failing}/{} checks not ready yet",
                checks.len()
            );
        }
        report();
        let backoff = if elapsed >= soft_timeout_secs { 15 } else { 3 };
        tokio::time::sleep(Duration::from_secs(backoff)).await;
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let seeds_dir = std::env::var("BOSS_SIM_SEEDS_DIR")
        .unwrap_or_else(|_| "/opt/boss/examples/brewery/seeds".to_string());
    let seeds_path = PathBuf::from(&seeds_dir);

    // One-shot `prepare` subcommand: seed the whole brewery tenant
    // model (classes → JobKinds → policy → data) through the public
    // API, then exit. This is the converged prepare phase — reset /
    // launchers / CI call it instead of the old scattered
    // bootstrap + policy-bootstrap + data-seed + classes-curl steps,
    // so the offline path and the live demo seed identical code.
    //
    // Per-service routing by default (reset seeds with the gateway
    // stopped); BOSS_SIM_PREPARE_GATEWAY routes everything through one
    // gateway URL instead. reqwest::blocking inside, so it runs on a
    // blocking thread off the async runtime.
    if std::env::args().nth(1).as_deref() == Some("prepare") {
        let gateway = std::env::var("BOSS_SIM_PREPARE_GATEWAY").ok();
        let seeds = seeds_path.clone();
        info!(seeds = %seeds_dir, gateway = ?gateway, "boss-brewery-sim prepare");
        return tokio::task::spawn_blocking(move || {
            boss_brewery_engine::prepare::prepare_model(gateway.as_deref(), &seeds)
        })
        .await
        .context("spawn_blocking prepare")?;
    }

    // One-shot `run` subcommand: drive a bounded regen (N sim-days)
    // against a live API stack, then exit — the offline 12-month seed
    // regen + the CI correctness gate (validate-brewery-sim.sh). Built
    // on the SAME run_brewery_live driver the daemon's per-tick loop
    // shares, so the bounded regen and the live daemon can't drift.
    // Assumes the tenant is already prepared (`boss-brewery-sim
    // prepare`) — it drives, it does not seed. Env-driven (matches the
    // daemon's style + the BOSS_REGEN_* vars validate-brewery-sim.sh
    // already exports). reqwest::blocking inside → spawn_blocking.
    if std::env::args().nth(1).as_deref() == Some("run") {
        let seeds = seeds_path.clone();
        let api_base =
            std::env::var("BOSS_SIM_API_BASE").unwrap_or_else(|_| "direct://127.0.0.1".to_string());
        let days: u32 = std::env::var("BOSS_REGEN_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(365);
        let start = std::env::var("BOSS_REGEN_START")
            .ok()
            .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());
        let warp: f64 = std::env::var("BOSS_REGEN_WARP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8640.0);
        let poll: u64 = std::env::var("BOSS_REGEN_POLL_SLEEP_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(200);
        // Default hard-fail: the canonical regen requires zero non-2xx.
        // BOSS_REGEN_HARD_FAIL=false (or 0) relaxes it for ad-hoc runs.
        let hard_fail = std::env::var("BOSS_REGEN_HARD_FAIL")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);
        let drain_pause: u64 = std::env::var("BOSS_REGEN_DRAIN_PAUSE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        info!(
            %api_base, days, ?start, warp, poll, hard_fail, drain_pause,
            "boss-brewery-sim run (bounded regen)"
        );
        return tokio::task::spawn_blocking(move || {
            run_regen(
                &seeds,
                &api_base,
                days,
                start,
                warp,
                poll,
                hard_fail,
                drain_pause,
            )
        })
        .await
        .context("spawn_blocking run")?;
    }

    let api_base =
        std::env::var("BOSS_SIM_API_BASE").unwrap_or_else(|_| "direct://127.0.0.1".to_string());

    info!(
        %api_base, seeds = %seeds_dir,
        "boss-brewery-sim starting"
    );

    // brewery-sim is a pure public-API client — it only ever touches
    // the system through the public HTTP API (/api/clock/now, /api/jobs,
    // the domain services). It has no database or message-bus access by
    // construction (see this crate's Cargo.toml + infra/lint/sim-boundary-audit.sh).

    // Business calendars as DATA — fetched from boss-calendar so the
    // sim shares ONE source of truth with the dispatcher + service
    // rather than hardcoding holiday lists. Collect the distinct codes
    // the tenant's counterparty + periodic specs reference (plus
    // us-banking, which the sampler always needs), fetch each, and feed
    // the result into `load`. A calendar that's absent / fails to fetch
    // is logged + skipped; the engine's all-business fallback covers
    // the miss. The tenant.toml parse here is cheap + repeated inside
    // `load`; collecting codes off it keeps the fetch list data-driven.
    let codes = {
        let seeds = seeds_path.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let tenant = boss_sim::shape_driven::TenantConfig::load(&seeds.join("tenant.toml"))
                .with_context(|| format!("loading tenant config from {}", seeds.display()))?;
            Ok(boss_brewery_engine::brewery_calendar_codes(&tenant))
        })
        .await
        .context("spawn_blocking collect calendar codes")??
    };
    info!(?codes, "fetching business calendars from boss-calendar");
    let calendars = boss_brewery_engine::fetch_calendars(&api_base, &codes).await;
    info!(
        fetched = calendars.len(),
        requested = codes.len(),
        "business calendars fetched"
    );

    // Authoritative actor identity for the cockpit: emp → role from the
    // running system (boss-people), not the sim's seed roster. Fetched once
    // at boot and overlaid on the seed-derived map below, so an assignee the
    // system holds resolves to its real role instead of `unassigned-role`.
    // Blocking client → spawn_blocking. Empty on failure (caller keeps the
    // seed fallback).
    let system_emp_roles = {
        let api_base = api_base.clone();
        tokio::task::spawn_blocking(move || boss_brewery_engine::fetch_employees(&api_base))
            .await
            .unwrap_or_default()
    };
    info!(
        count = system_emp_roles.len(),
        "employee roster fetched from boss-people"
    );

    // Long-lived engine state. Held across every tick so
    // counterparty pending-action chains survive the 1-day-chunk
    // boundaries. spawn_blocking + Mutex so the sync engine can
    // mutate it without crossing the tokio boundary.
    //
    // Recurring financial work (payroll, 941, income-tax) runs as
    // `[periodic.*]` specs in tenant.toml that open honest
    // JobKinds whose terminal step's side-effect POSTs to the
    // canonical `/api/ledger/*` endpoint — every such event has a
    // JobKind / Step / audit-trail behind it.
    // Vendor behaviors from the model (boss-inventory) — the simulator reads
    // each vendor's supply profile and synthesizes one supplier counterparty
    // chain per vendor (paced by its lead time + fulfilment), so the vendor
    // responds to its own procurement with an invoice. Blocking client →
    // spawn_blocking. Empty on failure (no synthesized chains).
    let vendor_behaviors = {
        let api_base = api_base.clone();
        tokio::task::spawn_blocking(move || boss_brewery_engine::fetch_vendors(&api_base))
            .await
            .unwrap_or_default()
    };
    info!(
        count = vendor_behaviors.len(),
        "vendor behaviors fetched from boss-inventory"
    );

    let engine = Arc::new(Mutex::new(
        tokio::task::spawn_blocking({
            let seeds = seeds_path.clone();
            // Boot from the control-plane config override if an operator
            // has set one (else the seed tenant.toml) — the "edit +
            // restart" config model — feeding in the calendars + vendor
            // behaviors fetched above so every engine shares one source of
            // truth.
            move || -> anyhow::Result<BreweryEngineState> {
                let tenant = sim_control::effective_tenant(&seeds)?;
                BreweryEngineState::load_with_tenant(&seeds, tenant, calendars, vendor_behaviors)
            }
        })
        .await
        .context("spawn_blocking engine load")??,
    ));
    info!("engine state loaded — counterparty + periodic state will persist across ticks");

    // Restart resilience for the CounterpartyEngine queue. The
    // BTreeMap-keyed pending-settlement queue holds emissions
    // scheduled for future dates (AR collections 30 days out,
    // bank-ach 30bd, vendor invoices); without an on-disk
    // checkpoint a daemon restart would drop them all and the
    // brewery's collected-cash projection would stall. We
    // checkpoint the queue to a JSON file after every successful
    // sim-day and reload it on boot. Path is configurable via
    // BOSS_SIM_STATE_DIR; default /var/lib/boss-sim so it survives
    // reseed but not a host-image rebuild.
    let state_dir =
        std::env::var("BOSS_SIM_STATE_DIR").unwrap_or_else(|_| "/var/lib/boss-sim".to_string());
    let state_path = PathBuf::from(&state_dir).join("counterparty-queue.json");
    if let Some(parent) = state_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!(
            path = %parent.display(),
            error = %e,
            "could not create sim-state dir; counterparty queue won't persist"
        );
    }
    match boss_sim::engines::CounterpartyState::load_from_file(&state_path) {
        Ok(loaded) if loaded.pending_count() > 0 => {
            // Epoch-staleness gate. The rewind path deletes this file
            // before its restart, but an epoch restart that happens
            // while the daemon is DOWN leaves no rewind signal at the
            // next boot — the checkpoint's own sim-date stamp is the
            // only provenance. A stamp in the sim future (or missing)
            // marks the previous epoch's queue: emissions against
            // invoices the reset wiped. Clock readiness is guaranteed
            // by wait_until_ready above, so an unreadable clock here is
            // exceptional — fail closed (discard) rather than risk
            // phantom settlements.
            let pending = loaded.pending_count();
            match read_clock().await {
                Ok(clock) if !loaded.stale_for(clock.current_sim_date) => {
                    engine
                        .lock()
                        .expect("engine mutex poisoned")
                        .counterparty
                        .restore_state(loaded);
                    info!(
                        state_path = %state_path.display(),
                        pending,
                        "restored counterparty pending queue from disk"
                    );
                }
                Ok(clock) => {
                    warn!(
                        state_path = %state_path.display(),
                        saved_on = ?loaded.saved_on,
                        sim_today = %clock.current_sim_date,
                        pending,
                        "counterparty checkpoint is from a previous epoch — discarding it"
                    );
                    if let Err(e) = cleanup_for_epoch_rewind(&state_path) {
                        warn!(error = %e, "could not remove stale checkpoint file");
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        pending,
                        "clock unreadable at checkpoint load — discarding checkpoint (fail closed: a stale queue writes phantom settlements)"
                    );
                    if let Err(e) = cleanup_for_epoch_rewind(&state_path) {
                        warn!(error = %e, "could not remove unverifiable checkpoint file");
                    }
                }
            }
        }
        Ok(_) => info!(
            state_path = %state_path.display(),
            "counterparty pending queue: empty checkpoint or fresh start"
        ),
        Err(e) => warn!(
            state_path = %state_path.display(),
            error = %e,
            "could not load counterparty queue — starting fresh"
        ),
    }

    // Long-lived live-API output. CRITICAL: must be held across
    // every tick of a sim-day so the per-day batch buffers
    // (`day_job_creates`, `day_invoices`, `day_shipments`,
    // `day_step_creates`, `day_step_updates`, etc) accumulate
    // across the day's ticks before flushing. A per-tick
    // LiveApiOutput would scope-die at the close of each
    // spawn_blocking closure, so the end_of_day flush would run
    // against an empty buffer and POST nothing — the daemon would
    // tick the calendar but produce zero events in audit_log
    // (only the AR-aging counterparty's per-tick emit_event PUT
    // chain would be visible). So we hoist the output up here,
    // share via Arc<Mutex>, and lock per tick alongside the
    // engine.
    // Shared per-actor API-activity tally (cockpit telemetry). One handle,
    // written by both the workforce + the live output, snapshotted into
    // telemetry each tick.
    let api_activity = boss_sim::api_activity::new_handle();
    // Account id → class, so the cockpit rolls each customer order up under
    // the buying account's CLASS (one row per class + a distinct-account
    // count) rather than one row per account — the customer analogue of the
    // employee-by-role rollup. Interim tenant classification pending the
    // account_type Class registry (see examples/brewery/seeds/tenant.toml).
    let account_classes: HashMap<String, String> = {
        let guard = engine.lock().expect("engine mutex poisoned");
        let mut m: HashMap<String, String> = guard
            .state
            .subjects
            .get("account")
            .into_iter()
            .flatten()
            .map(|id| (id.clone(), brewery_account_class(id)))
            .collect();
        // The storefront / taproom aggregate account is hardcoded in the
        // order JobKinds' metadata, not sampled from the demand pool.
        m.entry("acc-direct-shop".to_string())
            .or_insert_with(|| "retail".to_string());
        m
    };
    let output = Arc::new(Mutex::new({
        let mut o = LiveApiOutput::new(&api_base)
            .with_api_activity(api_activity.clone())
            .with_account_classes(account_classes);
        register_default_event_routes(&mut o);
        o
    }));
    info!("live-API output constructed; per-day batch buffers persist across ticks");

    // External-party callback receiver. The dispatcher PUSHES the events its
    // counterparties care about (step.done.billing → AR-aging collections,
    // courier scans, …) to BOSS_EVENT_WEBHOOK_URL; this binds
    // BOSS_SIM_CALLBACK_BIND to RECEIVE them into a queue. We drain that queue
    // onto `engine.bus` at the start of each sim-day (the first tick), so the
    // CounterpartyEngine reacts to live events and emits its deferred response
    // back through the public API — exactly what `run_brewery_live` does in the
    // regen bin. Held for the whole process lifetime so the background listener
    // thread stays up. No-op (empty, never-filled queue) when
    // BOSS_SIM_CALLBACK_BIND is unset, so non-demo deployments are unaffected.
    let callbacks = start_callback_receiver();

    // The workforce executor — claims Ready steps and completes Active
    // steps once their duration has elapsed against the clock. Built once
    // and driven every tick (see the per-tick call below). Without it the
    // daemon GENERATES jobs but never WORKS them: every assigned worker-
    // step (handoff / scheduling / production) sits Ready forever and the
    // brewery never actually brews — only dispatcher-resolved gates +
    // counterparty posts move, a false-healthy facade. We do NOT configure
    // the clock here; clock-api owns it (primed by reset-to-baseline).
    let workforce = {
        let guard = engine.lock().expect("engine mutex poisoned");
        // emp → role, inverted from the seeded roster, so the cockpit can
        // attribute each PUT /steps to the worker's role.
        let mut emp_roles: HashMap<String, String> = HashMap::new();
        for (role, emps) in &guard.state.employees_by_role {
            for emp in emps {
                emp_roles.insert(emp.clone(), role.clone());
            }
        }
        // Overlay the system's authoritative emp → role on top, so an
        // assignee the system holds resolves to its real role even when the
        // seed roster lags (the fix for `unassigned-role`).
        for (emp, role) in &system_emp_roles {
            emp_roles.insert(emp.clone(), role.clone());
        }
        Arc::new(Mutex::new(
            build_workforce(&guard, &api_base)
                .with_actor_telemetry(api_activity.clone(), emp_roles),
        ))
    };
    info!("workforce executor constructed; driving assigned steps each tick");

    // Control + telemetry server: exposes how the daemon is engaging the
    // public API (the Cockpit reads this) over a localhost-only port.
    // boss-simulator proxies to it. Background task on this runtime,
    // sharing the telemetry the tick loop refreshes each tick.
    let telemetry = Arc::new(Mutex::new(sim_control::SimTelemetry::new(
        "automation:sim".to_string(),
        "system-sim".to_string(),
        api_base.clone(),
    )));
    {
        let control_bind =
            std::env::var("BOSS_SIM_CONTROL_BIND").unwrap_or_else(|_| "127.0.0.1:7011".to_string());
        let telemetry = telemetry.clone();
        let seeds = seeds_path.clone();
        tokio::spawn(async move {
            if let Err(e) = sim_control::serve(control_bind, telemetry, seeds).await {
                warn!(error = %e, "sim control + telemetry server exited");
            }
        });
    }

    // ---- pre-Go readiness gate ------------------------------------------------
    // Before the tick loop opens its first Job, verify the stack can actually
    // CARRY a Job to a terminal state — not merely answer /health 200. The
    // failure this guards: the dispatcher process is up + health-green but its
    // durable assignment consumer never bound (e.g. JetStream not ready at cold
    // start under a no-restart launcher), so ready Steps are never assigned, the
    // workforce idles, and Jobs pile up `open` unbounded.
    //
    // We HOLD here (creating no Jobs) until every check is green rather than
    // flood a broken stack with thousands of Jobs that can't progress — and we
    // do NOT exit, so the container + every service + the SPA stay UP for
    // inspection (curl /api/dispatcher/readyz, read the ✓/✗ report below).
    // `wait_until_ready` logs each pass and keeps waiting; past
    // BOSS_SIM_READINESS_TIMEOUT_SECS (default 180) it warns louder + slows the
    // poll. Bypass with BOSS_SIM_SKIP_READINESS=1.
    if std::env::var("BOSS_SIM_SKIP_READINESS")
        .map(|v| v != "0" && v != "false")
        .unwrap_or(false)
    {
        warn!("BOSS_SIM_SKIP_READINESS set — skipping the pre-Go readiness gate");
    } else {
        let timeout_secs: u64 = std::env::var("BOSS_SIM_READINESS_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(180);
        wait_until_ready(&api_base, timeout_secs).await;
    }

    // Main tick loop. Each iteration: read clock, advance one
    // day, persist new date. Non-fatal errors (transient API
    // hiccups, NATS blips) log + retry on the next tick. The
    // engine is mutex-guarded so two ticks can't race; with
    // tick_interval_seconds ≥ 1 they realistically don't.
    let mut global_tick: u64 = 0;
    // The last sim-day the daemon has fully run; persists across outer
    // passes so a day is never re-run while the warp clock sits on it
    // (the cold-start over-fire fix). `None` until the first pass.
    let mut cursor: Option<NaiveDate> = None;
    loop {
        let clock = read_clock().await?;
        if clock.paused {
            info!(
                current_sim_date = %clock.current_sim_date,
                "sim_clock paused; sleeping {}s",
                clock.tick_interval_seconds
            );
            if let Ok(mut t) = telemetry.lock() {
                t.note_cadence(cadence_of(&clock, None));
            }
            tokio::time::sleep(Duration::from_secs(clock.tick_interval_seconds as u64)).await;
            continue;
        }

        // Epoch end: when current_sim_date catches the configured
        // end-of-epoch (typically 2027-04-02 = 1 year past
        // epoch_start), trigger an auto-restart. POSTs the same
        // restart-epoch endpoint the SimClockBadge "Restart epoch"
        // button hits — trim-not-truncate the live-tick audit_log
        // rows past the canonical baseline, replay the surviving
        // seed, rewind sim_clock to epoch_start, unpause. Daemon
        // observes restart_in_progress=true via the next tick
        // (skips advancement until it clears), then picks up at
        // epoch_start and runs the next 12 months. Operator can
        // still edit sim_clock directly to extend the epoch
        // instead of looping. Fallback: if the endpoint isn't
        // reachable, set paused=true so the loop idles gracefully
        // and an operator can run `reset-to-baseline.sh` by hand.
        if let Some(end) = clock.epoch_end_date
            && clock.current_sim_date >= end
            && !clock.restart_in_progress
        {
            info!(
                current_sim_date = %clock.current_sim_date,
                epoch_end_date = %end,
                "sim epoch complete — triggering auto-restart"
            );
            match trigger_restart_epoch(&api_base).await {
                Ok(()) => info!(
                    "restart-epoch dispatched; daemon will idle until restart_in_progress clears"
                ),
                Err(e) => {
                    warn!(error = %e, "restart-epoch dispatch failed; pausing for manual reset");
                    set_paused(true).await?;
                }
            }
            tokio::time::sleep(Duration::from_secs(clock.tick_interval_seconds as u64)).await;
            continue;
        }
        if clock.restart_in_progress {
            info!(
                current_sim_date = %clock.current_sim_date,
                "restart-epoch in progress; idling"
            );
            tokio::time::sleep(Duration::from_secs(clock.tick_interval_seconds as u64)).await;
            continue;
        }

        // The daemon tick is the SIM tick, not the SIM-DAY. Read
        // `tick_duration` off the loaded tenant.toml — `"1d"`
        // advances 1 sim-day per loop iteration; `"1h"` advances 1
        // sim-hour per iteration with the wall-clock budget split
        // 24-ways. Same `1 sim-year per real hour` budget; events
        // spread across the wall-clock window instead of bursting.
        //
        // Wall-clock per tick: `tick_interval_seconds /
        // ticks_per_day`. At hourly ticks with 10s/sim-day base:
        // 10 / 24 ≈ 0.42s real per sim-hour. That's the visual
        // breathing room the landing-page rework needs.
        //
        // Iteration count: `days_per_tick × ticks_per_day` total
        // sim-ticks. We advance one tick per iteration + sleep
        // between. End-of-sim-day rollup fires on the last tick
        // of each sim-day (drains bus + flushes per-day SimOutput
        // buffer). Calendar advances after the rollup.
        let ticks_per_day = {
            let guard = engine.lock().expect("engine mutex poisoned");
            guard.tenant.meta.ticks_per_day()
        };
        assert!(ticks_per_day > 0);
        let per_tick_sleep_ms =
            (clock.tick_interval_seconds as u64 * 1000) / (ticks_per_day as u64);

        // Run only sim-days the daemon hasn't run yet — (cursor,
        // current_sim_date], each exactly once. While the warp clock sits
        // on a day this is empty and the daemon idles instead of re-running
        // it (the old loop re-ran current_sim_date every pass, double-firing
        // periodics + rate jobs at cold-start). days_per_tick caps the
        // per-pass catch-up batch.
        // If the epoch was restarted underneath us the clock has jumped
        // backward to epoch_start; drop the now-stale cursor so the new
        // epoch runs from day one instead of idling until the clock climbs
        // a full year back to it (the post-auto-restart stall).
        let rewound = cursor_after_clock(cursor, clock.current_sim_date);
        if rewound != cursor {
            // Epoch rewind: the DB was reset to baseline, but THIS
            // process still holds the previous epoch's world in memory —
            // account/vendor rosters, counterparty pending queue,
            // half-filled day buffers, workforce claims. Driving the new
            // epoch with that state manufactures work for subjects the
            // reset wiped (the phantom-account / replay-divergence class,
            // 2026-07-13). Resetting every carrier in place is fragile —
            // the boot path already rebuilds all of it from seeds + the
            // API, so restart the process instead: drop the stale on-disk
            // checkpoint (boot would reload it) and exit 75, the same
            // deliberate-restart contract the config-apply path uses
            // (systemd Restart=on-failure brings us back in ~2s).
            info!(
                stale_cursor = ?cursor,
                current_sim_date = %clock.current_sim_date,
                "epoch rewind detected; discarding previous epoch's checkpoint and exiting for a clean restart"
            );
            if let Err(e) = cleanup_for_epoch_rewind(&state_path) {
                warn!(
                    path = %state_path.display(),
                    error = %e,
                    "could not remove stale counterparty checkpoint; the boot-time staleness gate will discard it instead"
                );
            }
            std::process::exit(75);
        }

        let days = days_to_run(
            cursor,
            clock.current_sim_date,
            (clock.days_per_tick.max(1)) as u32,
        );
        let Some(&start_day) = days.first() else {
            // No new sim-day yet: drive in-flight workforce, then idle.
            if let Err(e) = run_workforce_pass(workforce.clone()).await {
                warn!(error = %e, "workforce pass failed; continuing");
            }
            tokio::time::sleep(Duration::from_millis(per_tick_sleep_ms)).await;
            continue;
        };
        let total_ticks = (days.len() as u32) * ticks_per_day;
        let mut break_outer = false;
        for tick_offset in 0..total_ticks {
            let day_offset = tick_offset / ticks_per_day;
            let tick_idx = tick_offset % ticks_per_day;
            let day = start_day
                .checked_add_signed(chrono::Duration::days(day_offset as i64))
                .expect("date overflow");
            if let Err(e) = advance_one_tick(
                engine.clone(),
                output.clone(),
                callbacks.clone(),
                day,
                tick_idx,
            )
            .await
            {
                warn!(error = %e, day = %day, tick_idx, "tick advance failed; retrying next interval");
                break_outer = true;
                break;
            }
            // End-of-sim-day rollup on the last tick of each
            // sim-day (drains bus + flushes per-day buffer).
            if tick_idx + 1 == ticks_per_day {
                if let Err(e) = end_of_day(engine.clone(), output.clone(), day).await {
                    warn!(error = %e, day = %day, "end-of-day flush failed; retrying next interval");
                    break_outer = true;
                    break;
                }
                // Checkpoint the counterparty queue to disk after
                // each sim-day flush, stamped with the sim-day so a
                // later boot can tell this epoch's checkpoint from a
                // previous one's. Best-effort; a failed write logs
                // but doesn't break the tick (the next day will
                // retry the checkpoint).
                if let Ok(guard) = engine.lock() {
                    let snapshot = guard.counterparty.checkpoint(day);
                    if let Err(e) = snapshot.save_to_file(&state_path) {
                        warn!(
                            day = %day,
                            error = %e,
                            "counterparty checkpoint save failed"
                        );
                    }
                }
                cursor = Some(day);
            }
            // Drive the workforce after this tick's generation: claim
            // newly-Ready steps and complete Active steps whose duration
            // has elapsed. Runs every tick (operating day or not) so the
            // tail of in-flight work still settles. Non-fatal — a transient
            // blip retries next tick rather than killing the daemon.
            if let Err(e) = run_workforce_pass(workforce.clone()).await {
                warn!(error = %e, day = %day, tick_idx, "workforce pass failed; continuing");
            }
            // Refresh the telemetry the control server serves: this tick's
            // public-API engagement (workforce step transitions + per-domain
            // writes) + cadence. Cheap post-tick snapshot copy.
            global_tick += 1;
            {
                let wf = workforce
                    .lock()
                    .map(|w| w.stats.clone())
                    .unwrap_or_default();
                let api = output.lock().map(|o| o.stats.clone()).unwrap_or_default();
                let actors = boss_sim::api_activity::snapshot(&api_activity);
                if let Ok(mut t) = telemetry.lock() {
                    t.record_tick(
                        global_tick,
                        cadence_of(&clock, Some(day)),
                        &wf,
                        &api,
                        actors,
                    );
                }
            }
            // Sleep between sim-ticks so events spread across the
            // wall-clock window. Skip the sleep on the very last
            // tick of the loop; the outer loop's sleep at the
            // bottom covers it (preserves the
            // tick_interval_seconds-per-sim-day total budget).
            if tick_offset + 1 < total_ticks {
                tokio::time::sleep(Duration::from_millis(per_tick_sleep_ms)).await;
            }
        }

        if !break_outer && let Some(ran_through) = cursor {
            // clock-api is the single writer for current_sim_date; this
            // daemon only drives + logs. We never write sim_clock from here
            // — a late write could regress it behind clock-api and show the
            // SPA a date older than the events being written.
            info!(
                ran_through = %ran_through,
                days_this_pass = days.len(),
                ticks_per_day,
                "pass complete"
            );
        }

        tokio::time::sleep(Duration::from_millis(per_tick_sleep_ms)).await;
    }
}

/// Class (cockpit rollup label) for a brewery Account id. The synthetic
/// demand pool `acc-bigseed-*` are wholesale (B2B) customers; the
/// storefront / taproom aggregate account is retail; anything else rolls up
/// generically. Interim tenant classification pending the account_type Class
/// registry (see examples/brewery/seeds/tenant.toml).
fn brewery_account_class(account_id: &str) -> String {
    if account_id.starts_with("acc-bigseed") {
        "wholesale-customer".to_string()
    } else if account_id == "acc-direct-shop" {
        "retail".to_string()
    } else {
        "account".to_string()
    }
}

/// Decide which sim-days to run this pass, given `cursor` (the last day
/// the daemon already ran) and `target` (the clock's current day).
/// Forward-only, each day exactly once — the daemon never re-runs a day
/// it already processed.
///
/// This is the fix for the cold-start over-firing: the old loop ran
/// `current_sim_date` every pass regardless, so while the warp clock sat
/// on a day the daemon re-ran it — double-firing periodics (6× facility/
/// utilities/tax) and re-spawning rate jobs (~2× early revenue). Mirrors
/// the dispatcher's `schedule_runner::advance_cursor`, except first
/// observation *runs* the current day (the daemon builds the demo
/// forward from wherever the clock sits — the epoch on a fresh reset)
/// rather than establishing a fire-nothing baseline.
///
/// - First observation (`cursor == None`) → run `target`.
/// - No advance (`target <= cursor`, incl. an epoch-restart rewind) → run
///   nothing; the daemon idles until the clock ticks over.
/// - Forward (`target > cursor`) → the half-open range `(cursor, target]`,
///   capped at `max_batch` days this pass. The cursor advances, so the
///   next pass continues — catch-up never skips a day, only spreads it.
fn days_to_run(cursor: Option<NaiveDate>, target: NaiveDate, max_batch: u32) -> Vec<NaiveDate> {
    match cursor {
        None => vec![target],
        Some(c) if target <= c => Vec::new(),
        Some(c) => {
            let mut days = Vec::new();
            let mut d = c;
            while (days.len() as u32) < max_batch.max(1) {
                match d.succ_opt() {
                    Some(n) if n <= target => {
                        d = n;
                        days.push(d);
                    }
                    _ => break,
                }
            }
            days
        }
    }
}

/// Reset the day cursor when the clock jumps backward. A restart-epoch
/// rewinds `current_sim_date` to `epoch_start`, but the daemon's in-memory
/// cursor still holds the last day of the epoch that just finished — a year
/// ahead. Left as-is, `days_to_run(cursor, current)` returns empty until the
/// clock climbs a full year back to the stale cursor, so the daemon idles
/// silently the whole time (the "running but no activity for ~8.7h after an
/// auto-restart" stall). A backward jump only happens on a rewind, so drop
/// the cursor to `None` and let the new epoch resume from day one. A
/// monotonic (forward-or-equal) clock keeps its cursor unchanged.
fn cursor_after_clock(cursor: Option<NaiveDate>, current: NaiveDate) -> Option<NaiveDate> {
    match cursor {
        Some(c) if current < c => None,
        other => other,
    }
}

/// Remove the previous epoch's counterparty checkpoint before the
/// rewind restart. Missing file is success — there may simply have
/// been no end-of-day checkpoint yet this epoch.
fn cleanup_for_epoch_rewind(state_path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(state_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod day_cursor_tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn epoch_rewind_cleanup_removes_checkpoint() {
        let dir = std::env::temp_dir().join(format!("rewind-cleanup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("counterparty-queue.json");
        std::fs::write(&path, "{}").unwrap();
        cleanup_for_epoch_rewind(&path).unwrap();
        assert!(!path.exists(), "stale checkpoint must be removed");
        // Missing file is success — no checkpoint yet this epoch.
        cleanup_for_epoch_rewind(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_observation_runs_current_day() {
        // Cold start: cursor None, clock at the epoch → run the epoch day,
        // so the daemon still builds the demo forward from day 0.
        assert_eq!(days_to_run(None, d(2025, 4, 1), 31), vec![d(2025, 4, 1)]);
    }

    #[test]
    fn no_advance_runs_nothing() {
        // The warp clock hasn't ticked over: same day → idle. (THE bug:
        // the old loop re-ran this day, double-firing the periodics.)
        assert!(days_to_run(Some(d(2025, 4, 5)), d(2025, 4, 5), 31).is_empty());
    }

    #[test]
    fn backward_clock_runs_nothing() {
        // An epoch restart rewinds the clock; never re-run on a rewind.
        assert!(days_to_run(Some(d(2025, 4, 5)), d(2025, 4, 1), 31).is_empty());
    }

    #[test]
    fn single_day_advance_runs_that_day_once() {
        assert_eq!(
            days_to_run(Some(d(2025, 4, 5)), d(2025, 4, 6), 31),
            vec![d(2025, 4, 6)]
        );
    }

    #[test]
    fn multi_day_gap_runs_each_missing_day_in_order() {
        // Daemon fell behind: clock jumped 04-05 → 04-08. Run 06, 07, 08
        // (each once, oldest first) — no skip, no re-run.
        assert_eq!(
            days_to_run(Some(d(2025, 4, 5)), d(2025, 4, 8), 31),
            vec![d(2025, 4, 6), d(2025, 4, 7), d(2025, 4, 8)]
        );
    }

    #[test]
    fn catch_up_is_capped_per_pass_without_skipping() {
        // A big gap caps at max_batch this pass; the cursor advances so the
        // next pass continues from there (no day skipped).
        let first = days_to_run(Some(d(2025, 4, 1)), d(2025, 5, 1), 3);
        assert_eq!(first, vec![d(2025, 4, 2), d(2025, 4, 3), d(2025, 4, 4)]);
        let next = days_to_run(Some(d(2025, 4, 4)), d(2025, 5, 1), 3);
        assert_eq!(next, vec![d(2025, 4, 5), d(2025, 4, 6), d(2025, 4, 7)]);
    }

    #[test]
    fn cursor_survives_a_forward_or_equal_clock() {
        // The common case — a monotonic clock keeps its cursor.
        assert_eq!(
            cursor_after_clock(Some(d(2025, 4, 5)), d(2025, 4, 5)),
            Some(d(2025, 4, 5))
        );
        assert_eq!(
            cursor_after_clock(Some(d(2025, 4, 5)), d(2025, 4, 6)),
            Some(d(2025, 4, 5))
        );
        assert_eq!(cursor_after_clock(None, d(2025, 4, 5)), None);
    }

    #[test]
    fn cursor_resets_on_epoch_rewind_so_the_new_epoch_runs() {
        // The stall: after a restart-epoch, current_sim_date rewinds to
        // epoch_start while the cursor still holds the last day of the
        // finished epoch (a year ahead). Without the detection days_to_run
        // idles for a full year. `cursor_after_clock` is now the rewind
        // DETECTOR — the daemon exits(75) on it for a clean-state restart
        // (the fresh process boots with cursor None and runs from day
        // one, the second assertion below).
        let stale = Some(d(2026, 3, 31)); // last day of the finished epoch
        let epoch_start = d(2025, 4, 1); // clock rewound here
        assert_eq!(cursor_after_clock(stale, epoch_start), None);
        // Regression guard: stale cursor → idle (the bug); reset cursor → runs.
        assert!(days_to_run(stale, epoch_start, 31).is_empty());
        assert_eq!(
            days_to_run(cursor_after_clock(stale, epoch_start), epoch_start, 31),
            vec![epoch_start]
        );
    }

    #[test]
    fn account_class_buckets_the_brewery_pools() {
        // The acc-bigseed-* demand pool rolls up as one B2B/wholesale class;
        // the storefront/taproom account is retail; anything else is generic.
        assert_eq!(
            brewery_account_class("acc-bigseed-0000"),
            "wholesale-customer"
        );
        assert_eq!(
            brewery_account_class("acc-bigseed-0049"),
            "wholesale-customer"
        );
        assert_eq!(brewery_account_class("acc-direct-shop"), "retail");
        assert_eq!(brewery_account_class("acc-mystery"), "account");
    }
}

#[derive(Debug)]
struct Clock {
    current_sim_date: NaiveDate,
    /// How many sim-days to drive per outer-loop iteration. With
    /// the formula clock advancing on its own, this is the
    /// simulator's BATCH SIZE — how many days of events to emit
    /// per wake-up — not a property of the clock itself.
    days_per_tick: i32,
    /// Wall-seconds between simulator wake-ups. The simulator
    /// polls clock-api on each wake, decides how many sim-days
    /// have passed, and emits events for them.
    tick_interval_seconds: i32,
    paused: bool,
    epoch_start_date: Option<NaiveDate>,
    epoch_end_date: Option<NaiveDate>,
    warp_factor: Option<f64>,
    restart_in_progress: bool,
}

/// Read the current clock state by polling clock-api directly.
/// clock-api owns time end-to-end — the formula computes sim_now
/// on every read, so the simulator never writes time, only reads.
async fn read_clock() -> Result<Clock> {
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let url = format!("{}/api/clock/now", clock_url.trim_end_matches('/'));
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let body: serde_json::Value = resp.json().await.with_context(|| "decode /api/clock/now")?;
    let now: chrono::DateTime<chrono::Utc> = body
        .get("now")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc))
        .context("missing or unparseable /api/clock/now `now`")?;
    let epoch_end_date = body
        .get("epoch_end")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    let paused = body
        .get("paused")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let restart_in_progress = body
        .get("restart_in_progress")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let days_per_tick: i32 = std::env::var("BOSS_SIM_DAYS_PER_TICK")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let tick_interval_seconds: i32 = std::env::var("BOSS_SIM_TICK_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let epoch_start_date = body
        .get("epoch_start")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    let warp_factor = body.get("warp_factor").and_then(|v| v.as_f64());
    Ok(Clock {
        current_sim_date: now.date_naive(),
        days_per_tick,
        tick_interval_seconds,
        paused,
        epoch_start_date,
        epoch_end_date,
        warp_factor,
        restart_in_progress,
    })
}

/// Build a telemetry [`sim_control::Cadence`] snapshot from the current
/// clock state. `day` overrides the sim-date for a specific tick (the
/// loop reports each tick's own day); `None` falls back to the clock's
/// current_sim_date (idle / paused branches).
fn cadence_of(clock: &Clock, day: Option<NaiveDate>) -> sim_control::Cadence {
    let d = day.unwrap_or(clock.current_sim_date);
    sim_control::Cadence {
        sim_date: Some(d.format("%Y-%m-%d").to_string()),
        paused: clock.paused,
        epoch_start: clock
            .epoch_start_date
            .map(|x| x.format("%Y-%m-%d").to_string()),
        epoch_end: clock
            .epoch_end_date
            .map(|x| x.format("%Y-%m-%d").to_string()),
        warp_factor: clock.warp_factor,
        days_per_tick: Some(clock.days_per_tick),
        tick_interval_seconds: Some(clock.tick_interval_seconds),
    }
}

/// POST /api/jobs/sim-clock/restart-epoch — same endpoint the
/// SimClockBadge "Restart epoch" button hits. Returns Ok on
/// 200/202; any other status (or a connection failure) becomes
/// an error so the caller can fall back to pause-and-wait.
async fn trigger_restart_epoch(api_base: &str) -> Result<()> {
    // api_base is either `direct://127.0.0.1` (in-process loopback
    // marker) or a real http(s) origin. Translate the loopback to
    // the canonical jobs-api port; everything else gets a literal
    // POST.
    let base = if let Some(host) = api_base.strip_prefix("direct://") {
        format!("http://{host}:7900")
    } else {
        api_base.trim_end_matches('/').to_string()
    };
    let url = format!("{base}/api/jobs/sim-clock/restart-epoch");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let resp = client
        .post(&url)
        .header("x-boss-user", "{\"id\":\"system\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}")
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("restart-epoch returned {status}: {body}");
    }
    Ok(())
}

/// Pause the clock by hitting clock-api's /pause endpoint.
/// clock-api owns clock state; the simulator is a pure consumer.
async fn set_paused(paused: bool) -> Result<()> {
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let path = if paused {
        "/api/clock/pause"
    } else {
        "/api/clock/resume"
    };
    let url = format!("{}{}", clock_url.trim_end_matches('/'), path);
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?
        .post(&url)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?
        .error_for_status()
        .with_context(|| format!("POST {url}"))?;
    Ok(())
}

/// Advance the held engine state by exactly one tick (1 sim-hour
/// at hourly granularity, 1 sim-day at day granularity, etc).
/// Pairs with [`end_of_day`] which fires after the last tick of
/// each sim-day.
///
/// On the first tick of each sim-day (`tick_idx == 0`) the
/// external-party `callbacks` queue is drained onto `engine.bus`
/// before the tick runs — mirroring `run_brewery_live`, which
/// drains the same queue before that day's ticks so the
/// CounterpartyEngine reacts to live dispatcher pushes within the
/// same day's `brewery_end_of_day` rollup. Done inside this
/// `spawn_blocking` (where the engine is already locked) so the
/// bus mutation co-locates with the tick that consumes it, with
/// no extra lock acquisition on the hot path. Empty/no-op when
/// `BOSS_SIM_CALLBACK_BIND` is unset.
async fn advance_one_tick(
    engine: Arc<Mutex<BreweryEngineState>>,
    output: Arc<Mutex<LiveApiOutput>>,
    callbacks: Arc<Mutex<VecDeque<SimBusEvent>>>,
    day: NaiveDate,
    tick_idx: u32,
) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<_> {
        let mut engine_guard = engine.lock().expect("engine mutex poisoned");
        let mut output_guard = output.lock().expect("output mutex poisoned");
        // Drain on the first tick of an OPERATING sim-day only. The bus is
        // day-bounded and `brewery_end_of_day` clears it, while the
        // CounterpartyEngine that consumes these events runs inside
        // `run_brewery_one_tick` — which no-ops on non-operating days. Draining
        // there would clear the events un-reacted-to. Holding off until the next
        // operating day's first tick (the queue survives in the meantime)
        // matches `run_brewery_live`, whose drain sits inside the same
        // `is_operating_day` guard.
        if tick_idx == 0
            && engine_guard.tenant.meta.is_operating_day(day)
            && let Ok(mut q) = callbacks.lock()
        {
            for ev in q.drain(..) {
                engine_guard.bus.emit(ev);
            }
        }
        run_brewery_one_tick(&mut engine_guard, day, tick_idx, &mut *output_guard)
    })
    .await
    .context("spawn_blocking advance_one_tick")??;
    Ok(())
}

/// End-of-sim-day rollup — drains the engine's per-day bus +
/// flushes the per-day SimOutput buffer. Companion to
/// [`advance_one_tick`]. The output is the long-lived
/// process-scoped Arc<Mutex<LiveApiOutput>> so the per-day
/// `day_*` buffers populated across this sim-day's ticks
/// actually flush here instead of getting silently dropped at
/// the close of each per-tick spawn_blocking closure.
async fn end_of_day(
    engine: Arc<Mutex<BreweryEngineState>>,
    output: Arc<Mutex<LiveApiOutput>>,
    day: NaiveDate,
) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<_> {
        let mut engine_guard = engine.lock().expect("engine mutex poisoned");
        let mut output_guard = output.lock().expect("output mutex poisoned");
        brewery_end_of_day(&mut engine_guard, day, &mut *output_guard)
    })
    .await
    .context("spawn_blocking end_of_day")??;
    Ok(())
}

/// Drive one workforce check-in. `work_once` is `reqwest::blocking` and
/// fans out across its own thread scope, so it must run inside
/// `spawn_blocking`, not on the async runtime thread. Mirrors the per-day
/// `work_once` calls in `run_brewery_live`.
async fn run_workforce_pass(workforce: Arc<Mutex<Workforce>>) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut wf = workforce.lock().expect("workforce mutex poisoned");
        wf.work_once()
    })
    .await
    .context("spawn_blocking workforce pass")??;
    Ok(())
}

/// Drive a bounded brewery regen against a live API stack + print a
/// summary — the offline 12-month seed regen and the CI correctness
/// gate. Sync (reqwest::blocking via [`run_brewery_live`]), so the
/// `run` subcommand invokes it inside `spawn_blocking`. Assumes the
/// tenant model is already prepared (`boss-brewery-sim prepare`); it
/// drives the day-loop, it does not seed.
#[allow(clippy::too_many_arguments)]
fn run_regen(
    seeds: &Path,
    api_base: &str,
    days: u32,
    start: Option<NaiveDate>,
    warp_factor: f64,
    poll_sleep_ms: u64,
    hard_fail: bool,
    drain_pause_ms: u64,
) -> Result<()> {
    // Duration-based completion timing: pass each StepType's
    // typical_duration_hours so end_of_day computes completion
    // sim_time = LA 08:00 + duration per step instead of a uniform
    // spread — the day's audit_log reads as a realistic ops cadence.
    let step_registry = boss_jobs::step_registry::StepRegistry::v1();
    let step_durations: HashMap<String, (f64, f64)> = step_registry
        .all()
        .into_iter()
        .filter_map(|t| {
            t.typical_duration_hours
                .map(|h| (t.kind.to_string(), (h, t.typical_duration_jitter)))
        })
        .collect();

    let mut output = LiveApiOutput::new(api_base)
        .with_hard_fail(hard_fail)
        .with_drain_pause_ms(drain_pause_ms)
        .with_step_durations(step_durations);
    register_default_event_routes(&mut output);

    // Every domain-write side effect flows engine → jobs-api PUT step →
    // `step.done.<kind>` NATS event → dispatcher rule → domain HTTP API,
    // exactly like the daemon. The in-process bridges stay silent.
    let report = run_brewery_live(
        seeds,
        days,
        start,
        api_base,
        warp_factor,
        poll_sleep_ms,
        &mut output,
    )?;

    println!();
    println!("=== brewery regen summary (live-api) ===");
    println!("api_base:           {api_base}");
    println!("days simulated:     {}", report.days_simulated);
    println!("jobs created:       {}", report.jobs_created);
    println!("jobs closed:        {}", report.jobs_closed);
    println!("steps completed:    {}", report.steps_completed);
    println!();
    println!("=== LiveApiOutput stats ===");
    println!("asset_events:       {}", output.stats.asset_events);
    println!("invoices_created:   {}", output.stats.invoices_created);
    println!("invoices_updated:   {}", output.stats.invoices_updated);
    println!("shipments:          {}", output.stats.shipments);
    println!("agreements:         {}", output.stats.agreements);
    println!("jobs:               {}", output.stats.jobs);
    println!("purchase_orders:    {}", output.stats.purchase_orders);
    println!("account_notes:      {}", output.stats.account_notes);
    println!("tax_filings:        {}", output.stats.tax_filings);
    println!("bank_settlements:   {}", output.stats.bank_settlements);
    println!("days_flushed:       {}", output.stats.days_flushed);
    println!("errors:             {}", output.stats.errors);
    println!();
    Ok(())
}
