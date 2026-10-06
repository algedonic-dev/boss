//! The two rules that keep a relaxed Longhorn node-drain-policy from
//! being forgotten (backlog 46584350): `check-longhorn-drain-policy-daily`
//! files the read-only `check-longhorn-drain-policy` for the forge every
//! day, and `watch-check-longhorn-drain-policy-daily` judges its verdict
//! with `ops.judge` in watch mode — an urgent alarm once a non-default
//! value has stood 24 hours, or when the check could not read.
//!
//! WHY. set-longhorn-drain-policy relaxes the policy for the w-1 NVMe
//! window of 2026-09-30 (backlog 52ea56ac) and is meant to set it back
//! after w-1 boots. Left relaxed, the next drain of w-1 passes the only
//! healthy replica of /work and gate-runner-disk without anyone deciding
//! so. The script's own verdict line, and what the watch's `when` makes
//! of it, are run over the script's REAL output in
//! `crates/core/boss-testing/tests/longhorn_drain_policy_sh.rs`; this file
//! pins the rules' shape and the predicate.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::parse_raw_path;
use boss_dispatcher::rules::schedule_runner::ScheduleRunner;
use chrono::NaiveDate;
use std::collections::HashMap;

mod common;

const RULE: &str = "check-longhorn-drain-policy-daily";
const WATCH: &str = "watch-check-longhorn-drain-policy-daily";
const VERB: &str = "check-longhorn-drain-policy";

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

fn text<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a str> {
    match arg(args, k) {
        Some(Value::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn verb_file(name: &str) -> serde_json::Value {
    let path = boss_testing::repo_root().join(format!("infra/ops/verbs/{name}.json"));
    serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("{} is JSON: {e}", path.display()))
}

#[test]
fn the_day_files_one_read_only_check_for_the_forge() {
    let reg = common::authored_rule(RULE);
    let day = NaiveDate::from_ymd_opt(2026, 10, 1).expect("a day");
    let (matched, _) = ScheduleRunner::matched_for_day(&reg, &HashMap::new(), day, &NoHelpers);
    assert_eq!(matched.len(), 1, "the rule fires once a day: {matched:?}");
    let inv = &matched[0].invocations;
    assert_eq!(inv.len(), 1, "one spawn: {inv:?}");
    assert_eq!(inv[0].handler, "jobs.spawn");
    let args = &inv[0].args;
    assert_eq!(text(args, "kind"), Some("ops-request"));
    // The subject is the SETTING, an identity this rule alone files, so
    // the silence sweep derives `ops-request/longhorn-drain-policy`.
    assert_eq!(text(args, "subject"), Some("longhorn-drain-policy"));
    let vals: Vec<&Value> = args.iter().map(|(_, v)| v).collect();
    for want in [VERB, "forge"] {
        assert!(
            vals.contains(&&Value::String(want.into())),
            "{want} rides the spawn: {args:?}"
        );
    }
    // The verb a clock files is READ-ONLY: no rule may file a write that
    // no person filed (infra/ops/verbs/README.md), and a request waiting
    // on a passkey would leave the ops-queue watch.
    let v = verb_file(VERB);
    assert!(
        !v["about"].as_str().unwrap_or_default().contains("MUTATING"),
        "{v}"
    );
    assert!(v.get("requires_approval").is_none(), "{v}");
    assert_eq!(v["params"], serde_json::json!([]), "nothing to supply");
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
}

/// No `when`: the check only reads, and a `NOT open_*` guard would let one
/// unfinished request retire the cadence.
#[test]
fn the_check_carries_no_dedup_guard() {
    let raw = parse_raw_path(boss_testing::dispatcher_rules_dir())
        .expect("parse the shipped rule directory");
    let rule = raw
        .rules
        .iter()
        .find(|r| r.name == RULE)
        .unwrap_or_else(|| panic!("{RULE} is not in the shipped directory"));
    assert_eq!(rule.when, None, "`{RULE}` acquired a `when`");
}

fn watch_invocations(outcome: &str) -> Vec<Vec<(String, Value)>> {
    let reg = common::authored_rule(WATCH);
    let closed = serde_json::json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "closed_on": "2026-10-02",
        "kind": "ops-request",
        "outcome": outcome,
        "title": "read how long Longhorn's node-drain-policy has stood non-default",
        "subject_id": "longhorn-drain-policy",
        "parent_step_id": null,
    });
    boss_dispatcher::rules::registry::match_event(&reg, "jobs.job.closed", &closed, &NoHelpers)
        .matched
        .iter()
        .flat_map(|h| h.invocations.iter())
        .filter(|i| i.handler == "ops.judge")
        .map(|i| i.args.clone())
        .collect()
}

/// Every close reaches the watch — `answered` (the verb ran, whatever its
/// exit) and `refused` — and it is a WATCH: no follow-on is filed.
#[test]
fn the_watch_reads_every_close_of_the_check_and_files_no_follow_on() {
    for outcome in ["answered", "refused"] {
        let inv = watch_invocations(outcome);
        assert_eq!(inv.len(), 1, "{outcome}: one ops.judge do: {inv:?}");
        assert_eq!(text(&inv[0], "verb"), Some(VERB));
        for k in ["then_verb", "then_host", "then_args"] {
            assert!(arg(&inv[0], k).is_none(), "a watch names no {k}");
        }
    }
    assert!(watch_invocations("cancelled").is_empty());
}

/// The verdict is the check's own READ line, and `when` holds for the
/// default and for a value younger than 24 hours — nothing else.
#[test]
fn the_watch_alarms_on_a_policy_non_default_for_24_hours() {
    let a = watch_invocations("answered").remove(0);
    let pattern = text(&a, "verdict_pattern").expect("verdict_pattern");
    assert!(!pattern.contains('\\'), "no backslash survives boss-expr");
    assert!(
        pattern.starts_with("^check-longhorn-drain-policy: READ "),
        "anchored at the line's start, in the check's voice: {pattern}"
    );
    let re = regex::Regex::new(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
    let script = std::fs::read_to_string(
        boss_testing::repo_root().join("infra/forge/longhorn-drain-policy.sh"),
    )
    .expect("the script");
    assert!(
        script.contains(
            r#"echo "$ME: READ — node-drain-policy $CUR, default $DEF, non_default 1, hours $hours""#
        ) && script.contains(
            r#"echo "$ME: READ — node-drain-policy $CUR, default $DEF, non_default 0, hours 0""#
        ),
        "the check's verdict lines moved; the watch's pattern reads them"
    );
    let caps = re
        .captures("check-longhorn-drain-policy: READ — node-drain-policy allow-if-replica-is-stopped, default block-if-contains-last-replica, non_default 1, hours 30")
        .expect("the verdict matches");
    assert_eq!(
        (&caps["value"], &caps["non_default"], &caps["hours"]),
        ("allow-if-replica-is-stopped", "1", "30")
    );
    assert!(
        !re.is_match("check-longhorn-drain-policy: NON-DEFAULT — node-drain-policy has been allow-if-replica-is-stopped since 2026-09-30T15:00:00Z (30 hour(s))"),
        "the human line is not the verdict"
    );

    let when_src = text(&a, "when").expect("when");
    let when = boss_dispatcher::rules::expr::parse(when_src)
        .unwrap_or_else(|e| panic!("when {when_src:?}: {e}"));
    let holds = |non_default: i64, hours: i64| {
        boss_dispatcher::rules::expr::eval(
            &when,
            &boss_dispatcher::rules::expr::Context {
                payload: &serde_json::json!({
                    "value": "x", "default": "y", "non_default": non_default, "hours": hours
                }),
                helpers: &NoHelpers,
            },
        )
        .unwrap_or_else(|e| panic!("{when_src:?}: {e}"))
    };
    assert_eq!(holds(0, 0), Value::Bool(true), "the default is no alarm");
    assert_eq!(holds(1, 0), Value::Bool(true), "the window itself");
    assert_eq!(holds(1, 23), Value::Bool(true));
    assert_eq!(
        holds(1, 24),
        Value::Bool(false),
        "a day relaxed is the alarm"
    );
    assert_eq!(holds(1, 400), Value::Bool(false));
}
