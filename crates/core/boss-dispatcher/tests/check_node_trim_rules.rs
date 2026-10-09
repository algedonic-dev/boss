//! The two rules that make a failed, stuck or missing node trim loud
//! (backlog 33e62de8): `check-node-trim-daily` files the read-only
//! `check-node-trim` for the forge every day, and
//! `watch-check-node-trim-daily` judges its verdict with `ops.judge` in
//! watch mode — an urgent alarm when the newest finished Job failed, the
//! last success is 26 hours old, a success moved no discard counter, the
//! tree declares a CronJob the cluster does not hold, or the check could
//! not read.
//!
//! WHY. CronJob `boss-node-trim` is one attempt a day in a namespace
//! allowed no token, no Role and no second workload, so nothing read its
//! failures (review 8b1981da F1 of the trim car). The script's own verdict
//! line, and what the watch's pattern makes of it, are run over the
//! script's REAL output in
//! `crates/core/boss-testing/tests/node_trim_check_sh.rs`; this file pins
//! the rules' shape and evaluates the predicate with boss-expr itself.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::parse_raw_path;
use boss_dispatcher::rules::schedule_runner::ScheduleRunner;
use chrono::NaiveDate;
use std::collections::HashMap;

mod common;

const RULE: &str = "check-node-trim-daily";
const WATCH: &str = "watch-check-node-trim-daily";
const VERB: &str = "check-node-trim";

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
    let day = NaiveDate::from_ymd_opt(2026, 10, 9).expect("a day");
    let (matched, _) = ScheduleRunner::matched_for_day(&reg, &HashMap::new(), day, &NoHelpers);
    assert_eq!(matched.len(), 1, "the rule fires once a day: {matched:?}");
    let inv = &matched[0].invocations;
    assert_eq!(inv.len(), 1, "one spawn: {inv:?}");
    assert_eq!(inv[0].handler, "jobs.spawn");
    let args = &inv[0].args;
    assert_eq!(text(args, "kind"), Some("ops-request"));
    // The subject is an identity this rule alone files, so the silence
    // sweep derives `ops-request/node-trim`: the reader that stops is
    // itself noticed.
    assert_eq!(text(args, "subject"), Some("node-trim"));
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
        "closed_on": "2026-10-09",
        "kind": "ops-request",
        "outcome": outcome,
        "title": "read whether the build node's daily trim ran, and what it told the drives",
        "subject_id": "node-trim",
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

/// The verdict is the check's own READ line, and `when` holds for a
/// healthy trim under 26 hours old and for a not-yet — nothing else.
#[test]
fn the_watch_alarms_on_a_failed_stale_missing_or_evidence_free_trim() {
    let a = watch_invocations("answered").remove(0);
    let pattern = text(&a, "verdict_pattern").expect("verdict_pattern");
    assert!(!pattern.contains('\\'), "no backslash survives boss-expr");
    assert!(
        pattern.starts_with("^check-node-trim: READ ") && pattern.ends_with('$'),
        "anchored at both ends, in the check's voice: {pattern}"
    );
    let re = regex::Regex::new(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
    let script =
        std::fs::read_to_string(boss_testing::repo_root().join("infra/forge/check-node-trim.sh"))
            .expect("the script");
    assert!(
        script.contains(
            r#"echo "$ME: READ — state $1, missing $missing, hours $2, failed $3, unmoved $4, last_success $5, job $6, outcome $7, reason $8, devices $9, moved ${10}""#
        ),
        "the check's verdict line moved; the watch's pattern reads it"
    );
    let caps = re
        .captures("check-node-trim: READ — state present, missing 0, hours 30, failed 1, unmoved 0, last_success 2026-10-08T12:41:00Z, job boss-node-trim-29332120, outcome failed, reason exit-1, devices 1, moved 1")
        .expect("the verdict matches");
    assert_eq!(
        (
            &caps["state"],
            &caps["hours"],
            &caps["failed"],
            &caps["reason"],
            &caps["last_success"]
        ),
        ("present", "30", "1", "exit-1", "2026-10-08T12:41:00Z")
    );
    assert!(
        re.is_match("check-node-trim: READ — state not-yet, missing 0, hours 0, failed 0, unmoved 0, last_success never, job none, outcome none, reason NotYet, devices 0, moved 0"),
        "the not-yet line is a verdict too"
    );
    for human in [
        "check-node-trim: LAST SUCCESS — 2026-10-08T12:41:00Z, 2 hour(s) ago",
        "check-node-trim: CAUSE — exit-1: pod boss-node-trim-29332120-x7k2p exited 1 (Error)",
        "check-node-trim: FAILED — CANNOT ANSWER — namespaces: Unable to connect to the server",
    ] {
        assert!(
            !re.is_match(human),
            "a human line is not the verdict: {human}"
        );
    }

    let when_src = text(&a, "when").expect("when");
    let when = boss_dispatcher::rules::expr::parse(when_src)
        .unwrap_or_else(|e| panic!("when {when_src:?}: {e}"));
    let holds = |missing: i64, hours: i64, failed: i64, unmoved: i64| {
        boss_dispatcher::rules::expr::eval(
            &when,
            &boss_dispatcher::rules::expr::Context {
                payload: &serde_json::json!({
                    "state": "x", "missing": missing, "hours": hours, "failed": failed,
                    "unmoved": unmoved, "last_success": "t", "job": "j", "outcome": "o",
                    "reason": "r", "devices": 2, "moved": 1
                }),
                helpers: &NoHelpers,
            },
        )
        .unwrap_or_else(|e| panic!("{when_src:?}: {e}"))
    };
    let (ok, alarm) = (Value::Bool(true), Value::Bool(false));
    assert_eq!(holds(0, 2, 0, 0), ok, "a healthy trim is no alarm");
    assert_eq!(holds(0, 0, 0, 0), ok, "a not-yet prints every number 0");
    assert_eq!(holds(0, 25, 0, 0), ok);
    assert_eq!(holds(0, 26, 0, 0), alarm, "26 hours without a success");
    assert_eq!(holds(0, 400, 0, 0), alarm);
    assert_eq!(holds(0, 2, 1, 0), alarm, "the newest finished Job failed");
    assert_eq!(holds(0, 2, 0, 1), alarm, "a success that moved no counter");
    assert_eq!(holds(1, 0, 0, 0), alarm, "declared and not there");
}
