//! The `a-changes-verdict-returns-the-build-to-its-station` rule
//! (`infra/dispatcher/rules/a-changes-verdict-returns-the-build-to-its-station.toml`,
//! backlog c2b21676).
//!
//! The rule's only job is to WAKE the handler on a reviewer's recorded
//! CHANGES; the handler's tests (`boss-dispatcher-handlers`,
//! `jobs_changes_return_the_build`) drive what it does with the car and
//! the item. These pin the trigger: the payload is the agent-run row the
//! job merge door publishes on `jobs.job.updated` after `boss review`
//! writes `review` onto it — the same write the release rule wakes on.

use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "a-changes-verdict-returns-the-build-to-its-station";
const CAR: &str = "c0ffee00-0000-4000-8000-000000000000";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn reviewer_run(review: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "5eed5eed-0000-4000-8000-000000000000",
        "kind": "agent-run",
        "status": "open",
        "subject": { "id": "custom", "subject_kind": "custom" },
        "metadata": { "packet": CAR, "step": "review", "review": review },
    })
}

fn verdict(v: &str) -> serde_json::Value {
    serde_json::json!({ "car": CAR, "branch": "feat/x", "verdict": v,
                        "reviewed_sha": "1111111111111111111111111111111111111111" })
}

fn fires(topic: &str, payload: &serde_json::Value) -> usize {
    match_event(&rule(), topic, payload, &NoHelpers)
        .matched
        .len()
}

/// A recorded CHANGES naming a car wakes the handler once, with the
/// payload as its only input.
#[test]
fn a_changes_verdict_on_a_car_wakes_the_return() {
    let hits = match_event(
        &rule(),
        "jobs.job.updated",
        &reviewer_run(verdict("changes")),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].invocations.len(), 1);
    assert_eq!(
        hits[0].invocations[0].handler,
        "jobs.changes_return_the_build"
    );
    assert!(
        hits[0].invocations[0].args.is_empty(),
        "the payload is the input"
    );
}

/// A RELEASE is the release rule's, not this one's; no verdict, a
/// verdict naming no car, and a packet that is not an agent-run wake
/// nothing.
#[test]
fn only_a_changes_verdict_on_a_car_wakes_it() {
    assert_eq!(
        fires("jobs.job.updated", &reviewer_run(verdict("release"))),
        0,
        "a release"
    );
    let mut no_review = reviewer_run(serde_json::Value::Null);
    no_review["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("review");
    assert_eq!(fires("jobs.job.updated", &no_review), 0, "no verdict");
    assert_eq!(
        fires(
            "jobs.job.updated",
            &reviewer_run(serde_json::json!({ "car": "", "verdict": "changes" }))
        ),
        0,
        "a verdict naming no car"
    );
    let mut car = reviewer_run(verdict("changes"));
    car["kind"] = serde_json::json!("ship-a-change");
    assert_eq!(fires("jobs.job.updated", &car), 0, "only an agent-run");
}

#[test]
fn no_other_topic_wakes_it() {
    for topic in ["jobs.job.closed", "jobs.step.updated", "step.done.task"] {
        assert_eq!(
            fires(topic, &reviewer_run(verdict("changes"))),
            0,
            "{topic}"
        );
    }
}
