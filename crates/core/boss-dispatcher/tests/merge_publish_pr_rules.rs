//! The two rules of publish-to-github's `merge` step (backlog
//! 602fe95f, David 2026-09-30: "NOBODY pushes or merges main by hand"),
//! pinned over the shipped rule directory, not a copy:
//!
//! - `merge-publish-pr-on-merge-ready` files the forge's
//!   `merge-publish-pr` request when the step goes ready, carrying the
//!   publish packet back as `for_publish`;
//! - `complete-publish-merge-step-on-merge-publish-pr-answered` reads the
//!   verb's answer: `merged <url> — main reads <sha>` completes the step
//!   with `merged_sha`, and a FAILED or REFUSED run troubles it (the
//!   handler's annotate-and-alert), because a merge that did not happen
//!   and says nothing is the five silent hours of f47861a5 again.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::match_event;

mod common;

const FILING: &str = "merge-publish-pr-on-merge-ready";
const ANSWER: &str = "complete-publish-merge-step-on-merge-publish-pr-answered";

fn step_ready(metadata: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "job_id": "p1", "step_id": "s1", "kind": "task",
        "subject_kind": "custom", "subject_id": "github-mirror",
        "assignee_id": null,
        "metadata": metadata
    })
}

fn get(args: &[(String, Value)], k: &str) -> Value {
    args.iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
}

#[test]
fn the_merge_step_spawns_a_forge_merge_request_carrying_its_packet() {
    let reg = common::authored_rule(FILING);
    let out = match_event(
        &reg,
        "step.ready.task",
        &step_ready(serde_json::json!({ "ops_verb": "merge-publish-pr" })),
        &NoHelpers,
    );
    assert!(out.skipped.is_empty(), "must evaluate: {:?}", out.skipped);
    assert_eq!(
        out.matched.len(),
        1,
        "the merge step fires exactly one request"
    );
    let args = &out.matched[0].invocations[0].args;
    assert_eq!(out.matched[0].invocations[0].handler, "jobs.spawn");
    assert_eq!(get(args, "kind"), Value::String("ops-request".into()));
    assert_eq!(get(args, "subject"), Value::String("forge".into()));
    assert_eq!(
        get(args, "metadata.verb"),
        Value::String("merge-publish-pr".into())
    );
    assert_eq!(get(args, "metadata.host"), Value::String("forge".into()));
    assert_eq!(
        get(args, "metadata.for_publish"),
        Value::String("p1".into()),
        "for_publish carries the publish packet's own id — the answer rule's edge"
    );
    // No args list: the verb's argv fixes --merge, and a rule-filed
    // request can reach nothing else.
    assert!(
        !args.iter().any(|(k, _)| k.starts_with("metadata.args")),
        "the filing hands the verb no arguments: {args:?}"
    );
}

#[test]
fn other_task_steps_and_the_done_topic_do_not_fire_the_merge() {
    let reg = common::authored_rule(FILING);
    for metadata in [
        serde_json::json!({}),
        serde_json::json!({ "ops_verb": "publish-github-pr" }),
        serde_json::json!({ "ops_verb": "read-publish-checks" }),
        serde_json::json!({ "ops_verb": "" }),
    ] {
        let out = match_event(
            &reg,
            "step.ready.task",
            &step_ready(metadata.clone()),
            &NoHelpers,
        );
        assert!(out.matched.is_empty(), "{metadata} must NOT file a merge");
        assert!(out.skipped.is_empty(), "{metadata} must evaluate to false");
    }
    let out = match_event(
        &reg,
        "step.done.task",
        &step_ready(serde_json::json!({ "ops_verb": "merge-publish-pr" })),
        &NoHelpers,
    );
    assert!(
        out.matched.is_empty(),
        "a completed merge files no second one"
    );
}

#[test]
fn the_answer_completes_merge_from_the_verbs_own_line_and_troubles_it_on_failure() {
    let reg = common::authored_rule(ANSWER);
    let closed = |kind: &str, outcome: &str| {
        serde_json::json!({
            "id": "c98a782f-adb4-40a9-860a-456063cfe66a", "kind": kind,
            "outcome": outcome, "closed_on": "2026-09-30", "parent_step_id": null,
        })
    };
    let out = match_event(
        &reg,
        "jobs.job.closed",
        &closed("ops-request", "answered"),
        &NoHelpers,
    );
    assert!(out.skipped.is_empty(), "must evaluate: {:?}", out.skipped);
    assert_eq!(out.matched.len(), 1);
    let inv = &out.matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.complete_linked_step");
    let args = &inv.args;
    assert_eq!(get(args, "verb"), Value::String("merge-publish-pr".into()));
    assert_eq!(get(args, "link"), Value::String("for_publish".into()));
    assert_eq!(get(args, "steps"), Value::String("merge".into()));
    assert_eq!(
        get(args, "on_failure"),
        Value::String("annotate-and-alert".into()),
        "a merge that did not happen must trouble its step, never leave it quiet"
    );
    let Value::String(pattern) = get(args, "verdict_pattern") else {
        panic!("verdict_pattern is a string");
    };
    assert!(
        !pattern.contains('\\'),
        "no backslash survives boss-expr: {pattern}"
    );
    // The pattern matches the verb's own answer line — the one its verb
    // file's `effect` also names — and captures the sha main reads.
    let re = regex::Regex::new(&pattern).expect("the pattern is a regex");
    let line = "publish-github-pr: merged https://github.com/algedonic-dev/boss/pull/250 — main reads 0123456789abcdef0123456789abcdef01234567";
    let caps = re.captures(line).expect("the answer line matches");
    assert_eq!(
        &caps["merged_sha"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(
        &caps["pr_url"],
        "https://github.com/algedonic-dev/boss/pull/250"
    );
    for not_an_answer in [
        "publish-github-pr: REFUSED — check Gate (infra/gate.sh, full) on 0123 concluded failure",
        "publish-github-pr: FAILED — pushing the snapshot to main: remote rejected",
        "publish-github-pr: opened https://github.com/algedonic-dev/boss/pull/250 at 0123456789abcdef",
    ] {
        assert!(
            !re.is_match(not_an_answer),
            "{not_an_answer} read as a merge"
        );
    }
    let Value::String(done) = get(args, "done_metadata") else {
        panic!("done_metadata is a string");
    };
    let done: serde_json::Value = serde_json::from_str(&done).expect("done_metadata is JSON");
    assert_eq!(done["merged_sha"], "{merged_sha}");

    for (kind, outcome) in [("ops-request", "refused"), ("publish-to-github", "merged")] {
        let out = match_event(&reg, "jobs.job.closed", &closed(kind, outcome), &NoHelpers);
        assert!(out.matched.is_empty(), "{kind}/{outcome} must not fire");
        assert!(out.skipped.is_empty(), "{kind}/{outcome} must evaluate");
    }
}
