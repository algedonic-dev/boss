//! The `file-backlog-items-on-break-glass-deposit-red` rule
//! (`infra/dispatcher/rules/`): a maintenance-break-glass-deposit packet
//! closing `failed` hands its run step to the chore judge, which files
//! one backlog-item per RED route while one is open — the refusal, and
//! the authorized_keys line David places (review b6d2a716 of 7336cb5f,
//! N1; backlog 17a7bd18). A run that closes `not-yet` (the image-only
//! policy only reports) or `completed` files nothing.
//!
//! Pins the `when` against the expr engine over the `jobs.job.closed`
//! payload, as `recovery_sheet_red_rule.rs` does for its chore.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "file-backlog-items-on-break-glass-deposit-red";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn closed(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "c1", "closed_on": "2026-09-30", "title": "Break-glass kubeconfig deposit to boss-gcp",
        "kind": kind, "outcome": outcome, "subject_id": "infra/maintenance-break-glass-deposit",
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
        &closed(
            "maintenance-break-glass-deposit",
            serde_json::json!("failed"),
        ),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1, "a failed check fires exactly once");
    let inv = &hits[0].invocations[0];
    assert_eq!(inv.handler, "maintenance.chore.file_reds");
    assert_eq!(arg(&inv.args, "step"), Some(&Value::String("run".into())));
    assert_eq!(
        arg(&inv.args, "design"),
        Some(&Value::String("835c0c9c".into()))
    );
    assert_eq!(
        arg(&inv.args, "area"),
        Some(&Value::String("credentials".into()))
    );
}

#[test]
fn every_other_close_on_the_topic_is_quietly_false() {
    for (kind, outcome) in [
        (
            "maintenance-break-glass-deposit",
            serde_json::json!("completed"),
        ),
        ("maintenance-break-glass-deposit", serde_json::Value::Null),
        ("maintenance-playground-crawl", serde_json::json!("failed")),
        (
            "maintenance-break-glass-deposit",
            serde_json::json!("not-yet"),
        ),
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
            "kind={kind} outcome={outcome} must NOT file break-glass deposit reds"
        );
    }
}
