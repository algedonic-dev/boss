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
//!   BOSS_POSTGRES_URL    default postgres://boss:boss@127.0.0.1/boss
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

    // brewery-sim is a pure consumer of /api/clock/now and other
    // public HTTP APIs; it never touches the DB directly. The
    // DATABASE_URL env var (BOSS_POSTGRES_URL) is read by the
    // boss-brewery-engine subprocess for its own purposes.

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
    let engine = Arc::new(Mutex::new(
        tokio::task::spawn_blocking({
            let seeds = seeds_path.clone();
            move || BreweryEngineState::load(&seeds)
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
            let pending = loaded.pending_count();
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
    let output = Arc::new(Mutex::new({
        let mut o = LiveApiOutput::new(&api_base);
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
        Arc::new(Mutex::new(build_workforce(&guard, &api_base)))
    };
    info!("workforce executor constructed; driving assigned steps each tick");

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
    loop {
        let clock = read_clock().await?;
        if clock.paused {
            info!(
                current_sim_date = %clock.current_sim_date,
                "sim_clock paused; sleeping {}s",
                clock.tick_interval_seconds
            );
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
        let total_ticks = (clock.days_per_tick as u32) * ticks_per_day;
        let per_tick_sleep_ms =
            (clock.tick_interval_seconds as u64 * 1000) / (ticks_per_day as u64);

        let mut last_advanced = clock.current_sim_date;
        let mut break_outer = false;
        for tick_offset in 0..total_ticks {
            let day_offset = tick_offset / ticks_per_day;
            let tick_idx = tick_offset % ticks_per_day;
            let day = clock
                .current_sim_date
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
                // each sim-day flush. Best-effort; a failed write
                // logs but doesn't break the tick (the next day
                // will retry the checkpoint).
                if let Ok(guard) = engine.lock() {
                    let snapshot = guard.counterparty.state();
                    if let Err(e) = snapshot.save_to_file(&state_path) {
                        warn!(
                            day = %day,
                            error = %e,
                            "counterparty checkpoint save failed"
                        );
                    }
                }
                last_advanced = day;
            }
            // Drive the workforce after this tick's generation: claim
            // newly-Ready steps and complete Active steps whose duration
            // has elapsed. Runs every tick (operating day or not) so the
            // tail of in-flight work still settles. Non-fatal — a transient
            // blip retries next tick rather than killing the daemon.
            if let Err(e) = run_workforce_pass(workforce.clone()).await {
                warn!(error = %e, day = %day, tick_idx, "workforce pass failed; continuing");
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

        if !break_outer {
            let next = last_advanced
                .checked_add_signed(chrono::Duration::days(1))
                .expect("date overflow");
            // clock-api is the single writer for current_sim_date;
            // this daemon just drives the advance + logs. Writing
            // sim_clock from here too would race clock-api (which
            // writes it on every /advance) — a late write landing
            // after clock-api had advanced past `next` would
            // regress sim_clock backward and the SPA's Simulator
            // panel would show a date older than the events being
            // written.
            info!(
                advanced_to = %next,
                ticks_per_day,
                total_ticks,
                "tick complete"
            );
        }

        tokio::time::sleep(Duration::from_millis(per_tick_sleep_ms)).await;
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
    epoch_end_date: Option<NaiveDate>,
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
    Ok(Clock {
        current_sim_date: now.date_naive(),
        days_per_tick,
        tick_interval_seconds,
        paused,
        epoch_end_date,
        restart_in_progress,
    })
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
    println!("messages:           {}", output.stats.messages);
    println!("account_notes:      {}", output.stats.account_notes);
    println!("tax_filings:        {}", output.stats.tax_filings);
    println!("bank_settlements:   {}", output.stats.bank_settlements);
    println!("days_flushed:       {}", output.stats.days_flushed);
    println!("errors:             {}", output.stats.errors);
    println!();
    Ok(())
}
