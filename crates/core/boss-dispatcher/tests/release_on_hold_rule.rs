//! The `release-a-held-car-when-its-review-hold-is-placed` rule
//! (`infra/dispatcher/rules/release-a-held-car-when-its-review-hold-is-placed.toml`,
//! backlog 60993cda), the second trigger of the release handler.
//!
//! Like its sibling `release_on_review_rule.rs`, this pins only the WAKE:
//! the handler (`boss-dispatcher-handlers`, `jobs_release_on_review`)
//! reads the car and its reviewer and asks the door, whose judge decides.
//! The payload is the post-write step row `jobs.step.updated` carries
//! (`boss_jobs::events::step_state_payload`). The shapes below are the
//! live review step's: the auto-park green writes `hold` and `hold_sha`,
//! `boss dispatch` names the reviewer as `agent_run`, and a release
//! deletes `hold` (or, on rows written before that, sets it `false`).

use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::registry::{Registry, match_event};
use serde_json::{Value, json};

mod common;

const RULE: &str = "release-a-held-car-when-its-review-hold-is-placed";
const H1: &str = "1111111111111111111111111111111111111111";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn review_step(md: Value) -> Value {
    json!({
        "id": "00000000-0000-4000-8000-000000000003",
        "step_id": "00000000-0000-4000-8000-000000000003",
        "job_id": "c0ffee00-0000-4000-8000-000000000000",
        "kind": "task",
        "title": "Open for review",
        "spec_slug": "review",
        "status": "active",
        "metadata": md,
    })
}

/// How many invocations the rule makes on `payload`, and that the match
/// itself did not fail: a predicate error is not a quiet "no".
fn fires(topic: &str, payload: &Value) -> usize {
    let out = match_event(&rule(), topic, payload, &NoHelpers);
    assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    out.matched
        .iter()
        .map(|m| {
            assert!(
                m.invocations
                    .iter()
                    .all(|i| i.handler == "jobs.release_on_review" && i.args.is_empty()),
                "the payload is the input: {m:?}"
            );
            m.invocations.len()
        })
        .sum()
}

/// A held review step that names its reviewer and its head wakes the
/// release handler — the auto-park green's write, and every later write
/// while it stands.
#[test]
fn a_review_hold_naming_a_reviewer_and_a_head_wakes_the_release_handler() {
    let held = review_step(json!({"hold": "trust-boundary car (area policy)",
                                  "hold_sha": H1, "agent_run": "5eed5eed"}));
    assert_eq!(fires("jobs.step.updated", &held), 1);
}

/// `hold` is not ordered by the predicate: a bare `hold: true` and an
/// old `hold: false` evaluate without error (the handler reads the hold
/// through `stranded::hold_reason`; a released step is a no-op there).
#[test]
fn a_hold_of_any_shape_evaluates_without_error() {
    for hold in [json!(true), json!(false), Value::Null] {
        let step = review_step(json!({"hold": hold, "hold_sha": H1, "agent_run": "5eed5eed"}));
        assert_eq!(fires("jobs.step.updated", &step), 1, "{hold}");
    }
}

/// No reviewer named, no head, or another step's write: nothing wakes.
/// A missed wake leaves the car held, never released.
#[test]
fn nothing_without_a_reviewer_and_a_head_on_a_review_step_wakes_it() {
    let unnamed = review_step(json!({"hold": "x", "hold_sha": H1}));
    let unbound = review_step(json!({"hold": "x", "agent_run": "5eed5eed"}));
    let mut other = review_step(json!({"hold": "x", "hold_sha": H1, "agent_run": "5eed5eed"}));
    other["spec_slug"] = json!("gate");
    for (what, step) in [
        ("no reviewer", unnamed),
        ("no head", unbound),
        ("another step", other),
    ] {
        assert_eq!(fires("jobs.step.updated", &step), 0, "{what}");
    }
}

/// Only a step write wakes it; the verdict's write is the sibling rule's.
#[test]
fn no_other_topic_wakes_it() {
    let held = review_step(json!({"hold": "x", "hold_sha": H1, "agent_run": "5eed5eed"}));
    for topic in ["jobs.job.updated", "jobs.step.completed", "step.done.task"] {
        assert_eq!(fires(topic, &held), 0, "{topic}");
    }
}
