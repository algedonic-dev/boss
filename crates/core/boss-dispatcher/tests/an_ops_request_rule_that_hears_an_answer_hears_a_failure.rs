//! Every rule that hears an ops-request close on `answered` also hears
//! it close on `failed` and `indeterminate` (design b7bb1625, backlog
//! 50a1578f).
//!
//! WHY. Until the ops-request row forked on the exit, `answered` meant
//! "the verb ran" whatever it exited, so a rule keyed on
//! `outcome = "answered"` heard every run and its judge
//! (`jobs.complete_linked_step`, `ops.judge`, `maintenance.sweep.judge`)
//! read the exit off the execute record to say whether it failed
//! (`verb_failure`, which car d35db666 put on the one classifier). The
//! row now closes a nonzero exit on `failed` and an unknown one on
//! `indeterminate`. A rule left on `answered` alone would stop hearing
//! exactly the runs its judge exists to fail: the linked step would sit
//! ready behind a closed request forever, with nothing on it saying the
//! verb failed — the silence the item was filed against, moved one hop.
//!
//! So the set of outcomes that mean "the verb ran" is READ OFF THE ROW
//! (every terminal its `ready_when` reaches through an answered
//! execute), never typed here, and every authored rule that matches the
//! closed marker of an `answered` request is held to match each of them
//! (CLAUDE.md §9a: the rule files and the row name one fact).

use std::collections::BTreeSet;

use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::registry::match_event;
use boss_jobs::registry::seedable_platform_workflows;
use serde_json::json;

mod common;

/// The outcomes an ops-request closes on once its verb RAN: the
/// terminals whose predicate reads an answered execute.
fn outcomes_of_a_verb_that_ran() -> BTreeSet<String> {
    let row = seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "ops-request")
        .expect("the ops-request protocol is in the platform bundle");
    row.steps
        .iter()
        .filter(|s| {
            s.ready_when
                .contains("steps.execute.metadata.disposition = \"answered\"")
        })
        .filter_map(|s| s.terminal.as_ref().map(|t| t.outcome.clone()))
        .collect()
}

fn closed(outcome: &str) -> serde_json::Value {
    json!({
        "id": "0b0e0000-0000-4000-8000-000000000001",
        "kind": "ops-request",
        "outcome": outcome,
        "title": "Host read requested: unit-status on forge",
        "subject_id": "forge",
        "closed_on": "2026-11-20",
        "parent_step_id": null,
    })
}

fn rules_hearing(outcome: &str) -> BTreeSet<String> {
    let reg = common::authored_registry();
    let heard = match_event(&reg, "jobs.job.closed", &closed(outcome), &NoHelpers);
    assert!(
        heard.skipped.is_empty(),
        "a rule's predicate failed on an ops-request close ({outcome}): {:?}",
        heard.skipped
    );
    heard.matched.into_iter().map(|m| m.rule_name).collect()
}

/// The row's own reading: a run that exited 0 or 75 closes `answered`,
/// a nonzero one `failed`, an unknown one `indeterminate`.
#[test]
fn the_row_closes_a_verb_that_ran_on_three_outcomes() {
    assert_eq!(
        outcomes_of_a_verb_that_ran(),
        ["answered", "failed", "indeterminate"]
            .map(String::from)
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
}

#[test]
fn a_rule_that_hears_an_answer_hears_every_way_a_verb_that_ran_can_close() {
    let answered = rules_hearing("answered");
    // A control: the judges this car widens are among them, so an empty
    // set (a moved directory, a renamed marker key) cannot pass.
    for rule in [
        "complete-pool-publish-on-publish-pool-answered",
        "complete-protocol-publish-on-publish-workflow-answered",
        "judge-disk-headroom-sweep-on-report-answered",
        "watch-check-node-trim-daily",
    ] {
        assert!(answered.contains(rule), "{rule} hears an answer");
    }
    for outcome in outcomes_of_a_verb_that_ran() {
        let heard = rules_hearing(&outcome);
        let deaf: Vec<&String> = answered.difference(&heard).collect();
        assert!(
            deaf.is_empty(),
            "these rules hear an ops-request close `answered` and not `{outcome}`, so a \
             verb that ran and closed `{outcome}` would never reach their judge: {deaf:?}"
        );
    }
}

/// The widening is not a wildcard: a request that never ran (refused,
/// not released, nothing to do) is heard only by the rules that already
/// asked for it.
#[test]
fn a_rule_on_answered_alone_still_does_not_hear_a_refusal() {
    let refused = rules_hearing("refused");
    for rule in [
        "complete-pool-publish-on-publish-pool-answered",
        "judge-disk-headroom-sweep-on-report-answered",
    ] {
        assert!(!refused.contains(rule), "{rule} must not hear a refusal");
    }
    assert!(refused.contains("watch-check-node-trim-daily"));
}
