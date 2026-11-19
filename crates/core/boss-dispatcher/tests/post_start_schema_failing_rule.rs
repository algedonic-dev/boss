//! The `file-backlog-items-on-post-start-schema-failing` rule
//! (`infra/dispatcher/rules/`): a maintenance-post-start-schema pass
//! whose `run` step records `result = "failed"` hands that step to the
//! streak judge (`maintenance.verdict.streak`), which files ONE
//! backlog-item when the newest N completed passes ALL read failed.
//!
//! WHY (backlog f93bdee4). Every pass from 2026-11-01 to 2026-11-09
//! failed — 11,299 packets, one a minute, each closed at "Post-start
//! indexes still owed" — and nothing read one of them. The COUNT lives in
//! the handler and is pinned beside it (`handlers/verdict_streak.rs`);
//! what this file pins is the selection, the word, and the N the rule
//! declares. An `ok` pass is selected too: the run that breaks a streak
//! is what closes its item (backlog 3ff17da3).

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::match_event;

mod common;

const RULE: &str = "file-backlog-items-on-post-start-schema-failing";

/// The `step.done.task` marker as boss-jobs emits it for a maintenance
/// packet's `run` step — the shape boss_gcp_deposit_key_rule.rs pins.
fn run_done(
    workflow_kind: &str,
    spec_slug: &str,
    metadata: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "job_id": "c-1", "step_id": "s-run", "kind": "task",
        "subject_kind": "custom", "subject_id": "infra/maintenance-post-start-schema",
        "workflow_kind": workflow_kind,
        "completed_on": "2026-11-09",
        "metadata": metadata,
        "notify_on_done": false,
        "spec_slug": spec_slug
    })
}

/// A failed pass as boss-chore.sh recorded it on packet c17009a8
/// (2026-11-09T18:50Z), its output cut to the lines that matter.
fn failed_pass() -> serde_json::Value {
    serde_json::json!({
        "result": "failed",
        "exit_status": "125",
        "output": "post-start: initial precheck exit 125\nmigrate.sh: post-start debt paid — no pending entries\npost-start: command left descendants; refusing success\npost-start: apply outcome exit 125\n"
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn a_failed_pass_fires_the_streak_judge_once() {
    let hits = match_event(
        &common::authored_rule(RULE),
        "step.done.task",
        &run_done("maintenance-post-start-schema", "run", failed_pass()),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1, "a failed pass fires exactly once: {hits:?}");
    let inv = &hits[0].invocations;
    assert_eq!(inv.len(), 1, "{inv:?}");
    assert_eq!(inv[0].handler, "maintenance.verdict.streak");
    for (k, want) in [
        ("step", "run"),
        ("field", "result"),
        ("value", "failed"),
        ("evidence", "output"),
        ("route", "post-start-schema/failing"),
        ("design", "f93bdee4"),
        ("area", "infra/postgres"),
    ] {
        assert_eq!(
            arg(&inv[0].args, k),
            Some(&Value::String(want.into())),
            "arg {k}"
        );
    }
    // N against the one-minute tick: one failure is a build that outwaited
    // its deadline and is retried; five in a row is a broken pass.
    assert_eq!(arg(&inv[0].args, "n"), Some(&Value::Int(5)));
}

#[test]
fn an_ok_pass_hands_the_judge_the_run_that_ends_a_streak() {
    let mut pass = failed_pass();
    pass["result"] = serde_json::json!("ok");
    pass["exit_status"] = serde_json::json!("0");
    let hits = match_event(
        &common::authored_rule(RULE),
        "step.done.task",
        &run_done("maintenance-post-start-schema", "run", pass),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1, "an ok pass fires once: {hits:?}");
    assert_eq!(
        arg(&hits[0].invocations[0].args, "value"),
        Some(&Value::String("failed".into())),
        "the same judge, judging the same word"
    );
}

#[test]
fn every_other_step_and_kind_stays_silent() {
    let reg = common::authored_rule(RULE);
    let mut not_yet = failed_pass();
    not_yet["result"] = serde_json::json!("not-yet");
    for (why, kind, slug, meta) in [
        (
            "a pass that could not be judged yet",
            "maintenance-post-start-schema",
            "run",
            not_yet,
        ),
        (
            "another chore's failed run",
            "maintenance-recovery-sheet",
            "run",
            failed_pass(),
        ),
        (
            "another step of the same packet",
            "maintenance-post-start-schema",
            "scheduled",
            failed_pass(),
        ),
    ] {
        let hits = match_event(
            &reg,
            "step.done.task",
            &run_done(kind, slug, meta),
            &NoHelpers,
        )
        .matched;
        assert!(hits.is_empty(), "{why} must fire nothing: {hits:?}");
    }
}
