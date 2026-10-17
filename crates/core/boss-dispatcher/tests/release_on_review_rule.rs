//! The `release-a-held-car-on-its-reviewer-verdict` rule
//! (`infra/dispatcher/rules/release-a-held-car-on-its-reviewer-verdict.toml`,
//! design 393bdef6 Q1, backlog 0cfc57e6).
//!
//! The rule's only job is to WAKE the handler on a reviewer's recorded
//! verdict; the release door's judge decides, and the handler's tests
//! (`boss-dispatcher-handlers`, `jobs_release_on_review`) drive that
//! judge. These pin the trigger: the payload is the agent-run row the
//! job merge door publishes on `jobs.job.updated` after `boss review`
//! writes `review` onto it.

use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "release-a-held-car-on-its-reviewer-verdict";
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

fn fires(payload: &serde_json::Value) -> usize {
    match_event(&rule(), "jobs.job.updated", payload, &NoHelpers)
        .matched
        .len()
}

/// A recorded verdict naming a car wakes the handler, whatever the
/// verdict: the door refuses CHANGES, and the refusal is recorded.
#[test]
fn a_recorded_verdict_on_a_car_wakes_the_release_handler() {
    for v in ["release", "changes"] {
        let hits = match_event(
            &rule(),
            "jobs.job.updated",
            &reviewer_run(verdict(v)),
            &NoHelpers,
        )
        .matched;
        assert_eq!(hits.len(), 1, "{v}");
        assert_eq!(hits[0].invocations.len(), 1);
        assert_eq!(hits[0].invocations[0].handler, "jobs.release_on_review");
        assert!(
            hits[0].invocations[0].args.is_empty(),
            "the payload is the input"
        );
    }
}

/// No verdict, a verdict naming no car, and any packet that is not an
/// agent-run (a car's own write carries `review` nowhere it reads) wake
/// nothing: a missed wake leaves the car held, never released.
#[test]
fn nothing_without_a_verdict_on_a_car_wakes_it() {
    let mut no_review = reviewer_run(serde_json::Value::Null);
    no_review["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("review");
    assert_eq!(fires(&no_review), 0, "no verdict");
    assert_eq!(
        fires(&reviewer_run(
            serde_json::json!({ "car": "", "verdict": "release" })
        )),
        0,
        "a verdict naming no car"
    );
    let mut car = reviewer_run(verdict("release"));
    car["kind"] = serde_json::json!("ship-a-change");
    assert_eq!(fires(&car), 0, "only an agent-run's verdict");
}

#[test]
fn no_other_topic_wakes_it() {
    for topic in ["jobs.job.closed", "jobs.step.updated", "step.done.task"] {
        let hits = match_event(
            &rule(),
            topic,
            &reviewer_run(verdict("release")),
            &NoHelpers,
        )
        .matched;
        assert!(hits.is_empty(), "{topic}");
    }
}
