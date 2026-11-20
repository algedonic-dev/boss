//! The `file-backlog-items-on-boss-gcp-deposit-key-unconfined` rule
//! (`infra/dispatcher/rules/`): a boss-gcp converge whose `run` step
//! records `deposit_key_verdict = "unconfined"` hands that step to the
//! chore judge, which files ONE backlog-item per `RED <route> …` line the
//! converge left in `output` while one is open (backlog dea2236f). A
//! `clean` verdict reaches the judge too, so it can close the items a
//! tick no longer prints (backlog 52534a15).
//!
//! WHY THE TICK DOES NOT RED (review 1fa0d093 of car 3cd92f7a, B2). The
//! car's first shape exited the converge non-zero on a finding, so its
//! run step read `result=exit-code`: landing.rs counts a boss-gcp
//! converge only on `result=ok`, and the publish-drift rule fires only on
//! an ok run that moved. A finding standing in David's authorized_keys
//! held every infra-channel car at "Converging" and lost the moved tick's
//! drift check. So the finding rides an ok run, and this rule is its
//! alarm — pinned here over the shipped rule file, beside the drift rule
//! firing on the same tick.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::match_event;

mod common;

const RULE: &str = "file-backlog-items-on-boss-gcp-deposit-key-unconfined";

/// The `step.done.task` marker as boss-jobs emits it for a maintenance
/// packet's `run` step — the shape publish_drift_rule.rs pins.
fn run_done(
    workflow_kind: &str,
    spec_slug: &str,
    metadata: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "job_id": "c-1", "step_id": "s-run", "kind": "task",
        "subject_kind": "custom", "subject_id": "infra/maintenance-boss-gcp-converge",
        "workflow_kind": workflow_kind,
        "completed_on": "2026-10-10",
        "metadata": metadata,
        "notify_on_done": false,
        "spec_slug": spec_slug
    })
}

/// A tick that MOVED the checkout and found a bare twin, as the
/// converge records it now: result ok, the verdict word, the RED line.
fn finding_tick() -> serde_json::Value {
    serde_json::json!({
        "result": "ok",
        "converge_remote": "forge",
        "converge_from": "afd5756c0000000000000000000000000000aaaa",
        "converge_sha": "cb053ed60000000000000000000000000000bbbb",
        "deposit_key_verdict": "unconfined",
        "deposit_key_audit": "UNCONFINED: deposit-key-audit: 1 finding(s) in /home/david/.ssh/authorized_keys",
        "output": "RED boss-gcp-converge/deposit-key-unconfined refused: deposit-key-audit: UNCONFINED /home/david/.ssh/authorized_keys:1 — …"
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn an_unconfined_verdict_on_the_run_step_fires_the_chore_judge_once() {
    let hits = match_event(
        &common::authored_rule(RULE),
        "step.done.task",
        &run_done("maintenance-boss-gcp-converge", "run", finding_tick()),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1, "a finding tick fires exactly once: {hits:?}");
    let inv = &hits[0].invocations;
    assert_eq!(inv.len(), 1, "{inv:?}");
    assert_eq!(inv[0].handler, "maintenance.chore.file_reds");
    assert_eq!(
        arg(&inv[0].args, "step"),
        Some(&Value::String("run".into()))
    );
    assert_eq!(
        arg(&inv[0].args, "area"),
        Some(&Value::String("credentials".into()))
    );
    // A tick with no RED line files nothing; the fallback route stays the
    // break-glass deposit's alone (chore_file_reds.rs, e4a9a9b3).
    assert_eq!(arg(&inv[0].args, "no_red_failure_route"), None);
    // Every finding prints under one route, so the judge keys each item
    // on its RED line: a second line while the first is open is named
    // (backlog d1b03874).
    assert_eq!(
        arg(&inv[0].args, "one_item_per"),
        Some(&Value::String("finding".into()))
    );
    // The word that says a tick found nothing, so it closes what it no
    // longer prints (backlog 52534a15).
    assert_eq!(
        arg(&inv[0].args, "clean_field"),
        Some(&Value::String("deposit_key_verdict".into()))
    );
    assert_eq!(
        arg(&inv[0].args, "clean_value"),
        Some(&Value::String("clean".into()))
    );
}

/// A CLEAN TICK REACHES THE JUDGE (backlog 52534a15): it is the reading
/// that closes a finding item nobody closed by hand. The judge writes
/// nothing when there is nothing to close.
#[test]
fn a_clean_tick_fires_the_chore_judge_so_it_can_close_what_cleared() {
    let mut clean = finding_tick();
    clean["deposit_key_verdict"] = serde_json::json!("clean");
    clean.as_object_mut().expect("an object").remove("output");
    let hits = match_event(
        &common::authored_rule(RULE),
        "step.done.task",
        &run_done("maintenance-boss-gcp-converge", "run", clean),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(
        hits[0].invocations[0].handler,
        "maintenance.chore.file_reds"
    );
}

#[test]
fn an_unverified_tick_and_every_other_step_stay_silent() {
    let reg = common::authored_rule(RULE);
    let mut unverified = finding_tick();
    unverified["deposit_key_verdict"] = serde_json::json!("unverified");
    let mut before = finding_tick();
    before
        .as_object_mut()
        .expect("an object")
        .remove("deposit_key_verdict");
    for (why, kind, slug, meta) in [
        (
            "an UNVERIFIED audit — no evidence is not a finding either",
            "maintenance-boss-gcp-converge",
            "run",
            unverified,
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
            finding_tick(),
        ),
        (
            "another step of the same packet",
            "maintenance-boss-gcp-converge",
            "scheduled",
            finding_tick(),
        ),
    ] {
        let hits = match_event(
            &reg,
            "step.done.task",
            &run_done(kind, slug, meta),
            &NoHelpers,
        )
        .matched;
        assert!(hits.is_empty(), "{why} must file nothing: {hits:?}");
    }
}

/// THE SAME TICK STILL COUNTS AS CONVERGED (B2): a finding rides an ok
/// run, so the publish-drift rule — which fires only on an ok run that
/// moved — still fires on the tick that carries it.
#[test]
fn a_finding_tick_that_moved_still_fires_the_publish_drift_check() {
    let hits = match_event(
        &common::authored_rule("check-publish-drift-on-boss-gcp-converge-moved"),
        "step.done.task",
        &run_done("maintenance-boss-gcp-converge", "run", finding_tick()),
        &NoHelpers,
    )
    .matched;
    assert_eq!(
        hits.len(),
        1,
        "a moved tick carrying a deposit key finding is still a moved ok run: {hits:?}"
    );
}
