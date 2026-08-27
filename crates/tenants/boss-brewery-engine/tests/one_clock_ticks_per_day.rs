//! One clock for ticks-per-day — the daemon/engine boundary invariant.
//!
//! `engines/tick.rs` claims per-day expected volume is invariant of
//! tick granularity: a sim-day's ticks always sum to 24.0 hours, so a
//! `rate = 5.0` Workflow opens ~5 Jobs/day whether the day is run as
//! 1 slice (warp backfill) or 1440 (live, `tick_duration = "1m"`).
//!
//! That only holds if the N the caller slices the day into is the
//! same N the engine sizes ticks by. The 2026-08 backfill bug was two
//! clocks: the warp daemon ran 1 slice per sim-day while
//! `run_brewery_one_tick` re-derived `ticks_per_day = 1440` from the
//! tenant, building a 1-minute tick for the day's only slice — every
//! Poisson job rate and subject-birth rate ran at 1/1440 strength for
//! the entire fabricated year (seasonal-release fired 3 times ever,
//! intended ~30/yr). The fix is the `ticks_per_day` parameter on
//! `run_brewery_one_tick`: the caller's slicing IS the clock, and the
//! engine can no longer disagree with it.
//!
//! Both tests drive whole sim-days through the reentrant per-tick API
//! exactly as `boss-brewery-sim` does — N calls, then the end-of-day
//! flush — and assert the day's expected volume came out, within a
//! 4-sigma Poisson band. Before the fix the warp-shaped run created
//! ~0 Jobs instead of ~150.

use boss_brewery_engine::{BreweryEngineState, brewery_end_of_day, run_brewery_one_tick};
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::step_registry::StepRegistry;
use boss_sim::calendar::CalendarRegistry;
use boss_sim::engines::{CounterpartyEngine, PeriodicEngine, RunReport, SimEventBus};
use boss_sim::output::InMemoryOutput;
use boss_sim::rng::Rng;
use boss_sim::shape_driven::{JobRate, ShapeDrivenState, TenantConfig, TenantMeta};
use chrono::NaiveDate;
use std::collections::HashMap;

const DAYS: u32 = 30;
const RATE_PER_DAY: f64 = 5.0;
// Expected 150 Jobs; Poisson sigma = sqrt(150) ~ 12.2, 4-sigma ~ 49.
const BAND: std::ops::RangeInclusive<u64> = 101..=199;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// Trigger -> one terminal task step; enough for Jobs to open and
/// close without any tenant seed bundle.
fn steps() -> Vec<StepSpec> {
    vec![
        StepSpec {
            title: "trigger".into(),
            kind: "trigger".into(),
            ready_when: "true".into(),
            title_template: "Opened".into(),
            ..Default::default()
        },
        StepSpec {
            title: "brew".into(),
            kind: "task".into(),
            ready_when: "steps.trigger.done".into(),
            title_template: "Brew".into(),
            terminal: Some(Terminal {
                outcome: "completed".into(),
            }),
            ..Default::default()
        },
    ]
}

/// A synthetic engine state: one Workflow at 5 Jobs/day, one seeded
/// Subject, every day operating. `tick_duration = "1m"` matches the
/// production brewery tenant — it is what the pre-fix engine
/// re-derived 1440 from, and what the daemon's live phase passes.
fn engine(seed: u32) -> BreweryEngineState {
    let tenant = TenantConfig {
        meta: TenantMeta {
            tenant_id: "one-clock".into(),
            display_name: "One clock".into(),
            seed,
            start_date: d(2026, 4, 27),
            end_date: d(2026, 12, 31),
            operating_days: vec![],
            tick_duration: "1m".into(),
            step_speed_multiplier: None,
            operating_hours: HashMap::new(),
        },
        job_rates: [(
            "seasonal-release".to_string(),
            JobRate {
                rate: RATE_PER_DAY,
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
    let mut state = ShapeDrivenState::new();
    state.seed_subject("location", "loc-1");
    BreweryEngineState {
        kinds: vec![WorkflowSpec::platform_seed(
            "seasonal-release",
            "Seasonal Release",
            "production",
            vec!["location".into()],
            steps(),
        )],
        registry: StepRegistry::v1(),
        tenant,
        state,
        rng: Rng::new(seed),
        periodic: PeriodicEngine::new(vec![], CalendarRegistry::for_tests()),
        counterparty: CounterpartyEngine::new(vec![], CalendarRegistry::for_tests()),
        bus: SimEventBus::new(),
        report: RunReport::default(),
    }
}

/// Drive `DAYS` sim-days the way the daemon does: `slices_per_day`
/// calls to `run_brewery_one_tick`, then `brewery_end_of_day`.
/// Returns total Jobs created.
fn run_days_in_slices(engine: &mut BreweryEngineState, slices_per_day: u32) -> u64 {
    let mut output = InMemoryOutput::default();
    let mut day = d(2026, 4, 27);
    for _ in 0..DAYS {
        for tick_idx in 0..slices_per_day {
            run_brewery_one_tick(engine, day, tick_idx, slices_per_day, &mut output).unwrap();
        }
        brewery_end_of_day(engine, day, &mut output).unwrap();
        day = day.succ_opt().unwrap();
    }
    engine.state.counters.jobs_created
}

/// Warp backfill: the daemon collapses each sim-day to ONE slice.
/// That one slice must still carry the full 24 hours — the full
/// day's expected Job volume.
#[test]
fn warp_single_slice_days_carry_full_day_volume() {
    let mut eng = engine(0xB0551);
    let jobs = run_days_in_slices(&mut eng, 1);
    assert!(
        BAND.contains(&jobs),
        "warp (1 slice/day) created {jobs} Jobs over {DAYS} days; \
         expected ~{} (4-sigma band {BAND:?}). A shortfall of ~1440x \
         means the engine is sizing ticks from its own clock instead \
         of the caller's slicing.",
        DAYS as f64 * RATE_PER_DAY,
    );
}

/// Live: 1440 one-minute slices per sim-day sum to the same 24
/// hours and the same expected volume.
#[test]
fn live_1440_slice_days_carry_full_day_volume() {
    let mut eng = engine(0xB0552);
    let jobs = run_days_in_slices(&mut eng, 1440);
    assert!(
        BAND.contains(&jobs),
        "live (1440 slices/day) created {jobs} Jobs over {DAYS} days; \
         expected ~{} (4-sigma band {BAND:?})",
        DAYS as f64 * RATE_PER_DAY,
    );
}
