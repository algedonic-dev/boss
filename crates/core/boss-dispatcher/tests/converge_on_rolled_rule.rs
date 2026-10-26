//! The `complete-train-converged-on-runner-rolled` rule
//! (`infra/dispatcher/rules/`, backlog 090720c3): the cluster-deploy-
//! runner's mid-run `rolled` write on its `run` step hands the step to
//! `jobs.converge_trains_on_rolled`.
//!
//! Pins the `when` against the expr engine over the `jobs.step.updated`
//! payload (the step row, `events::step_state_payload`): it selects a run
//! step carrying `rolled`, and reads QUIETLY false — matched nothing,
//! skipped nothing — on every other step update on the topic. That topic
//! carries every step write on the board, and every maintenance chore's
//! `run` step is written with no `rolled` at all; a predicate that
//! errored there would be a warning on every chore tick.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "complete-train-converged-on-runner-rolled";
const SHA: &str = "9557a4d3fe62dfd3786b0d4f97cb3e3753f2bc0e";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn step(slug: &str, metadata: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "s1", "step_id": "s1", "job_id": "j1", "kind": "task",
        "title": "Run to completion", "spec_slug": slug, "status": "ready",
        "assignee_id": "automation:boss-step", "metadata": metadata,
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn a_run_step_recording_rolled_fires_the_handler_once() {
    let out = match_event(
        &rule(),
        "jobs.step.updated",
        &step(
            "run",
            serde_json::json!({ "rolled": SHA, "rolled_at": "2026-10-19T02:24:40Z" }),
        ),
        &NoHelpers,
    );
    assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    assert_eq!(out.matched.len(), 1);
    let inv = &out.matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.converge_trains_on_rolled");
    assert_eq!(
        arg(&inv.args, "train_kind"),
        Some(&Value::String("pr-train".into()))
    );
    assert_eq!(
        arg(&inv.args, "converge_kind"),
        Some(&Value::String("maintenance-cluster-converge".into()))
    );
}

#[test]
fn every_other_step_update_is_quietly_false() {
    for (slug, metadata) in [
        // An unchanged tick, a failed run, any other chore's run step.
        ("run", serde_json::json!({ "unchanged": "9557a4d" })),
        ("run", serde_json::json!({})),
        ("run", serde_json::json!({ "rolled": "" })),
        // A train's own steps, and a step whose metadata is empty.
        ("converged", serde_json::json!({ "rolled": SHA })),
        ("merged", serde_json::json!({ "merge_ref": "9557a4d3fe62" })),
    ] {
        let out = match_event(
            &rule(),
            "jobs.step.updated",
            &step(slug, metadata.clone()),
            &NoHelpers,
        );
        assert!(
            out.matched.is_empty(),
            "{slug} {metadata} must not fire: {:?}",
            out.matched
        );
        assert!(
            out.skipped.is_empty(),
            "{slug} {metadata} must read false, not fail: {:?}",
            out.skipped
        );
    }
}
