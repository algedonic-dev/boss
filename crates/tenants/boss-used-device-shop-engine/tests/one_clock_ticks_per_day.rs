//! One clock for ticks-per-day — the sibling of
//! `boss-brewery-engine/tests/one_clock_ticks_per_day.rs` (the full
//! story of the 1440x warp-backfill rate bug lives there).
//!
//! `run_used_device_shop_one_tick` had the same re-derivation of
//! `ticks_per_day` from `tenant.meta` that starved the brewery's
//! Poisson rates whenever a caller's slicing disagreed with the
//! tenant's granularity. It now takes the caller's slice count as a
//! parameter; this pins that a day run as ONE slice (warp) and a day
//! run as 1440 slices both carry the full day's expected Job volume.

use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::step_registry::StepRegistry;
use boss_sim::calendar::CalendarRegistry;
use boss_sim::engines::{CounterpartyEngine, PeriodicEngine, RunReport, SimEventBus};
use boss_sim::output::InMemoryOutput;
use boss_sim::rng::Rng;
use boss_sim::shape_driven::{JobRate, ShapeDrivenState, TenantConfig, TenantMeta};
use boss_used_device_shop_engine::{
    UsedDeviceShopEngineState, run_used_device_shop_one_tick, used_device_shop_end_of_day,
};
use chrono::NaiveDate;
use std::collections::HashMap;

const DAYS: u32 = 30;
const RATE_PER_DAY: f64 = 5.0;
// Expected 150 Jobs; Poisson sigma = sqrt(150) ~ 12.2, 4-sigma ~ 49.
const BAND: std::ops::RangeInclusive<u64> = 101..=199;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

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
            title: "diagnose".into(),
            kind: "task".into(),
            ready_when: "steps.trigger.done".into(),
            title_template: "Diagnose".into(),
            terminal: Some(Terminal {
                outcome: "completed".into(),
            }),
            ..Default::default()
        },
    ]
}

fn engine(seed: u32) -> UsedDeviceShopEngineState {
    let tenant = TenantConfig {
        meta: TenantMeta {
            tenant_id: "one-clock-shop".into(),
            display_name: "One clock shop".into(),
            seed,
            start_date: d(2026, 4, 27),
            end_date: d(2026, 12, 31),
            operating_days: vec![],
            tick_duration: "1m".into(),
            step_speed_multiplier: None,
            operating_hours: HashMap::new(),
        },
        job_rates: [(
            "refurb-used".to_string(),
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
    state.seed_subject("asset", "ast-1");
    UsedDeviceShopEngineState {
        kinds: vec![WorkflowSpec::platform_seed(
            "refurb-used",
            "Refurb Used Device",
            "service",
            vec!["asset".into()],
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

fn run_days_in_slices(engine: &mut UsedDeviceShopEngineState, slices_per_day: u32) -> u64 {
    let mut output = InMemoryOutput::default();
    let mut day = d(2026, 4, 27);
    for _ in 0..DAYS {
        for tick_idx in 0..slices_per_day {
            run_used_device_shop_one_tick(engine, day, tick_idx, slices_per_day, &mut output)
                .unwrap();
        }
        used_device_shop_end_of_day(engine, day, &mut output).unwrap();
        day = day.succ_opt().unwrap();
    }
    engine.state.counters.jobs_created
}

#[test]
fn warp_single_slice_days_carry_full_day_volume() {
    let mut eng = engine(0xD0551);
    let jobs = run_days_in_slices(&mut eng, 1);
    assert!(
        BAND.contains(&jobs),
        "warp (1 slice/day) created {jobs} Jobs over {DAYS} days; \
         expected ~{} (4-sigma band {BAND:?})",
        DAYS as f64 * RATE_PER_DAY,
    );
}

#[test]
fn live_1440_slice_days_carry_full_day_volume() {
    let mut eng = engine(0xD0552);
    let jobs = run_days_in_slices(&mut eng, 1440);
    assert!(
        BAND.contains(&jobs),
        "live (1440 slices/day) created {jobs} Jobs over {DAYS} days; \
         expected ~{} (4-sigma band {BAND:?})",
        DAYS as f64 * RATE_PER_DAY,
    );
}
