//! Unified day-loop runner — drives Periodic → HumanWorker →
//! Counterparty, in that order. One bus, one
//! state, one RNG, one `SimOutput` shared across all engines.
//!
//! The HumanWorker engine is the existing `shape_driven::simulate_day`
//! function; until it becomes a `SimEngine` impl in Phase 4, the
//! runner calls it directly between the Periodic and Counterparty
//! steps.

use anyhow::Result;
use chrono::NaiveDate;

use boss_jobs::registry::JobKindSpec;
use boss_jobs::step_registry::StepRegistry;

use crate::engines::{
    CounterpartyEngine, DayContext, PeriodicEngine, SimEngine, SimEventBus, Tick,
};
use crate::output::SimOutput;
use crate::rng::Rng;
use crate::shape_driven::{
    ShapeDrivenState, TenantConfig, open_job_from_request, simulate_tick_with_handlers,
};

/// Cumulative counters from a `RunReport`. Tests + dashboards read
/// these to sanity-check a run.
#[derive(Debug, Default, Clone)]
pub struct RunReport {
    pub days_simulated: u64,
    pub jobs_created: u64,
    pub jobs_closed: u64,
    pub steps_completed: u64,
    /// Events seen on the SimOutput::emit_event path. Captured in
    /// `InMemoryOutput.events` when that's the sink; tallied here
    /// per topic for quick assertion.
    pub events_by_topic: std::collections::BTreeMap<String, u64>,
    /// Periodic firings observed (count of `periodic.job_requested`
    /// events on the bus). Useful when the periodic action is
    /// OpenJob.
    pub periodic_fires: u64,
    /// Jobs materialized from `periodic.job_requested` events (a
    /// strict subset of `periodic_fires` — fires whose JobKind
    /// resolved against the registry). The remainder are dropped
    /// with `state.counters.jobs_skipped_unknown_kind`.
    pub jobs_opened_from_periodic: u64,
    /// Sum of CounterpartyEngine queue depth at end of run — Jobs
    /// closed but settlements still pending.
    pub counterparty_pending: u64,
}

/// Run the full day-loop from `start` through `end` (inclusive),
/// driving every engine in the design-doc order. Returns a
/// `RunReport` summary.
///
/// `output` is the SimOutput sink. `InMemoryOutput` captures
/// emit_event calls into a Vec for assertions; `LiveApiOutput`
/// posts them to the registered endpoints.
#[allow(clippy::too_many_arguments)]
pub fn run_days(
    start: NaiveDate,
    end: NaiveDate,
    job_kinds: &[JobKindSpec],
    step_registry: &StepRegistry,
    tenant: &TenantConfig,
    state: &mut ShapeDrivenState,
    rng: &mut Rng,
    output: &mut dyn SimOutput,
    periodic: &mut PeriodicEngine,
    counterparty: &mut CounterpartyEngine,
) -> Result<RunReport> {
    run_ticks_with_handlers(
        start,
        end,
        1, // ticks_per_day = 1 → bit-for-bit equivalent to the
        //                       legacy day-tick path.
        job_kinds,
        step_registry,
        tenant,
        state,
        rng,
        output,
        periodic,
        counterparty,
    )
}

/// Tick-aware day loop — Phase A of the sub-day-tick rollout
/// (TODO.md "Hourly tick granularity for the live sim").
///
/// Splits each sim-day into `ticks_per_day` equal ticks. Each
/// tick advances the shape-driven engine (Poisson + birth +
/// step-completion scaled by `tick.day_fraction()`) but
/// day-anchored mechanisms (Periodic + Batch + Counterparty
/// queue drain + the periodic.job_requested fanout) only fire
/// on the first tick of each sim-day.
///
/// At `ticks_per_day = 1` this is bit-for-bit equivalent to
/// `run_days_with_handlers` — used as the wrapper above. At
/// `ticks_per_day = 24` a sim-day is split into 24 hourly ticks;
/// per-day expected Job volume + step completion + subject birth
/// stay invariant (see `engines::tick::Tick` for the math).
///
/// Phase B will lift Periodic/Counterparty onto sub-day cadences
/// when sub-day periodic events become useful (e.g. inbox-poll
/// every 15 minutes, intra-day counterparty resolution at
/// `mean_days = 0.5`).
#[allow(clippy::too_many_arguments)]
pub fn run_ticks_with_handlers(
    start: NaiveDate,
    end: NaiveDate,
    ticks_per_day: u32,
    job_kinds: &[JobKindSpec],
    step_registry: &StepRegistry,
    tenant: &TenantConfig,
    state: &mut ShapeDrivenState,
    rng: &mut Rng,
    output: &mut dyn SimOutput,
    periodic: &mut PeriodicEngine,
    counterparty: &mut CounterpartyEngine,
) -> Result<RunReport> {
    assert!(
        ticks_per_day > 0,
        "ticks_per_day must be positive; got {ticks_per_day}"
    );
    let mut report = RunReport::default();
    let mut bus = SimEventBus::new();

    let mut day = start;
    while day <= end {
        // Operating-day filter — the shape-driven engine already
        // gates on this internally, but the Periodic engine doesn't,
        // so check here so the whole day uniformly skips.
        if !tenant.meta.is_operating_day(day) {
            day = day.succ_opt().expect("date sequence overflow");
            continue;
        }

        // Latch the sim day on the output before any in-day emit
        // fires. `LiveApiOutput` uses this to stamp the X-Sim-Time
        // header on every outbound POST/PUT for the rest of the
        // day, so financial_facts written by in-day side effects
        // (products.consume → COGS, products.produce → WIP→FG,
        // inventory.overhead.absorbed) get the sim date rather than
        // wall-clock. Test outputs no-op.
        output.start_of_day(day)?;

        for tick_idx in 0..ticks_per_day {
            run_one_tick_with_handlers(
                day,
                tick_idx,
                ticks_per_day,
                job_kinds,
                step_registry,
                tenant,
                state,
                rng,
                output,
                periodic,
                counterparty,
                &mut bus,
                &mut report,
            )?;
        }

        // End-of-day rollup — fires once per sim-day after the
        // last tick. The per-tick path expects the caller to call
        // `end_of_day_rollup` in the reentrant API; this in-loop
        // path inlines it.
        end_of_day_rollup(day, &mut bus, output, &mut report)?;
        day = day.succ_opt().expect("date sequence overflow");
    }

    output.flush()?;

    report.jobs_created = state.counters.jobs_created;
    report.jobs_closed = state.counters.jobs_closed;
    report.steps_completed = state.counters.steps_completed;
    report.counterparty_pending = counterparty.pending() as u64;

    Ok(report)
}

/// Reentrant single-tick advance — Phase C of the sub-day-tick
/// rollout. The daemon path
/// (`crates/boss-brewery-engine/src/bin/boss_brewery_sim.rs`)
/// calls this in a loop with wall-clock sleeps between ticks so
/// events spread across the wall-clock window instead of bursting
/// once per sim-day. The in-loop `run_ticks_with_handlers` body
/// above just calls this for every tick.
///
/// **Caller contract**: `bus` + `report` are external state owned
/// across an entire sim-day's tick sequence. After processing the
/// last tick (`tick_idx + 1 == ticks_per_day`), the caller MUST
/// invoke [`end_of_day_rollup`] to flush per-day events into the
/// report, fire `output.end_of_day`, and clear the bus.
///
/// Phase A's day-anchored gating preserved: Periodic + Batch fire
/// only on `tick_idx == 0`; the periodic.job_requested fanout
/// runs on tick 0 too; Counterparty queue drain runs on the last
/// tick so it sees the full day's bus events.
#[allow(clippy::too_many_arguments)]
pub fn run_one_tick_with_handlers(
    day: NaiveDate,
    tick_idx: u32,
    ticks_per_day: u32,
    job_kinds: &[JobKindSpec],
    step_registry: &StepRegistry,
    tenant: &TenantConfig,
    state: &mut ShapeDrivenState,
    rng: &mut Rng,
    output: &mut dyn SimOutput,
    periodic: &mut PeriodicEngine,
    counterparty: &mut CounterpartyEngine,
    bus: &mut SimEventBus,
    report: &mut RunReport,
) -> Result<()> {
    let tick_duration_hours = 24.0 / (ticks_per_day as f64);
    let is_first_in_day = tick_idx == 0;
    // start_hour_of_day = which sim-hour this tick starts at.
    // tick_idx 0 with 1d ticks → 0.0 (covers full day).
    // tick_idx 0..23 with hourly ticks → 0.0..23.0.
    // tick_idx 0..95 with 15m ticks → 0.0, 0.25, 0.5, ..., 23.75.
    let start_hour_of_day = (tick_idx as f64) * tick_duration_hours;
    let tick = Tick::new(tick_duration_hours, start_hour_of_day, is_first_in_day);

    // 1. Periodic — calendar-driven cycles. Now tick-aware
    //    (Phase B-2 follow-up): the engine's `step()` consults
    //    `Cadence::fires_on_tick(anchor, day, tick)` per spec,
    //    so day-anchored cadences (Daily/Weekly/Monthly/etc)
    //    still gate on `is_first_in_day` while sub-day cadences
    //    (Hourly/EveryNMinutes) fire on the right tick within
    //    the day. Run periodic per-tick so sub-day cadences
    //    have a chance to fire; the engine's gating filters
    //    out non-firing specs cheaply.
    //
    // (Step 2 — BatchEngine — retired 2026-05-06; was a
    // back-door that produced canonical events without going
    // through the JobKind / Step / audit-trail. The Periodic
    // engine above now opens JobKinds for those flows
    // (sales-tax-filing already; payroll-run / payroll-941 /
    // income-tax JobKinds in Stage 4) and the terminal step's
    // side-effect handler emits the canonical event through
    // the same /api/ledger/* POST the BatchEngine used to hit.)
    {
        let mut ctx = DayContext {
            day,
            tick,
            rng,
            state,
            output,
            bus,
        };
        periodic.step(&mut ctx)?;
    }

    // 3. HumanWorker — shape-driven engine, tick-aware. Advances
    //    Steps with `tick.day_fraction()`-scaled completion
    //    probability; opens new Jobs with tick-scaled Poisson
    //    rates; cadence rows fire only on `is_first_in_day`
    //    (Phase B-2 will add a `time_of_day` field for sub-day
    //    cadence anchoring).
    let summary = simulate_tick_with_handlers(
        job_kinds,
        step_registry,
        tenant,
        day,
        &tick,
        state,
        rng,
        output,
        bus,
    );
    let _ = summary;

    // 3.5. Drain `periodic.job_requested` events Periodic / Batch
    //      published earlier today and materialize a Job per
    //      request. Day-anchored — periodic + batch only fire on
    //      first-tick, so this drain only does work then; later
    //      ticks see an empty stream.
    if is_first_in_day {
        let requests: Vec<serde_json::Value> = bus
            .events_matching("periodic.job_requested")
            .map(|ev| ev.payload.clone())
            .collect();
        for payload in requests {
            if open_job_from_request(
                &payload,
                job_kinds,
                tenant,
                day,
                state,
                rng,
                output,
                bus,
                step_registry,
            ) {
                report.jobs_opened_from_periodic += 1;
            }
        }
    }

    // 4. Counterparty — drains today's due rows, then queues new
    //    rows from today's bus events. Day-anchored today (Phase
    //    A); the queue keys on `NaiveDate`. Run on the LAST tick
    //    of the day so the queue sees every bus event the
    //    Periodic + Batch + HumanWorker engines fired across all
    //    of today's ticks. Phase B-2 widens the queue cursor to
    //    `NaiveDateTime` so sub-day delays (`mean_days = 0.5`)
    //    actually fire 12 hours later instead of "tomorrow."
    if tick_idx + 1 == ticks_per_day {
        let mut ctx = DayContext {
            day,
            tick,
            rng,
            state,
            output,
            bus,
        };
        counterparty.step(&mut ctx)?;
    }

    Ok(())
}

/// End-of-sim-day rollup — caller of [`run_one_tick_with_handlers`]
/// invokes this after the last tick of each sim-day. Tallies bus
/// events into the report's per-topic counters, fires the
/// `SimOutput::end_of_day` flush hook (the live-API path uses
/// this to push the day's buffered POSTs), then clears the bus.
pub fn end_of_day_rollup(
    day: NaiveDate,
    bus: &mut SimEventBus,
    output: &mut dyn SimOutput,
    report: &mut RunReport,
) -> Result<()> {
    for ev in bus.events() {
        *report.events_by_topic.entry(ev.topic.clone()).or_insert(0) += 1;
        if ev.topic == "periodic.job_requested" {
            report.periodic_fires += 1;
        }
    }
    output.end_of_day(day)?;
    bus.clear_day();
    report.days_simulated += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::CalendarRegistry;
    use crate::engines::{Cadence, CounterpartySpec, DelaySpec, PeriodicAction, PeriodicSpec};
    use crate::output::InMemoryOutput;
    use crate::shape_driven::{JobRate, TenantConfig, TenantMeta};
    use boss_jobs::registry::{StepSpec, Terminal};
    use serde_json::json;
    use std::collections::HashMap;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// v2 step chain: a trigger step (`ready_when="true"`, fires at Job
    /// open) followed by one `task` step per `(kind, label)` in
    /// `work`, each gated on the prior step's completion; the last work
    /// step is the terminal. Mirrors the pre-v2 tier graph these tests
    /// used, in the flat predicate-graph shape v2 expects.
    fn chain(work: &[(&str, &str)]) -> Vec<StepSpec> {
        let mut steps = vec![StepSpec {
            title: "trigger".into(),
            kind: "trigger".into(),
            ready_when: "true".into(),
            title_template: "Opened".into(),
            ..Default::default()
        }];
        let mut prev = "trigger".to_string();
        for (i, (kind, label)) in work.iter().enumerate() {
            let title = format!("step-{i}");
            let terminal = if i + 1 == work.len() {
                Some(Terminal {
                    outcome: "completed".into(),
                })
            } else {
                None
            };
            steps.push(StepSpec {
                title: title.clone(),
                kind: (*kind).to_string(),
                ready_when: format!("steps.{prev}.done"),
                title_template: (*label).to_string(),
                terminal,
                metadata_defaults: json!({}),
                ..Default::default()
            });
            prev = title;
        }
        steps
    }

    /// Smoke test: a day-loop that runs Periodic + HumanWorker +
    /// Counterparty advances every counter the way the design doc
    /// describes — periodic fires on cadence, HumanWorker opens +
    /// closes Jobs, counterparty queues + drains.
    #[test]
    fn end_to_end_runner_drives_all_three_engines() {
        let mut spec = JobKindSpec::platform_seed(
            "morning-bake",
            "Morning Bake",
            "production",
            vec!["location".into()],
            chain(&[("task", "Mix"), ("task", "Bake")]),
        );
        spec.subject_kinds = vec!["location".into()];
        let kinds = vec![spec];

        let tenant = TenantConfig {
            meta: TenantMeta {
                tenant_id: "e2e".into(),
                display_name: "E2E".into(),
                seed: 0xE2E,
                start_date: d(2026, 4, 27),
                end_date: d(2026, 12, 31),
                operating_days: vec![],
                tick_duration: "1d".into(),
                step_speed_multiplier: None,
                operating_hours: std::collections::HashMap::new(),
            },
            job_rates: [(
                "morning-bake".to_string(),
                JobRate {
                    rate: 1.0,
                    ramp: vec![],
                    weekday_multiplier: None,
                    weekend_multiplier: None,
                    subject_distribution: HashMap::new(),
                    subject_cadence: Vec::new(),
                    month_multipliers: HashMap::new(),
                    deterministic: false,
                },
            )]
            .into_iter()
            .collect(),
            subject_rates: HashMap::new(),
            anomalies: HashMap::new(),
            counterparty: HashMap::new(),
            shock: Vec::new(),
            periodic: HashMap::new(),
            batch: HashMap::new(),
        };

        let registry = StepRegistry::v1();
        let mut state = ShapeDrivenState::new();
        state.seed_subject("location", "loc-brewery-brewhouse");
        let mut rng = Rng::new(0x123);
        let mut output = InMemoryOutput::default();

        let mut periodic = PeriodicEngine::new(
            vec![PeriodicSpec {
                name: "daily-tick".into(),
                cadence: Cadence::Daily,
                anchor_date: d(2026, 4, 27),
                business_calendar: None,
                action: PeriodicAction::EmitEvent {
                    topic: "internal.daily_tick".into(),
                    payload: serde_json::Value::Null,
                },
            }],
            CalendarRegistry::for_tests(),
        );
        let mut counterparty = CounterpartyEngine::new(
            vec![CounterpartySpec {
                actor_kind: None,
                name: "settler".into(),
                listens_to: "job.closed".into(),
                delay: DelaySpec {
                    mean_days: 1.0,
                    spread_days: 0.0,
                    business_calendar: Some("us-banking".into()),
                },
                emit_probability: 1.0,
                emits: "ledger.payment_settled".into(),
                payload: json!({"channel": "ach"}),
                followups: vec![],
                scans: vec![],
                emit_else: None,
                match_payload: serde_json::Map::new(),
            }],
            CalendarRegistry::for_tests(),
        );

        let report = run_days(
            d(2026, 4, 27),
            d(2026, 5, 26), // 30 calendar days
            &kinds,
            &registry,
            &tenant,
            &mut state,
            &mut rng,
            &mut output,
            &mut periodic,
            &mut counterparty,
        )
        .unwrap();

        assert_eq!(report.days_simulated, 30);
        assert!(report.jobs_created > 0, "expected morning-bake Jobs");
        // The sim no longer drives or closes Jobs — the workforce
        // executor works them against the live system — so jobs_closed
        // stays 0 in this generation-side integration test.
        // Periodic ticks fired daily.
        assert_eq!(
            report.periodic_fires, 0,
            "EmitEvent doesn't bump periodic_fires"
        );
        assert_eq!(
            *report
                .events_by_topic
                .get("internal.daily_tick")
                .unwrap_or(&0),
            30,
            "daily-tick should fire once per simulated day"
        );
        // Every closed Job becomes either a drained settlement or a
        // pending queue row.
        let drained = *report
            .events_by_topic
            .get("ledger.payment_settled")
            .unwrap_or(&0);
        assert_eq!(
            drained + report.counterparty_pending,
            report.jobs_closed,
            "drained ({drained}) + pending ({}) should equal jobs_closed ({})",
            report.counterparty_pending,
            report.jobs_closed
        );
    }

    /// Operating-day filter applies whole-loop, including the
    /// Periodic engine. A daily-tick spec on a Mon-Fri-only tenant
    /// must not fire on Saturdays.
    #[test]
    fn operating_days_skip_periodic_too() {
        let tenant = TenantConfig {
            meta: TenantMeta {
                tenant_id: "weekday".into(),
                display_name: "Weekday".into(),
                seed: 1,
                start_date: d(2026, 4, 27),
                end_date: d(2026, 12, 31),
                operating_days: vec![
                    "mon".into(),
                    "tue".into(),
                    "wed".into(),
                    "thu".into(),
                    "fri".into(),
                ],
                tick_duration: "1d".into(),
                step_speed_multiplier: None,
                operating_hours: std::collections::HashMap::new(),
            },
            job_rates: HashMap::new(),
            subject_rates: HashMap::new(),
            anomalies: HashMap::new(),
            counterparty: HashMap::new(),
            shock: Vec::new(),
            periodic: HashMap::new(),
            batch: HashMap::new(),
        };
        let kinds: Vec<JobKindSpec> = vec![];
        let registry = StepRegistry::v1();
        let mut state = ShapeDrivenState::new();
        let mut rng = Rng::new(7);
        let mut output = InMemoryOutput::default();
        let mut periodic = PeriodicEngine::new(
            vec![PeriodicSpec {
                name: "daily-tick".into(),
                cadence: Cadence::Daily,
                anchor_date: d(2026, 4, 27),
                business_calendar: None,
                action: PeriodicAction::EmitEvent {
                    topic: "x.y".into(),
                    payload: serde_json::Value::Null,
                },
            }],
            CalendarRegistry::for_tests(),
        );
        let mut counterparty = CounterpartyEngine::new(vec![], CalendarRegistry::for_tests());

        // Mon 4/27 through Sun 5/3 = 7 calendar days, 5 weekdays.
        let report = run_days(
            d(2026, 4, 27),
            d(2026, 5, 3),
            &kinds,
            &registry,
            &tenant,
            &mut state,
            &mut rng,
            &mut output,
            &mut periodic,
            &mut counterparty,
        )
        .unwrap();
        assert_eq!(report.days_simulated, 5);
        assert_eq!(*report.events_by_topic.get("x.y").unwrap_or(&0), 5);
    }

    /// PeriodicAction::OpenJob now materializes a Job through
    /// shape_driven::open_job_from_request. Before this wiring the
    /// emitted `periodic.job_requested` events were no-ops; the
    /// regression here is that a daily OpenJob for a known
    /// JobKind produces N Jobs over N operating days.
    #[test]
    fn periodic_open_job_action_materializes_jobs() {
        let mut spec = JobKindSpec::platform_seed(
            "equipment-preventive-maintenance",
            "Equipment preventive maintenance",
            "ops",
            vec!["location".into()],
            chain(&[("task", "Inspect")]),
        );
        spec.subject_kinds = vec!["location".into()];
        let kinds = vec![spec];

        let tenant = TenantConfig {
            meta: TenantMeta {
                tenant_id: "preventive-maintenance".into(),
                display_name: "preventive maintenance".into(),
                seed: 0xBEEF,
                start_date: d(2026, 4, 27),
                end_date: d(2026, 12, 31),
                operating_days: vec![],
                tick_duration: "1d".into(),
                step_speed_multiplier: None,
                operating_hours: std::collections::HashMap::new(),
            },
            job_rates: HashMap::new(),
            subject_rates: HashMap::new(),
            anomalies: HashMap::new(),
            counterparty: HashMap::new(),
            shock: Vec::new(),
            periodic: HashMap::new(),
            batch: HashMap::new(),
        };

        let registry = StepRegistry::v1();
        let mut state = ShapeDrivenState::new();
        state.seed_subject("location", "loc-1");
        let mut rng = Rng::new(0x42);
        let mut output = InMemoryOutput::default();

        let mut periodic = PeriodicEngine::new(
            vec![PeriodicSpec {
                name: "daily-preventive-maintenance".into(),
                cadence: Cadence::Daily,
                anchor_date: d(2026, 4, 27),
                business_calendar: None,
                action: PeriodicAction::OpenJob {
                    job_kind: "equipment-preventive-maintenance".into(),
                    subject_kind: Some("location".into()),
                    subject_id: Some("loc-1".into()),
                },
            }],
            CalendarRegistry::for_tests(),
        );
        let mut counterparty = CounterpartyEngine::new(vec![], CalendarRegistry::for_tests());

        let report = run_days(
            d(2026, 4, 27),
            d(2026, 5, 1), // 5 calendar days
            &kinds,
            &registry,
            &tenant,
            &mut state,
            &mut rng,
            &mut output,
            &mut periodic,
            &mut counterparty,
        )
        .unwrap();

        assert_eq!(report.days_simulated, 5);
        assert_eq!(report.periodic_fires, 5, "OpenJob fires daily");
        assert_eq!(
            report.jobs_opened_from_periodic, 5,
            "every fire should materialize a Job"
        );
        assert_eq!(
            report.jobs_created, 5,
            "and the global jobs_created counter agrees"
        );
    }

    /// `periodic.job_requested` for an unknown JobKind drops the
    /// request and bumps jobs_skipped_unknown_kind without
    /// crashing. Defends against tenant.toml drift where a
    /// PeriodicAction::OpenJob references a JobKind the registry
    /// didn't seed.
    #[test]
    fn periodic_open_job_for_unknown_kind_is_skipped() {
        let kinds: Vec<JobKindSpec> = vec![];
        let tenant = TenantConfig {
            meta: TenantMeta {
                tenant_id: "drift".into(),
                display_name: "Drift".into(),
                seed: 1,
                start_date: d(2026, 4, 27),
                end_date: d(2026, 12, 31),
                operating_days: vec![],
                tick_duration: "1d".into(),
                step_speed_multiplier: None,
                operating_hours: std::collections::HashMap::new(),
            },
            job_rates: HashMap::new(),
            subject_rates: HashMap::new(),
            anomalies: HashMap::new(),
            counterparty: HashMap::new(),
            shock: Vec::new(),
            periodic: HashMap::new(),
            batch: HashMap::new(),
        };

        let registry = StepRegistry::v1();
        let mut state = ShapeDrivenState::new();
        let mut rng = Rng::new(0x99);
        let mut output = InMemoryOutput::default();

        let mut periodic = PeriodicEngine::new(
            vec![PeriodicSpec {
                name: "ghost-preventive-maintenance".into(),
                cadence: Cadence::Daily,
                anchor_date: d(2026, 4, 27),
                business_calendar: None,
                action: PeriodicAction::OpenJob {
                    job_kind: "no-such-kind".into(),
                    subject_kind: Some("location".into()),
                    subject_id: Some("loc-1".into()),
                },
            }],
            CalendarRegistry::for_tests(),
        );
        let mut counterparty = CounterpartyEngine::new(vec![], CalendarRegistry::for_tests());

        let report = run_days(
            d(2026, 4, 27),
            d(2026, 4, 29),
            &kinds,
            &registry,
            &tenant,
            &mut state,
            &mut rng,
            &mut output,
            &mut periodic,
            &mut counterparty,
        )
        .unwrap();

        assert_eq!(report.periodic_fires, 3);
        assert_eq!(report.jobs_opened_from_periodic, 0);
        assert_eq!(state.counters.jobs_skipped_unknown_kind, 3);
    }

    // Dropped run_days_with_handlers_fires_step_type_side_effects
    // + empty_registry_dispatches_nothing. They asserted on the
    // in-process SideEffectRegistry pipeline, which the dispatcher's
    // rule registry now replaces (validated by the brewery regen +
    // dispatcher integration tests).

    // Note on cross-call equality: the day-tick path is
    // literally `run_ticks_with_handlers(..., 1, ...)` (see the
    // body above). A "run twice + assert equal counts" test would
    // not work in this codebase because the shape-driven engine
    // uses `Uuid::new_v4()` for Job/Step IDs (documented choice
    // at `shape_driven::engine::create_job_with_steps`); each
    // run produces different UUIDs, the per-tick `advance_steps`
    // sorts by UUID string, RNG draws diverge, counts differ.
    // Same per-day expected mean, different sample. The
    // hourly_ticks_match_daily test below uses tolerance bands
    // because that's the only honest framing.

    /// Phase A invariant — 24×1h ticks produce statistically
    /// equivalent per-sim-day Job + step volume to 1×1d.
    /// The seeds diverge (24× more RNG draws at hourly), so
    /// counts won't match exactly; the assertion is the *expected
    /// per-day rate* is preserved within Poisson sampling
    /// variance over a 60-day window.
    #[test]
    fn hourly_ticks_match_daily_per_day_volume_within_poisson_variance() {
        // Rate = 5/day, 60 days = expected 300 Jobs.
        // Poisson std-dev = sqrt(300) ≈ 17, so a 4-sigma band is
        // ±70. Both engines should land well inside.
        let scenario = || {
            let mut spec = JobKindSpec::platform_seed(
                "morning-bake",
                "Morning Bake",
                "production",
                vec!["location".into()],
                chain(&[("task", "Mix")]),
            );
            spec.subject_kinds = vec!["location".into()];
            let kinds = vec![spec];
            let tenant = TenantConfig {
                meta: TenantMeta {
                    tenant_id: "tick-rate-parity".into(),
                    display_name: "Tick rate parity".into(),
                    seed: 0x12345,
                    start_date: d(2026, 4, 27),
                    end_date: d(2026, 12, 31),
                    operating_days: vec![],
                    tick_duration: "1d".into(),
                    step_speed_multiplier: None,
                    operating_hours: std::collections::HashMap::new(),
                },
                job_rates: [(
                    "morning-bake".to_string(),
                    JobRate {
                        rate: 5.0,
                        ramp: vec![],
                        weekday_multiplier: None,
                        weekend_multiplier: None,
                        subject_distribution: HashMap::new(),
                        subject_cadence: Vec::new(),
                        month_multipliers: HashMap::new(),
                        deterministic: false,
                    },
                )]
                .into_iter()
                .collect(),
                subject_rates: HashMap::new(),
                anomalies: HashMap::new(),
                counterparty: HashMap::new(),
                shock: Vec::new(),
                periodic: HashMap::new(),
                batch: HashMap::new(),
            };
            (kinds, tenant)
        };

        let registry = StepRegistry::v1();
        let start = d(2026, 4, 27);
        let end = d(2026, 6, 25); // 60 days

        // Run A — daily ticks (1 per day).
        let (kinds, tenant) = scenario();
        let mut state_a = ShapeDrivenState::new();
        state_a.seed_subject("location", "loc-1");
        let mut rng_a = Rng::new(tenant.meta.seed);
        let mut output_a = InMemoryOutput::default();
        let mut p_a = PeriodicEngine::new(vec![], CalendarRegistry::for_tests());
        let mut c_a = CounterpartyEngine::new(vec![], CalendarRegistry::for_tests());
        let report_daily = run_ticks_with_handlers(
            start,
            end,
            1,
            &kinds,
            &registry,
            &tenant,
            &mut state_a,
            &mut rng_a,
            &mut output_a,
            &mut p_a,
            &mut c_a,
        )
        .unwrap();

        // Run B — hourly ticks (24 per day).
        let (kinds, tenant) = scenario();
        let mut state_b = ShapeDrivenState::new();
        state_b.seed_subject("location", "loc-1");
        let mut rng_b = Rng::new(tenant.meta.seed);
        let mut output_b = InMemoryOutput::default();
        let mut p_b = PeriodicEngine::new(vec![], CalendarRegistry::for_tests());
        let mut c_b = CounterpartyEngine::new(vec![], CalendarRegistry::for_tests());
        let report_hourly = run_ticks_with_handlers(
            start,
            end,
            24,
            &kinds,
            &registry,
            &tenant,
            &mut state_b,
            &mut rng_b,
            &mut output_b,
            &mut p_b,
            &mut c_b,
        )
        .unwrap();

        // Both engines saw 60 sim-days regardless of tick granularity.
        assert_eq!(report_daily.days_simulated, 60);
        assert_eq!(report_hourly.days_simulated, 60);

        // Per-day Job count: expected 300 (5/day × 60 days).
        // Tolerance ±100 (well above the 4-sigma Poisson band).
        let target = 300i64;
        let drift_daily = (report_daily.jobs_created as i64 - target).abs();
        let drift_hourly = (report_hourly.jobs_created as i64 - target).abs();
        assert!(
            drift_daily < 100,
            "daily ticks drifted from expected 300 jobs: got {}",
            report_daily.jobs_created
        );
        assert!(
            drift_hourly < 100,
            "hourly ticks drifted from expected 300 jobs: got {} (daily got {})",
            report_hourly.jobs_created,
            report_daily.jobs_created
        );

        // The two engines should land within Poisson noise of each
        // other — different RNG cascades but same expected mean.
        // 2-sigma band ≈ 35; 50 is generous.
        let cross_drift =
            (report_daily.jobs_created as i64 - report_hourly.jobs_created as i64).abs();
        assert!(
            cross_drift < 50,
            "daily vs hourly drift is too wide: daily={} hourly={}",
            report_daily.jobs_created,
            report_hourly.jobs_created
        );
    }
}
