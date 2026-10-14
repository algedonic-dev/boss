//! The `file-backlog-items-on-dev-scratch-reclaim-red` rule
//! (`infra/dispatcher/rules/`): a maintenance-dev-scratch-reclaim packet
//! closing `failed` hands its run step to the chore judge, which files
//! ONE backlog-item per problem the pass named while one is open
//! (backlog 5e5d4be8: every pass read on 2026-10-08 closed `failed`,
//! hourly, and reached nobody — a pass failing every hour still ARRIVES,
//! so the silence sweep never sees it).
//!
//! Pins the `when` against the expr engine over the `jobs.job.closed`
//! payload, as `recovery_sheet_red_rule.rs` does for the recovery sheet:
//! it fires for exactly a pass that closed `failed`, with the step,
//! design and area the judge files with, and reads false on every other
//! close.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "file-backlog-items-on-dev-scratch-reclaim-red";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn closed(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "c1", "closed_on": "2026-10-08", "title": "Dev pod scratch reclaim",
        "kind": kind, "outcome": outcome, "subject_id": "infra/maintenance-dev-scratch-reclaim",
        "parent_step_id": null
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn a_failed_pass_fires_the_judge_once_with_the_run_step() {
    let hits = match_event(
        &rule(),
        "jobs.job.closed",
        &closed(
            "maintenance-dev-scratch-reclaim",
            serde_json::json!("failed"),
        ),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1, "a failed pass fires exactly once");
    let inv = &hits[0].invocations[0];
    assert_eq!(inv.handler, "maintenance.chore.file_reds");
    assert_eq!(arg(&inv.args, "step"), Some(&Value::String("run".into())));
    assert_eq!(
        arg(&inv.args, "design"),
        Some(&Value::String("5e5d4be8".into()))
    );
    assert_eq!(
        arg(&inv.args, "area"),
        Some(&Value::String("infra/cluster".into()))
    );
    assert!(
        arg(&inv.args, "no_red_failure_route").is_none(),
        "the fallback route is the break-glass deposit's alone (backlog e4a9a9b3)"
    );
}

#[test]
fn every_other_close_on_the_topic_is_quietly_false() {
    for (kind, outcome) in [
        (
            "maintenance-dev-scratch-reclaim",
            serde_json::json!("completed"),
        ),
        ("maintenance-dev-scratch-reclaim", serde_json::Value::Null),
        ("maintenance-recovery-sheet", serde_json::json!("failed")),
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
            "kind={kind} outcome={outcome} must NOT file dev-scratch-reclaim reds"
        );
    }
}
