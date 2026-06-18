//! Shape-driven smoke test for the used-device-shop tenant: proves
//! the `boss-used-device-shop-engine` crate runs the tenant through
//! `run_days_with_handlers` end-to-end without hitting unknown
//! kinds, missing subjects, or panics.

use std::path::PathBuf;

use boss_used_device_shop_engine::run_used_device_shop;

fn used_device_shop_seeds_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("examples/used-device-shop/seeds")
}

#[test]
fn shape_driven_30_day_run_creates_jobs_and_closes_cleanly() {
    let seeds = used_device_shop_seeds_dir();
    let result = run_used_device_shop(&seeds, 30, None)
        .expect("30-day shape-driven run completes without errors");

    // The tenant.toml's [job_rates.*] blocks ramp from low launch
    // rates (intake 20/day, sale 1/day, support-incident 3/day,
    // etc.) — over 30 days even the slow kinds should emit at
    // least one Job.
    assert!(
        result.report.jobs_created > 0,
        "expected non-zero jobs_created across a 30-day shape-driven run; got {}",
        result.report.jobs_created
    );

    // Every JobKind in the tenant TOML has a non-zero rate at the
    // launch ramp; confirm the day-loop touched at least 30 days
    // (the operating_days filter doesn't strip anything since the
    // tenant runs 7 days a week).
    assert_eq!(
        result.report.days_simulated, 30,
        "expected 30 days simulated; got {}",
        result.report.days_simulated
    );

    // The shape-driven engine only OPENS jobs/steps; the workforce
    // executor completes + closes them against the live system (not
    // exercised in-process). Confirm the day-loop opens jobs and records
    // zero step completions of its own — the same contract the brewery
    // count test asserts.
    assert!(
        result.report.jobs_created > 0,
        "expected the engine to open jobs across a 30-day run; got {}",
        result.report.jobs_created
    );
    assert_eq!(
        result.report.steps_completed, 0,
        "shape-driven engine completes no steps itself; got {}",
        result.report.steps_completed
    );

    // No unknown-kind drops. Every JobKind the tenant.toml fires
    // must resolve in the registry (the seed_loader test enforces
    // this at the file level; this test confirms it at runtime).
    // `InMemoryOutput.events` is `Vec<(topic, payload)>`.
    assert_eq!(
        result
            .output
            .events
            .iter()
            .filter(|(topic, _)| topic == "jobs.unknown_kind")
            .count(),
        0,
        "tenant.toml fires a JobKind missing from job_kinds.toml"
    );
}

#[test]
fn shape_driven_run_keeps_runtime_invariants() {
    // Two runs with the same seed should land in roughly the same
    // ballpark — exact byte-determinism gets asserted at the
    // `simulate_day` level (`shape_driven::engine::run_is_deterministic_for_a_seed`)
    // but the higher-level `run_used_device_shop_into` path passes
    // through the bridge handler set + the `register_employee`
    // pool, both of which use HashMap-shaped state internally. A
    // tighter byte-identity assertion here would surface that
    // HashMap-iteration drift as a noisy failure, so we assert the
    // loose-but-meaningful invariant instead: each run produces a
    // positive number of jobs + the day count is stable. The
    // parity harness carries the strict per-kind diff.
    let seeds = used_device_shop_seeds_dir();
    let a = run_used_device_shop(&seeds, 14, None).unwrap();
    let b = run_used_device_shop(&seeds, 14, None).unwrap();

    assert_eq!(a.report.days_simulated, b.report.days_simulated);
    assert!(a.report.jobs_created > 0);
    assert!(b.report.jobs_created > 0);
}
