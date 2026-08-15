//! The feedback obligation, at the layer it is implemented in: two
//! rule rows over generic handlers (job 2c4ae549, migration 117).
//!
//! Everything here loads the registry the way the service does —
//! `load_active_rules` against the seeded `dispatcher_rules` table,
//! then `Registry::from_raw` — and drives `match_event` → `dispatch`
//! with `RecordingHandler` standing in for the HTTP handlers. So the
//! assertions are about what the SHIPPED rows do, not about a fixture
//! copy of them. (`dispatcher_rules_seed_matches_toml` separately pins
//! the seeded table against `infra/dispatcher/rules.toml`.)
//!
//! Three things are worth a test at this layer, and none of them are
//! visible from a handler unit test:
//!
//! 1. The rules SELECT correctly off the close marker — a merged car
//!    fires the completion, an abandoned one does not, a feedback
//!    close fires the notification and not the completion.
//! 2. A close marker with null `kind` / `outcome` (the catch-all
//!    close's shape) evaluates to a clean false. The expr binder makes
//!    an ABSENT identifier a `PredicateFailed`, which the runner NAKs
//!    and eventually dead-letters — so "present and null" versus
//!    "missing" is the difference between a rule that skips and a
//!    retry storm on every unrelated Job close in the system.
//! 3. The `steps` arg names real branches of the LIVE `user-feedback`
//!    Workflow. The rule row is data and the Workflow is data, and
//!    nothing but a test connects them: rename a branch and the rule
//!    would silently complete nothing, which is precisely the failure
//!    mode this whole car exists to remove.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::handler::{HandlerRegistry, RecordingHandler, dispatch};
use boss_dispatcher::rules::registry::{MatchedRule, Registry, load_active_rules, match_event};
use boss_jobs::registry::feedback_branch_for_disposition;
use boss_testing::TestDb;
use serde_json::json;

const COMPLETE_RULE: &str = "complete-feedback-branch-on-car-merged";
const NOTIFY_RULE: &str = "notify-filer-on-feedback-terminal";

/// Load the shipped registry through the production path.
async fn shipped_registry(db: &TestDb) -> Registry {
    let raw = load_active_rules(&db.pool)
        .await
        .expect("load active rules from dispatcher_rules");
    Registry::from_raw(raw).expect("the shipped rows parse")
}

/// A `jobs.job.closed` marker in the shape all three emit sites
/// produce: every key present, null where there is no answer.
fn close_marker(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "closed_on": "2026-08-13",
        "kind": kind,
        "outcome": outcome,
        "parent_step_id": null,
    })
}

fn matched_named<'a>(matched: &'a [MatchedRule], name: &str) -> Option<&'a MatchedRule> {
    matched.iter().find(|m| m.rule_name == name)
}

fn arg_of(m: &MatchedRule, handler: &str, arg: &str) -> String {
    let inv = m
        .invocations
        .iter()
        .find(|i| i.handler == handler)
        .unwrap_or_else(|| panic!("rule {} does not invoke {handler}", m.rule_name));
    match inv.args.iter().find(|(k, _)| k == arg) {
        Some((_, Value::String(s))) => s.clone(),
        other => panic!("arg {arg:?} on {handler} is {other:?}, expected a string"),
    }
}

/// The obligation's trigger: a merged car, and the completion fires
/// with the edge and the branch list resolved.
#[tokio::test(flavor = "multi_thread")]
async fn a_merged_car_fires_the_completion_with_its_edge_and_branches() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;
    let payload = close_marker("ship-a-change", json!("merged"));

    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoHelpers)
        .expect("the shipped predicates evaluate against a close marker");
    let m = matched_named(&matched, COMPLETE_RULE)
        .unwrap_or_else(|| panic!("{COMPLETE_RULE} did not match a merged car: {matched:?}"));

    assert_eq!(
        arg_of(m, "jobs.complete_linked_step", "link"),
        "backlog_item",
        "the rule must follow the DECLARED job edge, not a prose field"
    );
    assert_eq!(
        arg_of(m, "jobs.complete_linked_step", "steps"),
        "investigate,design-review,build"
    );

    // …and it reaches the handler through the real dispatch loop.
    // The registry is DERIVED from the rules that actually matched,
    // not hand-listed. `dispatch` refuses an unknown handler, so a
    // hand-list is a second roster of handler names that silently
    // rots: on 2026-08-14 a new rule on this very topic
    // (expire-signals-on-job-closed, migration 128) turned a train red
    // by breaking this test, which is about a DIFFERENT rule. That
    // list was never the subject of the test — it was scaffolding that
    // could fail.
    //
    // Deriving it keeps the assertion honest in the direction that
    // matters: this test says the completion fires ONCE, not that it
    // is the only thing firing. Another rule joining this topic is
    // someone else's business.
    let handler = RecordingHandler::new("jobs.complete_linked_step");
    let mut hreg = HandlerRegistry::new();
    hreg.register(handler.clone());
    // `RecordingHandler::new` wants a &'static str; the handler names
    // come from the parsed rules, so leak them. A test process is the
    // one place that is free.
    for m in &matched {
        for inv in &m.invocations {
            if inv.handler != "jobs.complete_linked_step" {
                let name: &'static str = Box::leak(inv.handler.clone().into_boxed_str());
                hreg.register(RecordingHandler::new(name));
            }
        }
    }
    let results = dispatch(&matched, &hreg, "evt-1", "jobs.job.closed", &payload)
        .await
        .expect("every named handler is registered");
    assert!(
        results.iter().all(|r| r.outcome.is_ok()),
        "dispatch reported a failure: {results:?}"
    );
    assert_eq!(handler.calls().await.len(), 1, "the completion fired once");
}

/// An abandoned car answers nothing. The packet stays open for
/// whoever picks the work back up.
#[tokio::test(flavor = "multi_thread")]
async fn an_abandoned_car_completes_nothing() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;
    let payload = close_marker("ship-a-change", json!("abandoned"));
    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoHelpers).expect("evaluates");
    assert!(
        matched_named(&matched, COMPLETE_RULE).is_none(),
        "an abandoned change must not close the packet that authorized it"
    );
}

/// A feedback packet closing notifies its filer, and does NOT feed
/// itself back into the completion rule.
#[tokio::test(flavor = "multi_thread")]
async fn a_feedback_terminal_fires_the_notification_only() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;

    // Every terminal a packet can reach, including the ones triage
    // closes outright.
    for outcome in ["completed", "duplicate", "declined"] {
        let payload = close_marker("user-feedback", json!(outcome));
        let matched =
            match_event(&reg, "jobs.job.closed", &payload, &NoHelpers).expect("evaluates");
        let m = matched_named(&matched, NOTIFY_RULE).unwrap_or_else(|| {
            panic!("{NOTIFY_RULE} did not match a `{outcome}` terminal: {matched:?}")
        });
        assert_eq!(
            arg_of(m, "messages.notify_job_terminal", "recipient_key"),
            "submitted_by",
            "the FILER is the recipient the rule names"
        );
        assert!(
            matched_named(&matched, COMPLETE_RULE).is_none(),
            "a feedback close must not run the completion rule"
        );
    }
}

/// THE regression this shape exists to prevent. A catch-all close
/// carries `kind` and `outcome` as present-but-null; a rule binding
/// them must evaluate to false, not blow up. If `match_event` errors
/// here, the runner NAKs — and it NAKs for every Job close in the
/// system, not just the ones these rules care about.
#[tokio::test(flavor = "multi_thread")]
async fn a_close_marker_with_no_outcome_evaluates_to_a_clean_false() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;

    for payload in [
        close_marker("closes-by-catch-all", json!(null)),
        close_marker("ship-a-change", json!(null)),
        close_marker("pr-train", json!("arrived")),
    ] {
        let matched =
            match_event(&reg, "jobs.job.closed", &payload, &NoHelpers).unwrap_or_else(|e| {
                panic!(
                    "a shipped predicate failed on a legitimate close marker ({e}) — \
                     that is a dead-letter storm, not a skipped rule. Payload: {payload:#}"
                )
            });
        assert!(matched_named(&matched, COMPLETE_RULE).is_none());
        assert!(matched_named(&matched, NOTIFY_RULE).is_none());
    }
}

/// The rule row is data and the Workflow is data; only a test
/// connects them. Every slug the rule names must be a real,
/// non-terminal `user-feedback` branch — rename one in the Workflow
/// and this fails loudly instead of the rule silently completing
/// nothing, which IS the failure mode this car exists to remove.
#[tokio::test(flavor = "multi_thread")]
async fn every_branch_the_rule_names_is_a_live_non_terminal_feedback_branch() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;
    let payload = close_marker("ship-a-change", json!("merged"));
    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoHelpers).expect("evaluates");
    let m = matched_named(&matched, COMPLETE_RULE).expect("the completion rule matched");
    let listed: Vec<String> = arg_of(m, "jobs.complete_linked_step", "steps")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(!listed.is_empty(), "the rule names no branches at all");

    // Every branch a disposition opens, from the shipped Workflow.
    let all_branches: Vec<(String, bool)> = [
        "reproduce",
        "design",
        "build",
        "needs-info",
        "duplicate",
        "decline",
    ]
    .iter()
    .filter_map(|d| feedback_branch_for_disposition(d))
    .map(|b| (b.slug, b.terminal))
    .collect();

    for slug in &listed {
        let branch = all_branches
            .iter()
            .find(|(s, _)| s == slug)
            .unwrap_or_else(|| {
                panic!(
                    "the rule names branch `{slug}`, which no `user-feedback` disposition \
                     opens. Live branches: {all_branches:?}"
                )
            });
        assert!(
            !branch.1,
            "`{slug}` is a DECLARED TERMINAL — triage closes the packet outright there, \
             so there is never an open step for a merged car to complete"
        );
    }

    // And the other direction: a branch added to the Workflow must be
    // a decision someone makes, not an omission nobody notices.
    // `needs-info` is deliberately excluded — a change landing does
    // not answer a question asked of the reporter.
    let mut expected: Vec<String> = all_branches
        .iter()
        .filter(|(_, terminal)| !*terminal)
        .map(|(s, _)| s.clone())
        .collect();
    expected.sort();
    let mut covered: Vec<String> = listed.clone();
    covered.push("needs-info".to_string());
    covered.sort();
    covered.dedup();
    assert_eq!(
        covered, expected,
        "the `user-feedback` fork grew (or shrank) a non-terminal branch. Decide \
         whether a shipped change satisfies it: add it to the rule row's `steps` \
         arg, or add it to this test's deliberate-exclusion list alongside \
         `needs-info`."
    );
}
