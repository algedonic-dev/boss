//! The `file-backlog-items-on-forge-converge-red` rule
//! (`infra/dispatcher/rules/`): a maintenance-forge-converge packet
//! closing `failed` hands its run step to the chore judge, which files
//! ONE backlog-item per `RED <route> …` line the converge left there
//! while one is open (delta review aa901496 of car 14142ab6, the ALARM
//! note; backlog 62b09c57 — a refused rewind of main or a HELD install
//! was a red converge every ten minutes that still ARRIVED, so the
//! silence sweep never saw it and nothing opened).
//!
//! Pins the `when` against the expr engine over the `jobs.job.closed`
//! payload, as `recovery_sheet_red_rule.rs` does: it fires for exactly a
//! forge converge that closed `failed`, with the step, design and area
//! the judge files with, and reads false on every other close — the
//! CLUSTER converge among them, whose reds are the trains' to raise.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "file-backlog-items-on-forge-converge-red";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn closed(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "c1", "closed_on": "2026-10-08",
        "title": "Forge host converges on its own units",
        "kind": kind, "outcome": outcome, "subject_id": "infra/maintenance-forge-converge",
        "parent_step_id": null
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn a_failed_forge_converge_fires_the_judge_once_with_the_run_step() {
    let hits = match_event(
        &rule(),
        "jobs.job.closed",
        &closed("maintenance-forge-converge", serde_json::json!("failed")),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1, "a failed converge fires exactly once");
    let inv = &hits[0].invocations[0];
    assert_eq!(inv.handler, "maintenance.chore.file_reds");
    assert_eq!(arg(&inv.args, "step"), Some(&Value::String("run".into())));
    assert_eq!(
        arg(&inv.args, "design"),
        Some(&Value::String("c98c79aa".into()))
    );
    assert_eq!(arg(&inv.args, "area"), Some(&Value::String("forge".into())));
    // The fallback that files a red with no RED line is the break-glass
    // deposit's alone (backlog e4a9a9b3); a converge red for a passing
    // reason is read on its packet, not opened.
    assert_eq!(arg(&inv.args, "no_red_failure_route"), None);
}

#[test]
fn every_other_close_on_the_topic_is_quietly_false() {
    for (kind, outcome) in [
        ("maintenance-forge-converge", serde_json::json!("completed")),
        ("maintenance-forge-converge", serde_json::Value::Null),
        ("maintenance-cluster-converge", serde_json::json!("failed")),
        ("maintenance-boss-gcp-converge", serde_json::json!("failed")),
        ("backlog-item", serde_json::json!("stale")),
    ] {
        let hits = match_event(
            &rule(),
            "jobs.job.closed",
            &closed(kind, outcome.clone()),
            &NoHelpers,
        )
        .matched;
        assert!(
            hits.is_empty(),
            "kind={kind} outcome={outcome} must NOT file forge-converge reds"
        );
    }
}
