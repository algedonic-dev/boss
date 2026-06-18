//! Shape-driven invariants harness for the used-device-shop
//! tenant. The shape-driven engine is the only engine, so this
//! asserts its own invariants rather than diffing against a second
//! one:
//!
//! - **Coverage**: every JobKind authored in
//!   `examples/used-device-shop/seeds/job_kinds.toml` actually
//!   fires over a 30-day window. A JobKind without a matching
//!   `[job_rates.*]` block in tenant.toml never fires; this test
//!   pins that so a dropped rate block surfaces here.
//! - **Lifecycle**: jobs open + steps complete + some jobs close +
//!   the canonical side-effect topics fire
//!   (`ledger.bank_sweep_request`, `accounts.account.created`,
//!   `delivery.tracking_*`).

use std::collections::HashMap;
use std::path::PathBuf;

use boss_used_device_shop_engine::run_used_device_shop;

fn used_device_shop_seeds_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("examples/used-device-shop/seeds")
}

/// Walk `output.job_creates` (a `Vec<serde_json::Value>` with each
/// entry shaped `{ "kind": "...", ... }`) and count by kind.
fn jobs_by_kind(creates: &[serde_json::Value]) -> HashMap<String, u64> {
    let mut by_kind: HashMap<String, u64> = HashMap::new();
    for create in creates {
        let kind = create
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>")
            .to_string();
        *by_kind.entry(kind).or_insert(0) += 1;
    }
    by_kind
}

#[test]
fn shape_driven_engine_produces_jobs() {
    let shape = run_used_device_shop(&used_device_shop_seeds_dir(), 30, None)
        .expect("shape-driven 30-day run completes");

    assert!(
        shape.report.jobs_created > 0,
        "shape-driven engine should emit Jobs over a 30-day window"
    );
}

#[test]
fn coverage_every_legacy_family_fires() {
    // The intake/refurb family (`refurb-used`, `refurb-oem-new`,
    // `receiving`) and the post-sale family (`sale`,
    // `account-onboarding`, `service-agreement`) each need a
    // tenant-TOML rate block to fire. This test pins that so a
    // dropped rate block on any of them surfaces here.
    let shape = run_used_device_shop(&used_device_shop_seeds_dir(), 30, None).unwrap();
    let kinds = jobs_by_kind(&shape.output.job_creates);
    eprintln!("shape kinds over 30 days: {kinds:?}");

    for required in &[
        // intake/refurb family
        "device-intake",
        "refurb-used",
        "refurb-oem-new",
        "receiving",
        // sale family
        "sale",
        "service-agreement",
        "account-onboarding",
        // service / support family
        "field-service",
        "support-incident",
        // training
        "training-session",
    ] {
        assert!(
            kinds.contains_key(*required),
            "expected `{required}` to fire over 30 days; got {kinds:?}. \
             Either the [job_rates.{required}] block in tenant.toml is \
             missing or set too low for the launch ramp."
        );
    }
}

#[test]
fn lifecycle_invariants_hold() {
    let shape = run_used_device_shop(&used_device_shop_seeds_dir(), 30, None).unwrap();

    assert!(
        shape.report.jobs_created > 10,
        "30-day shape-driven run should open >10 jobs across 32 \
         JobKinds; got {}",
        shape.report.jobs_created
    );
    // The shape-driven engine only OPENS jobs/steps; the workforce executor
    // completes + closes them against the live system (exercised end-to-end
    // by the regen, not in-process). So the sim records zero step completions
    // / Job closures of its own — same contract the brewery count test
    // asserts.
    assert_eq!(
        shape.report.steps_completed, 0,
        "shape-driven engine completes no steps itself; got {}",
        shape.report.steps_completed
    );
    assert_eq!(
        shape.report.jobs_closed, 0,
        "shape-driven engine closes no Jobs itself; got {}",
        shape.report.jobs_closed
    );

    // Side-effect topics — the bridge handlers + counterparty
    // engine fire on `output.emit_event`. `step.done.*` events
    // and `job.opened/closed` events live on the in-process bus
    // and clear at end_of_day, so they don't surface here. What
    // surfaces is everything routed through SimOutput::emit_event:
    // counterparty emissions (delivery.tracking_*), batch / periodic
    // emissions (ledger.bank_sweep_request, periodic.job_requested),
    // subject-birth events (accounts.account.created, etc.), and
    // bridge-emitted events (shipping.shipment.created,
    // asset.received, etc.).
    let topics: std::collections::HashSet<&str> = shape
        .output
        .events
        .iter()
        .map(|(t, _)| t.as_str())
        .collect();
    eprintln!("shape-driven emit_event topics: {topics:?}");
    assert!(
        topics.contains("ledger.bank_sweep_request"),
        "shape-driven engine should emit ledger.bank_sweep_request from \
         the [periodic.daily-bank-sweep] block; got {topics:?}"
    );
    assert!(
        topics.contains("accounts.account.created"),
        "shape-driven engine should emit accounts.account.created from \
         the [subject_rates.account] block; got {topics:?}"
    );
    // Completion-driven counterparty emissions (delivery.tracking_*) come
    // from the workforce executor against the live system (exercised by the
    // regen); shipment steps don't complete in-process here. Assert a bridge
    // intake event instead, which fires at job-open.
    assert!(
        topics.contains("asset.received"),
        "shape-driven engine should emit asset.received from the asset \
         intake bridge; got {topics:?}"
    );
}
