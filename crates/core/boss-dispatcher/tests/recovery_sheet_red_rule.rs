//! The `file-backlog-items-on-recovery-sheet-red` rule
//! (`infra/dispatcher/rules/`): a maintenance-recovery-sheet packet
//! closing `failed` hands its run step to the crawl's judge, which files
//! ONE backlog-item for the recovery sheet while one is open (design
//! 125d405d car 3; review of car 3, run ff8ffd04, finding 3 — a check
//! failing every day still ARRIVES, so the silence sweep never sees it).
//!
//! Pins the `when` against the expr engine over the `jobs.job.closed`
//! payload, as `playground_crawl_red_rule.rs` does for the crawl: it
//! fires for exactly a check that closed `failed`, with the step, design
//! and area the judge files with, and reads false on every other close.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "file-backlog-items-on-recovery-sheet-red";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn closed(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "c1", "closed_on": "2026-09-30", "title": "Recovery sheet check",
        "kind": kind, "outcome": outcome, "subject_id": "infra/maintenance-recovery-sheet",
        "parent_step_id": null
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn a_failed_check_fires_the_judge_once_with_the_run_step_and_the_design() {
    let hits = match_event(
        &rule(),
        "jobs.job.closed",
        &closed("maintenance-recovery-sheet", serde_json::json!("failed")),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1, "a failed check fires exactly once");
    let inv = &hits[0].invocations[0];
    assert_eq!(inv.handler, "maintenance.chore.file_reds");
    assert_eq!(arg(&inv.args, "step"), Some(&Value::String("run".into())));
    assert_eq!(
        arg(&inv.args, "design"),
        Some(&Value::String("125d405d".into()))
    );
    assert_eq!(
        arg(&inv.args, "area"),
        Some(&Value::String("recovery".into()))
    );
}

#[test]
fn every_other_close_on_the_topic_is_quietly_false() {
    for (kind, outcome) in [
        ("maintenance-recovery-sheet", serde_json::json!("completed")),
        ("maintenance-recovery-sheet", serde_json::Value::Null),
        ("maintenance-playground-crawl", serde_json::json!("failed")),
        ("reprint-recovery-sheet", serde_json::json!("declined")),
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
            "kind={kind} outcome={outcome} must NOT file recovery-sheet reds"
        );
    }
}
