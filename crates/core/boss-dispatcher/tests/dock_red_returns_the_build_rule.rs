//! The `a-dock-red-returns-the-build-to-its-station` rule
//! (`infra/dispatcher/rules/a-dock-red-returns-the-build-to-its-station.toml`,
//! backlog 8f3d731a).
//!
//! The rule's only job is to WAKE the handler when the conductor writes a
//! car's dock re-gate red `returned`; the handler's tests
//! (`boss-dispatcher-handlers`, `jobs_dock_red_returns_the_build`) drive
//! what it does with the car and the item. The payload is the car row the
//! job merge door publishes on `jobs.job.updated` after the conductor's
//! hold write.

use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "a-dock-red-returns-the-build-to-its-station";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn car(red: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "aab5f34e-9526-418a-8d63-379db8e4f0af",
        "kind": "ship-a-change",
        "status": "open",
        "subject": { "id": "custom", "subject_kind": "custom" },
        "metadata": { "branch": "feat/x", "base_regate": {
            "main": "597184ab", "head": "bc7ec555", "gate_run": "33f1a846", "red": red } },
    })
}

fn red(garaged: bool, returned: bool) -> serde_json::Value {
    serde_json::json!({ "gate_run": "33f1a846", "head": "bc7ec555", "main": "597184ab",
                        "verdict": "failed", "checks": ["test"],
                        "garaged": garaged, "returned": returned })
}

fn fires(topic: &str, payload: &serde_json::Value) -> usize {
    match_event(&rule(), topic, payload, &NoHelpers)
        .matched
        .len()
}

/// A returned red on a car wakes the handler once, with the payload as
/// its only input.
#[test]
fn a_returned_dock_red_wakes_the_return() {
    let hits = match_event(
        &rule(),
        "jobs.job.updated",
        &car(red(true, true)),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].invocations.len(), 1);
    assert_eq!(
        hits[0].invocations[0].handler,
        "jobs.dock_red_returns_the_build"
    );
    assert!(hits[0].invocations[0].args.is_empty());
}

/// A red the dock still retries — first red or garaged at one head — no
/// red at all, a red written before `returned` existed, and a packet that
/// is not a car wake nothing.
#[test]
fn only_a_returned_red_on_a_car_wakes_it() {
    assert_eq!(fires("jobs.job.updated", &car(red(false, false))), 0);
    assert_eq!(fires("jobs.job.updated", &car(red(true, false))), 0);
    assert_eq!(fires("jobs.job.updated", &car(serde_json::Value::Null)), 0);
    let mut old = red(true, false);
    old.as_object_mut().unwrap().remove("returned");
    assert_eq!(fires("jobs.job.updated", &car(old)), 0, "pre-8f3d731a red");
    let mut run = car(red(true, true));
    run["kind"] = serde_json::json!("agent-run");
    assert_eq!(fires("jobs.job.updated", &run), 0, "only a car");
}

#[test]
fn no_other_topic_wakes_it() {
    for topic in ["jobs.job.closed", "jobs.step.updated", "step.done.task"] {
        assert_eq!(fires(topic, &car(red(true, true))), 0, "{topic}");
    }
}
