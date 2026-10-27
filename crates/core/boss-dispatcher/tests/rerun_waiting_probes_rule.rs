//! The `rerun-waiting-probes-on-their-declared-close` rule (backlog
//! 9e9db2ea). A landed car whose probe said not-yet at arrival declares
//! the close it waits on (`waits_on.event`, the same declaration an owed
//! proof keys on); when a packet closes, this rule hands the close marker
//! to `jobs.run-car-probes` under `scope = "declared"`, and the handler
//! files a probe run for every OPEN waiting car that declared THAT close —
//! instead of the car waiting for the hourly recheck, which stays as the
//! safety net.
//!
//! Pins the rule as the dispatcher boots it: it fires on EVERY close —
//! which close fires which car is per-car data the handler reads — and it
//! fires the declared scope, not the owed, failing or arrival one.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "rerun-waiting-probes-on-their-declared-close";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn closed(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "p1", "closed_on": "2026-10-19", "kind": kind, "outcome": outcome,
        "title": "t", "subject_id": "s", "parent_step_id": null
    })
}

#[test]
fn every_close_hands_the_declared_scope_its_marker() {
    let reg = rule();
    for (kind, outcome) in [
        ("gate-run", serde_json::json!("green")),
        ("ops-request", serde_json::json!("answered")),
        ("pr-train", serde_json::Value::Null),
    ] {
        let hits = match_event(&reg, "jobs.job.closed", &closed(kind, outcome), &NoHelpers).matched;
        assert_eq!(hits.len(), 1, "a {kind} close fires the rule once");
        let inv = &hits[0].invocations[0];
        assert_eq!(inv.handler, "jobs.run-car-probes");
        assert!(
            inv.args
                .iter()
                .any(|(k, v)| k == "scope" && *v == Value::String("declared".into())),
            "the rule runs the DECLARED scope: {:?}",
            inv.args
        );
    }
}

#[test]
fn no_other_topic_fires_it() {
    let reg = rule();
    let hits = match_event(
        &reg,
        "jobs.job.created",
        &closed("gate-run", serde_json::Value::Null),
        &NoHelpers,
    )
    .matched;
    assert!(hits.is_empty());
}
