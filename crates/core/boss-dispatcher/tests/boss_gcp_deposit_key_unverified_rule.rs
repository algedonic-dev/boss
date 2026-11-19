//! The `file-backlog-items-on-boss-gcp-deposit-key-unverified` rule
//! (`infra/dispatcher/rules/`): a boss-gcp converge whose `run` step
//! records `deposit_key_verdict = "unverified"` hands that step to the
//! streak judge (`maintenance.verdict.streak`), which files ONE
//! backlog-item when the newest N completed converges ALL read
//! unverified (backlog ada4d019).
//!
//! WHY (backlog ada4d019, review ee6589a9 N1 of car 3cd92f7a). The
//! unconfined rule fires on `unconfined` only and says of itself that an
//! UNVERIFIED audit files nothing; nothing else read the word, so an
//! audit that could not judge the file read `unverified` every half hour,
//! forever, and nobody was told. The rule selects the tick; the COUNT
//! lives in the handler, read back from the converge packets, and is
//! pinned beside it (`handlers/verdict_streak.rs`): it fires on N
//! unverified ticks and stays quiet on N-1 and on any clean tick between.
//! What this file pins is the selection, and the N the rule declares.
//!
//! v2 (backlog 3ff17da3): the rule also selects a CLEAN or UNCONFINED
//! tick — the audit judging the file again — because the run that breaks
//! a streak is what closes its item, and v1 never handed that run to the
//! judge, so an item stayed open across weeks of clean ticks and the next
//! blind streak was folded into it. A tick with no verdict still selects
//! nothing.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::match_event;

mod common;

const RULE: &str = "file-backlog-items-on-boss-gcp-deposit-key-unverified";

/// The `step.done.task` marker as boss-jobs emits it for a maintenance
/// packet's `run` step — the shape boss_gcp_deposit_key_rule.rs pins.
fn run_done(
    workflow_kind: &str,
    spec_slug: &str,
    metadata: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "job_id": "c-1", "step_id": "s-run", "kind": "task",
        "subject_kind": "custom", "subject_id": "infra/maintenance-boss-gcp-converge",
        "workflow_kind": workflow_kind,
        "completed_on": "2026-10-18",
        "metadata": metadata,
        "notify_on_done": false,
        "spec_slug": spec_slug
    })
}

/// A tick whose audit could not reach a verdict, as the converge records
/// it (infra/gcp/boss-gcp-converge.sh, the `*)` arm of the verdict case).
fn unverified_tick() -> serde_json::Value {
    serde_json::json!({
        "result": "ok",
        "deposit_key_verdict": "unverified",
        "deposit_key_audit": "UNVERIFIED: the audit did not reach a verdict (status 124): deposit-key-audit: reading /home/david/.ssh/authorized_keys"
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn an_unverified_verdict_on_the_run_step_fires_the_streak_judge_once() {
    let hits = match_event(
        &common::authored_rule(RULE),
        "step.done.task",
        &run_done("maintenance-boss-gcp-converge", "run", unverified_tick()),
        &NoHelpers,
    )
    .matched;
    assert_eq!(
        hits.len(),
        1,
        "an unverified tick fires exactly once: {hits:?}"
    );
    let inv = &hits[0].invocations;
    assert_eq!(inv.len(), 1, "{inv:?}");
    assert_eq!(inv[0].handler, "maintenance.verdict.streak");
    for (k, want) in [
        ("step", "run"),
        ("field", "deposit_key_verdict"),
        ("value", "unverified"),
        ("evidence", "deposit_key_audit"),
        ("route", "boss-gcp-converge/deposit-key-unverified"),
        ("area", "credentials"),
    ] {
        assert_eq!(
            arg(&inv[0].args, k),
            Some(&Value::String(want.into())),
            "arg {k}"
        );
    }
    // N against the 30-minute tick: three ticks is 60-90 minutes blind,
    // the persistence the estate alarm uses (PERSIST_N) for a level.
    assert_eq!(arg(&inv[0].args, "n"), Some(&Value::Int(3)));
}

#[test]
fn a_clean_or_unconfined_tick_hands_the_judge_the_run_that_ends_a_streak() {
    let reg = common::authored_rule(RULE);
    for word in ["clean", "unconfined"] {
        let mut tick = unverified_tick();
        tick["deposit_key_verdict"] = serde_json::json!(word);
        let hits = match_event(
            &reg,
            "step.done.task",
            &run_done("maintenance-boss-gcp-converge", "run", tick),
            &NoHelpers,
        )
        .matched;
        assert_eq!(hits.len(), 1, "a {word} tick fires once: {hits:?}");
        let inv = &hits[0].invocations;
        assert_eq!(inv[0].handler, "maintenance.verdict.streak");
        assert_eq!(
            arg(&inv[0].args, "value"),
            Some(&Value::String("unverified".into())),
            "the same judge, judging the same word"
        );
    }
}

#[test]
fn a_tick_with_no_verdict_and_every_other_step_stay_silent() {
    let reg = common::authored_rule(RULE);
    let mut before = unverified_tick();
    before
        .as_object_mut()
        .expect("an object")
        .remove("deposit_key_verdict");
    let mut empty = unverified_tick();
    empty["deposit_key_verdict"] = serde_json::json!("");
    for (why, kind, slug, meta) in [
        (
            "a verdict word the audit does not print",
            "maintenance-boss-gcp-converge",
            "run",
            empty,
        ),
        (
            "a tick from before the verdict was recorded",
            "maintenance-boss-gcp-converge",
            "run",
            before,
        ),
        (
            "the forge's converge",
            "maintenance-forge-converge",
            "run",
            unverified_tick(),
        ),
        (
            "another step of the same packet",
            "maintenance-boss-gcp-converge",
            "scheduled",
            unverified_tick(),
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

/// The unconfined rule still files nothing for an unverified tick: no
/// evidence is not a finding. (Since v2 of this rule the streak judge
/// reads the unconfined tick too, as the end of a streak; it writes only
/// to its own route's item and its own note, so the two judges still
/// never file or close each other's items.)
#[test]
fn the_unconfined_rule_does_not_fire_on_the_tick_this_one_reads() {
    let hits = match_event(
        &common::authored_rule("file-backlog-items-on-boss-gcp-deposit-key-unconfined"),
        "step.done.task",
        &run_done("maintenance-boss-gcp-converge", "run", unverified_tick()),
        &NoHelpers,
    )
    .matched;
    assert!(hits.is_empty(), "{hits:?}");
}
