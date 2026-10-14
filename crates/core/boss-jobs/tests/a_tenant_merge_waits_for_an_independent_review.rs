//! ops-request's `review` step, read through the engine's own parser and
//! evaluator (backlog 84e7c3e4, design 9de0cd1c — Q1 answered by David
//! 2026-10-04: "Require an actually closed independent RELEASE ...
//! before the passkey approval becomes ready").
//!
//! The runner half — rendering the plan onto the review step, judging
//! the release, checking it again before the claim — is pinned in
//! boss-testing's `ops_runner_approval_sh.rs`. This half is the
//! protocol's: WHEN the approval may become ready, and that a request
//! whose verb asks for no review is ready exactly as before (David's
//! option A, bd331d38: a hold only where a rule requires one).

use boss_jobs::registry::{StepSpec, WorkflowSpec, seedable_platform_workflows};
use boss_jobs::step_registry::StepRegistry;
use serde_json::{Value, json};

fn ops_request() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "ops-request")
        .expect("the ops-request protocol is in the platform bundle")
}

fn step<'a>(wf: &'a WorkflowSpec, title: &str) -> &'a StepSpec {
    wf.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("ops-request declares a `{title}` step"))
}

fn permits(predicate: &str, context: &Value) -> bool {
    let expression = boss_expr::parse(predicate).unwrap();
    boss_expr::eval(
        &expression,
        &boss_expr::Context {
            payload: context,
            helpers: &boss_expr::NoHelpers,
        },
    )
    .unwrap()
    .as_bool()
    .unwrap()
}

/// The predicate context for a request: its job flags, `filed` done, and
/// the review step's state.
fn request(flags: Value, review_done: bool, verdict: Option<&str>) -> Value {
    let mut review = json!({"done": review_done, "metadata": {}});
    if let Some(v) = verdict {
        review["metadata"]["verdict"] = json!(v);
    }
    json!({
        "job": {"metadata": flags},
        "steps": {
            "filed": {"done": true, "metadata": {}},
            "review": review,
            "approve": {"done": false, "metadata": {}},
            "execute": {"done": false, "metadata": {}}
        }
    })
}

#[test]
fn the_review_is_ready_only_where_the_verb_asks_for_one() {
    let wf = ops_request();
    let review = step(&wf, "review");
    let both = json!({"requires_approval": true, "requires_review": true});
    assert!(permits(&review.ready_when, &request(both, false, None)));
    for flags in [json!({}), json!({"requires_approval": true})] {
        assert!(
            !permits(&review.ready_when, &request(flags.clone(), false, None)),
            "a request whose verb asks for no review never readies one: {flags}"
        );
    }
}

#[test]
fn the_approval_waits_for_a_release_and_only_where_one_is_required() {
    let wf = ops_request();
    let approve = &step(&wf, "approve").ready_when;
    let both = json!({"requires_approval": true, "requires_review": true});
    assert!(!permits(approve, &request(both.clone(), false, None)));
    assert!(!permits(
        approve,
        &request(both.clone(), true, Some("changes"))
    ));
    assert!(!permits(approve, &request(both.clone(), true, None)));
    assert!(permits(approve, &request(both, true, Some("release"))));
    // Every other approval verb: ready on `filed` exactly as before.
    assert!(permits(
        approve,
        &request(json!({"requires_approval": true}), false, None)
    ));
    // A read: never.
    assert!(!permits(approve, &request(json!({}), false, None)));
}

/// The write itself is never readied over an unreleased review, whatever
/// the approve step says — the protocol's half of "again before any
/// push" — and every other request's execute is untouched.
#[test]
fn execute_is_never_ready_over_a_review_that_did_not_release() {
    let wf = ops_request();
    let execute = &step(&wf, "execute").ready_when;
    let approved = |flags: Value, review_done: bool, verdict: Option<&str>| {
        let mut c = request(flags, review_done, verdict);
        c["steps"]["approve"] = json!({"done": true, "metadata": {"decision": "approved"}});
        c
    };
    let both = json!({"requires_approval": true, "requires_review": true});
    assert!(!permits(
        execute,
        &approved(both.clone(), true, Some("changes"))
    ));
    assert!(!permits(execute, &approved(both.clone(), false, None)));
    assert!(permits(execute, &approved(both, true, Some("release"))));
    assert!(permits(
        execute,
        &approved(json!({"requires_approval": true}), false, None)
    ));
    assert!(permits(execute, &request(json!({}), false, None)));
}

/// A REVIEW THAT DOES NOT RELEASE CLOSES THE REQUEST ON ITS OWN TERMINAL,
/// whose title says why (review N4 of car e1f4fc2a, run bd3091d2). It
/// closed through `refused` first, and that step's title is rendered at
/// materialisation — "Refused — outside the allowlist" on every request —
/// so a request its reviewer sent back read as an allowlist refusal.
/// `not-released` is still a refusal (outcome `refused`, kind `aborted`),
/// and `refused` no longer readies on a review, so exactly one terminal is
/// ready on a `changes` verdict.
#[test]
fn a_review_that_does_not_release_closes_the_request_on_a_terminal_that_says_so() {
    let wf = ops_request();
    let not_released = step(&wf, "not-released");
    let refused = &step(&wf, "refused").ready_when;
    let both = json!({"requires_approval": true, "requires_review": true});
    assert!(permits(
        &not_released.ready_when,
        &request(both.clone(), true, Some("changes"))
    ));
    assert!(!permits(
        &not_released.ready_when,
        &request(both.clone(), true, Some("release"))
    ));
    assert!(!permits(
        &not_released.ready_when,
        &request(both.clone(), false, None)
    ));
    // A request whose verb asks for no review: its review step is SKIPPED,
    // which the engine's context reads as not done.
    assert!(!permits(
        &not_released.ready_when,
        &request(json!({"requires_approval": true}), false, None)
    ));
    assert!(
        !permits(refused, &request(both, true, Some("changes"))),
        "`refused` readies on a review too, so two terminals race for one request"
    );
    assert_eq!(
        not_released.terminal.as_ref().map(|t| t.outcome.as_str()),
        Some("refused")
    );
    assert!(
        not_released.title_template.contains("review")
            && not_released.title_template.contains("changes")
            && !not_released.title_template.contains("allowlist"),
        "{}",
        not_released.title_template
    );
}

/// The step completes only with every key the runner renders and every
/// key the reviewer records, and `verdict` takes exactly two words.
#[test]
fn a_review_completes_only_with_the_plan_and_a_two_word_verdict() {
    let wf = ops_request();
    let review = step(&wf, "review");
    let profile = &review
        .agent
        .as_ref()
        .expect("the review step funds an agent")
        .profile;
    assert_eq!(profile, "tenant-reviewer");
    let done = json!({
        "plan": "plan: merge-tenant-main\n",
        "verb": "merge-tenant-main",
        "host": "forge",
        "args": ["prod", "fix/a-branch"],
        "rendered_plan_sha256": "ab".repeat(32),
        "verdict": "release",
        "reviewed_plan_sha256": "ab".repeat(32),
        "source_run": "cccccccc-0000-4000-8000-00000000000c",
        "findings": "read the plan and the branch",
    });
    if let Err(errs) = StepRegistry::validate_authored_fields(&review.fields, &done) {
        panic!("a complete review must satisfy the step's fields: {errs:?}");
    }
    let mut approved = done.clone();
    approved["verdict"] = json!("approved");
    assert!(
        StepRegistry::validate_authored_fields(&review.fields, &approved).is_err(),
        "a verdict outside release|changes is refused"
    );
    let missing: Vec<String> =
        StepRegistry::validate_authored_fields(&review.fields, &json!({"verdict": "release"}))
            .expect_err("a verdict on no plan cannot complete")
            .into_iter()
            .map(|e| e.field)
            .collect();
    for key in [
        "plan",
        "rendered_plan_sha256",
        "reviewed_plan_sha256",
        "source_run",
        "findings",
    ] {
        assert!(
            missing.iter().any(|m| m == key),
            "`{key}` missing is named: {missing:?}"
        );
    }
}
