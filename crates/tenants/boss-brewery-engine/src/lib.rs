//! Brewery operating-engine library — shared between the
//! `boss-brewery-engine` CLI and integration tests.
//!
//! Two entry points:
//!
//! - [`run_brewery`] — convenience wrapper that uses
//!   [`InMemoryOutput`] and returns it for assertion-style use
//!   (test fixtures, the CLI's default mode).
//! - [`run_brewery_into`] — generic-output variant for running
//!   against a [`LiveApiOutput`] pointed at a real API stack so
//!   the engine's writes flow through `DomainPublisher` →
//!   `PgAuditWriter`. The 12-month seed-generation path uses
//!   this.
//!
//! See `docs/design/correctness-protocol.md`,
//! `docs/design/seed-vs-emergent-state.md`, and
//! `docs/design/projection-rebuilders.md` § E for context.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use boss_inventory::types::InventoryItem;
use chrono::NaiveDate;
use serde::Deserialize;

use boss_jobs::seed_loader::load_job_kinds_with_owning_team;
use boss_jobs::step_registry::StepRegistry;
use boss_sim::calendar::CalendarRegistry;
use boss_sim::engines::{
    CounterpartyEngine, PeriodicEngine, RunReport, SimBusEvent, SimEventBus, end_of_day_rollup,
    run_one_tick_with_handlers, run_ticks_with_handlers,
};
use boss_sim::output::{InMemoryOutput, SimOutput};
use boss_sim::rng::Rng;
use boss_sim::shape_driven::{ShapeDrivenState, TenantConfig};

/// JobKind-publish logic shared by the `boss-brewery-bootstrap`
/// binary and the future unified "prepare" step.
pub mod prepare;

/// Result of a brewery-engine run — the day-loop's RunReport
/// plus the populated InMemoryOutput so callers can assert on
/// emitted facts.
pub struct BreweryRunResult {
    pub report: RunReport,
    pub output: InMemoryOutput,
}

/// Initial inventory state. The brewery's JobKinds reference
/// part SKUs in their `consume_parts` metadata; those parts
/// must exist in the inventory items table before the
/// dispatcher fires the consume call, or the POST returns 404
/// and the side effect is silently dropped.
/// `parts.toml` enumerates every SKU plus its initial bin /
/// on-hand / reorder thresholds. The brewery-engine
/// live-api mode POSTs these to `/api/inventory/items/batch`
/// before the day loop starts. This is initial-conditions seeding
/// (allowed by `docs/design/seed-vs-emergent-state.md`); usage,
/// reorders, and PO/vendor-invoice flow emerge from the sim.
#[derive(Debug, Deserialize)]
struct PartsBundle {
    parts: Vec<InventoryItem>,
}

/// Load `parts.toml` from the seed bundle. Missing file is OK —
/// returns an empty list (older seed bundles predate the parts
/// catalog).
pub fn load_parts(seeds: &Path) -> Result<Vec<InventoryItem>> {
    let path = seeds.join("parts.toml");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let bundle: PartsBundle =
        toml::from_str(&body).with_context(|| format!("parsing {}", path.display()))?;
    Ok(bundle.parts)
}

/// Seed the canonical brewery subjects so the JobKinds have
/// targets to anchor on. Production deployments derive this
/// from live tables; the standalone runner / test path uses
/// this hand-seed.
pub fn seed_brewery_subjects(state: &mut ShapeDrivenState) {
    // Inventory + finished-goods stock live in the live inventory /
    // products services now — the sim holds no on-hand mirror. Raw
    // parts are seeded into the inventory-api by the binary's
    // `seed_parts`; the workforce executor reads real on-hand back
    // when it gates production-consume / demand-gate steps.
    state.seed_subject("location", "loc-brewery-brewhouse");
    state.seed_subject("location", "loc-brewery-taproom");
    state.seed_subject("location", "loc-hq");
    for i in 0..50 {
        state.seed_subject("account", &format!("acc-bigseed-{i:04}"));
    }
    // Keep in sync with boss_brewery_data_seed::VENDOR_COUNT — the
    // auto-restock vendor resolver can pick any seeded vendor (incl.
    // the packaging supplier at index 12), and each must be a Subject
    // so the restock Job's subject resolves.
    for i in 0..13 {
        state.seed_subject("vendor", &format!("vnd-bigseed-{i:03}"));
    }
    // Campaign Subjects for the tap-launch + seasonal-release
    // marketing flows. The tap-launch JobKind (rate 0.04 ≈
    // 3/quarter in tenant.toml) attaches to a campaign Subject,
    // so the marketing surface needs anchors to fire against.
    // Six evergreen campaigns cover a typical brewery's seasonal
    // calendar, giving the marketing surface continuous activity.
    for slug in [
        "cmp-spring-saison",
        "cmp-summer-fest",
        "cmp-oktoberfest",
        "cmp-winter-stout",
        "cmp-anniversary",
        "cmp-collab-rotational",
    ] {
        state.seed_subject("campaign", slug);
    }

    // Populate the role-keyed Employee pool so `advance_steps` can
    // pick a real Employee actor for each step transition
    // (sim-fidelity). Reads
    // examples/brewery/seeds/employees.json — the same roster the
    // people projection holds — and groups by role. Without this,
    // every audit_log row would read `automation:brewery-sim` and
    // the demo would lose the "human-powered state machine" framing.
    let employees_path = std::path::Path::new("/opt/boss/examples/brewery/seeds/employees.json");
    if let Ok(bytes) = std::fs::read(employees_path)
        && let Ok(roster) = serde_json::from_slice::<Vec<serde_json::Value>>(&bytes)
    {
        for emp in roster {
            let id = emp.get("id").and_then(|v| v.as_str());
            let role = emp.get("role").and_then(|v| v.as_str());
            let status = emp
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("active");
            if status != "active" {
                continue;
            }
            if let (Some(id), Some(role)) = (id, role) {
                state.register_employee(role, id);
                // Subjects-by-kind too so `subject_kind=employee`
                // JobKind rates can pick assignees from the same
                // pool.
                state.seed_subject("employee", id);
            }
        }
    }
}

/// Mutable engine state a long-running daemon (boss-brewery-sim)
/// holds across ticks. The in-flight pending state of every
/// CounterpartyEngine chain (bank-ach 30bd delay, ar-aging
/// 30bd delay, malt-supplier 5bd, keg-courier 3-stage scans)
/// must survive tick boundaries; at 1-day chunks every chained
/// delay >1 day would otherwise be lost on each chunk.
///
/// Construct via [`BreweryEngineState::load`] once, then call
/// [`run_brewery_one_day`] in a loop to advance day-by-day
/// without losing pending-action state.
pub struct BreweryEngineState {
    pub kinds: Vec<boss_jobs::registry::JobKindSpec>,
    pub registry: StepRegistry,
    pub tenant: TenantConfig,
    pub state: ShapeDrivenState,
    pub rng: Rng,
    pub periodic: PeriodicEngine,
    pub counterparty: CounterpartyEngine,
    /// Cross-tick bus + report. When the daemon ticks per-tick
    /// (rather than per-sim-day), the bus must live across the
    /// ticks of a sim-day so the Counterparty engine's last-tick
    /// drain sees the full day's events. Reset in
    /// [`run_brewery_one_day`] to preserve its day-boundary
    /// semantics; held across ticks by [`run_brewery_one_tick`]
    /// + [`brewery_end_of_day`].
    pub bus: SimEventBus,
    pub report: RunReport,
}

impl BreweryEngineState {
    /// Build a fresh engine state from the seed bundle. Same
    /// initialization `run_brewery_into` does internally —
    /// extracted so daemons can hold one across many calls.
    ///
    /// Every step-completion side effect routes through the
    /// boss-dispatcher rule registry, which subscribes to
    /// `step.done.<kind>` on NATS and fires the registered HTTP
    /// handlers. The engine just drives Job/Step lifecycle here.
    pub fn load(seeds: &Path) -> Result<Self> {
        let tenant_path = seeds.join("tenant.toml");
        let kinds_path = seeds.join("job_kinds.toml");
        let tenant = TenantConfig::load(&tenant_path)
            .with_context(|| format!("loading tenant config from {}", tenant_path.display()))?;
        let kinds = load_job_kinds_with_owning_team(&kinds_path, &tenant.meta.tenant_id)
            .with_context(|| format!("loading job kinds from {}", kinds_path.display()))?;
        let registry = StepRegistry::v1();

        let mut state = ShapeDrivenState::new();
        seed_brewery_subjects(&mut state);

        let periodic =
            PeriodicEngine::new(tenant.periodic_specs(), CalendarRegistry::with_builtins());
        let counterparty = CounterpartyEngine::new(
            tenant.counterparty_specs(),
            CalendarRegistry::with_builtins(),
        );
        let rng = Rng::new(tenant.meta.seed);

        Ok(Self {
            kinds,
            registry,
            tenant,
            state,
            rng,
            periodic,
            counterparty,
            bus: SimEventBus::new(),
            report: RunReport::default(),
        })
    }
}

/// Advance the engine by exactly one tick. The `boss-brewery-sim`
/// daemon calls this in a per-tick loop with wall-clock sleeps
/// between ticks so events spread across the wall-clock window
/// instead of bursting once per sim-day.
///
/// The `engine.bus` carries cross-tick state across an entire
/// sim-day (Counterparty's last-tick drain reads it). After the
/// last tick of a sim-day (`tick_idx + 1 == ticks_per_day`),
/// callers MUST invoke [`brewery_end_of_day`] to flush per-day
/// counters + the SimOutput's per-day buffer + clear the bus.
///
/// Operating-day skip: this returns `Ok(())` without doing work
/// when the day isn't an operating day per `tenant.meta`. Daemons
/// can advance the calendar regardless; the engine just no-ops
/// for non-operating days.
pub fn run_brewery_one_tick(
    engine: &mut BreweryEngineState,
    day: NaiveDate,
    tick_idx: u32,
    output: &mut dyn SimOutput,
) -> Result<()> {
    if !engine.tenant.meta.is_operating_day(day) {
        return Ok(());
    }
    let ticks_per_day = engine.tenant.meta.ticks_per_day();
    run_one_tick_with_handlers(
        day,
        tick_idx,
        ticks_per_day,
        &engine.kinds,
        &engine.registry,
        &engine.tenant,
        &mut engine.state,
        &mut engine.rng,
        output,
        &mut engine.periodic,
        &mut engine.counterparty,
        &mut engine.bus,
        &mut engine.report,
    )
}

/// End-of-sim-day flush — companion to [`run_brewery_one_tick`].
/// Daemons MUST call this after the last tick of each sim-day to
/// drain the per-day bus + push the SimOutput's per-day buffer.
pub fn brewery_end_of_day(
    engine: &mut BreweryEngineState,
    day: NaiveDate,
    output: &mut dyn SimOutput,
) -> Result<()> {
    end_of_day_rollup(day, &mut engine.bus, output, &mut engine.report)
}

/// Run the brewery engine for `days` days starting at `start`
/// (defaults to the tenant's configured start date) into the
/// given `SimOutput`. The caller owns the output so it can be
/// `InMemoryOutput` (tests + assertion checks) or
/// `LiveApiOutput` (live-stack runs that drive audit_log via
/// the API services).
///
/// Every step-completion side effect routes through the
/// boss-dispatcher rule registry via NATS. The engine just
/// drives Job/Step lifecycle.
pub fn run_brewery_into(
    seeds: &Path,
    days: u32,
    start: Option<NaiveDate>,
    output: &mut dyn SimOutput,
) -> Result<RunReport> {
    let mut engine = BreweryEngineState::load(seeds)?;
    let start = start.unwrap_or(engine.tenant.meta.start_date);
    let end = start + chrono::Duration::days(days as i64 - 1);
    let ticks_per_day = engine.tenant.meta.ticks_per_day();

    run_ticks_with_handlers(
        start,
        end,
        ticks_per_day,
        &engine.kinds,
        &engine.registry,
        &engine.tenant,
        &mut engine.state,
        &mut engine.rng,
        output,
        &mut engine.periodic,
        &mut engine.counterparty,
    )
}

/// Live, clock-coordinated brewery run — the workforce model.
///
/// Unlike [`run_brewery_into`] (the in-memory test path, which sprints
/// days), this drives a live API stack the way a real deployment runs:
/// the formula clock is configured **once** and then free-runs, and the
/// sim coordinates *against* it. As the clock reaches each sim-day the
/// engine generates that day's Jobs (POSTed synchronously; the server
/// materializes their steps + emits `step.ready`), and the workforce
/// executor works open steps — claiming Ready work and completing Active
/// steps once their duration has elapsed against the clock. A trailing
/// drain lets the tail of in-flight steps + dispatcher-spawned restocks
/// settle. The 12-month seed regen uses this.
///
/// `warp_factor` is the single pacing knob (sim-seconds per wall-second;
/// wall-time ≈ sim-span ÷ warp); `poll_sleep_ms` is the wall pause
/// between workforce check-ins.
/// Start the live-api external-party callback receiver.
///
/// The dispatcher's `webhook.notify` handler POSTs `{topic, payload}` here
/// for each event an external counterparty cares about (invoice created,
/// shipment created, …). We buffer them; `run_brewery_live` drains the
/// buffer onto the bus each day so the CounterpartyEngine reacts and emits
/// its deferred response back through the public API. The simulator never
/// subscribes to the system's event stream — it only receives these
/// callbacks, preserving the sim/system boundary.
///
/// A sync `TcpListener` in its own thread: the run loop is
/// `reqwest::blocking` and must not nest a tokio runtime, and this adds no
/// dependency. Returns an empty (never-filled) buffer when
/// `BOSS_SIM_CALLBACK_BIND` is unset.
///
/// `pub` so the long-running `boss-brewery-sim` daemon (a separate bin that
/// links this crate) can start the same receiver and drain its queue onto
/// `engine.bus` each sim-day, exactly the way [`run_brewery_live`] does. The
/// engine bin reaches it transitively through `run_brewery_live`; the daemon
/// drives its own per-tick loop, so it needs the constructor directly.
pub fn start_callback_receiver() -> Arc<Mutex<VecDeque<SimBusEvent>>> {
    use std::io::{BufRead, BufReader, Read, Write};

    let buffer: Arc<Mutex<VecDeque<SimBusEvent>>> = Arc::new(Mutex::new(VecDeque::new()));
    let Ok(bind) = std::env::var("BOSS_SIM_CALLBACK_BIND") else {
        return buffer;
    };
    let listener = match std::net::TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, bind = %bind,
                "callback receiver bind failed; counterparties will stay dark");
            return buffer;
        }
    };
    tracing::info!(bind = %bind, "external-party callback receiver listening");

    let buf = buffer.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Ok(read_half) = stream.try_clone() else {
                continue;
            };
            let mut reader = BufReader::new(read_half);

            // Parse just enough HTTP: header lines until a blank line,
            // capturing Content-Length, then exactly that many body bytes.
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let t = line.trim_end();
                        if t.is_empty() {
                            break;
                        }
                        if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }
                    Err(_) => break,
                }
            }
            let mut body = vec![0u8; content_length];
            if reader.read_exact(&mut body).is_ok()
                && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body)
                && let Some(topic) = v.get("topic").and_then(|t| t.as_str())
            {
                let payload = v
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                if let Ok(mut q) = buf.lock() {
                    q.push_back(SimBusEvent::new(topic, "webhook", payload));
                }
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    buffer
}

/// Build the sim [`Workforce`](boss_sim::workforce::Workforce) executor
/// from an engine's StepRegistry: kind→typical-duration and
/// kind→required-at-done fields. Shared by the offline regen
/// ([`run_brewery_live`]) and the live `boss-brewery-sim` daemon so both
/// drive steps with identical pacing metadata. The caller owns clock
/// setup — the regen calls `configure_clock`; the daemon leaves the clock
/// to clock-api.
pub fn build_workforce(
    engine: &BreweryEngineState,
    api_base: &str,
) -> boss_sim::workforce::Workforce {
    use boss_sim::workforce::{RequiredField, Workforce};
    // kind → typical duration hours (drives duration-gated completion).
    let step_durations: std::collections::HashMap<String, f64> = engine
        .registry
        .all()
        .into_iter()
        .filter_map(|t| t.typical_duration_hours.map(|h| (t.kind.to_string(), h)))
        .collect();
    // kind → required-at-done fields, so the worker supplies any the
    // JobKind didn't default — the executor filling the step's form.
    let required_fields: std::collections::HashMap<String, Vec<RequiredField>> = engine
        .registry
        .all()
        .into_iter()
        .map(|t| {
            let reqs = t
                .fields
                .iter()
                .filter(|f| f.required)
                .map(|f| RequiredField {
                    name: f.name.to_string(),
                    field_type: f.field_type.to_string(),
                })
                .collect();
            (t.kind.to_string(), reqs)
        })
        .collect();
    Workforce::new(api_base, step_durations, required_fields)
}

#[allow(clippy::too_many_arguments)]
pub fn run_brewery_live(
    seeds: &Path,
    days: u32,
    start: Option<NaiveDate>,
    api_base: &str,
    warp_factor: f64,
    poll_sleep_ms: u64,
    output: &mut dyn SimOutput,
) -> Result<RunReport> {
    let mut engine = BreweryEngineState::load(seeds)?;
    let start = start.unwrap_or(engine.tenant.meta.start_date);
    let end = start + chrono::Duration::days(days as i64 - 1);
    let ticks_per_day = engine.tenant.meta.ticks_per_day();

    let mut workforce = build_workforce(&engine, api_base);

    // Set the clock ONCE; it free-runs from here and is never touched again.
    workforce.configure_clock(start, end, warp_factor)?;

    // External-party callbacks (the dispatcher's webhook.notify) land in this
    // buffer; we drain it onto the bus each day so the CounterpartyEngine
    // reacts to live events. Stays empty when no webhook is wired
    // (BOSS_SIM_CALLBACK_BIND unset), so non-regen runs are unaffected.
    let callbacks = start_callback_receiver();

    // Wall-clock anchor so the end-of-run summary can report the effective
    // workforce pass rate (checkins / elapsed) — the key throughput signal.
    let run_started = std::time::Instant::now();
    let poll = std::time::Duration::from_millis(poll_sleep_ms);
    let mut day = start;
    while day <= end {
        // Pace generation to the clock: wait until it reaches `day`,
        // driving in-flight work while we wait.
        loop {
            let (now, _) = workforce.clock_now()?;
            if now.date_naive() >= day {
                break;
            }
            workforce.work_once()?;
            std::thread::sleep(poll);
        }
        if engine.tenant.meta.is_operating_day(day) {
            output.start_of_day(day)?;
            // Drain external-party callbacks onto the bus before the
            // CounterpartyEngine runs this tick, so it reacts to the live
            // events the dispatcher forwarded since the last day.
            if let Ok(mut q) = callbacks.lock() {
                for ev in q.drain(..) {
                    engine.bus.emit(ev);
                }
            }
            for tick_idx in 0..ticks_per_day {
                run_one_tick_with_handlers(
                    day,
                    tick_idx,
                    ticks_per_day,
                    &engine.kinds,
                    &engine.registry,
                    &engine.tenant,
                    &mut engine.state,
                    &mut engine.rng,
                    output,
                    &mut engine.periodic,
                    &mut engine.counterparty,
                    &mut engine.bus,
                    &mut engine.report,
                )?;
            }
            end_of_day_rollup(day, &mut engine.bus, output, &mut engine.report)?;
        }
        workforce.work_once()?;
        day = day.succ_opt().expect("date sequence overflow");
    }

    let dayloop_checkins = workforce.stats.checkins;
    // Drain: keep driving until the workforce goes idle for several
    // consecutive rounds (the tail of in-flight steps + dispatcher-spawned
    // restocks settle), so the run doesn't end mid-pipeline. Capped so a
    // perpetually-churning pipeline can't hang the run.
    let mut idle = 0;
    let mut rounds = 0;
    while idle < 5 && rounds < 5_000 {
        let before = (workforce.stats.claimed, workforce.stats.completed);
        workforce.work_once()?;
        if (workforce.stats.claimed, workforce.stats.completed) == before {
            idle += 1;
        } else {
            idle = 0;
        }
        std::thread::sleep(poll);
        rounds += 1;
    }

    let s = &workforce.stats;
    let elapsed = run_started.elapsed().as_secs_f64();
    tracing::info!(
        checkins = s.checkins,
        dayloop_checkins,
        drain_rounds = rounds,
        claimed = s.claimed,
        completed = s.completed,
        deferred = s.deferred,
        in_progress = s.in_progress,
        errors = s.errors,
        elapsed_secs = elapsed,
        passes_per_sec = if elapsed > 0.0 {
            s.checkins as f64 / elapsed
        } else {
            0.0
        },
        "workforce run complete",
    );

    output.flush()?;
    engine.report.jobs_created = engine.state.counters.jobs_created;
    engine.report.counterparty_pending = engine.counterparty.pending() as u64;
    Ok(engine.report)
}

/// Run the brewery engine for `days` days into a fresh
/// `InMemoryOutput`. Tests + the CLI's default mode use this.
pub fn run_brewery(seeds: &Path, days: u32, start: Option<NaiveDate>) -> Result<BreweryRunResult> {
    let mut output = InMemoryOutput::default();
    let report = run_brewery_into(seeds, days, start, &mut output)?;
    Ok(BreweryRunResult { report, output })
}
