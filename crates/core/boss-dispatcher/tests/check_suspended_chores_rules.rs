//! The two rules that keep a suspended cluster chore from being forgotten
//! (backlog 87e08026, car 2 of d28c0051): `check-suspended-chores-daily`
//! files the read-only `check-suspended-chores` for the forge every day,
//! and `watch-check-suspended-chores-daily` judges its verdict with
//! `ops.judge` in watch mode — one urgent alarm for a pass whose oldest
//! suspension has stood 24 hours, or when the check could not read.
//!
//! WHY. The converge's client-side apply does not own spec.suspend, so a
//! chore suspended by hand stays suspended across every apply, and
//! check-manifests-applied only asks that the CronJob exists:
//! boss/boss-search-reindex, suspended at 2026-11-11 05:02Z, was reported
//! by nothing. The script's own verdict line, and what the watch's `when`
//! makes of it, are run over the script's REAL output in
//! `crates/core/boss-testing/tests/check_suspended_chores_sh.rs`; this
//! file pins the rules' shape and the predicate.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::parse_raw_path;
use boss_dispatcher::rules::schedule_runner::ScheduleRunner;
use chrono::NaiveDate;
use std::collections::HashMap;

mod common;

const RULE: &str = "check-suspended-chores-daily";
const WATCH: &str = "watch-check-suspended-chores-daily";
const VERB: &str = "check-suspended-chores";

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
    let day = NaiveDate::from_ymd_opt(2026, 11, 12).expect("a day");
    let (matched, _) = ScheduleRunner::matched_for_day(&reg, &HashMap::new(), day, &NoHelpers);
    assert_eq!(matched.len(), 1, "the rule fires once a day: {matched:?}");
    let inv = &matched[0].invocations;
    assert_eq!(inv.len(), 1, "one spawn: {inv:?}");
    assert_eq!(inv[0].handler, "jobs.spawn");
    let args = &inv[0].args;
    assert_eq!(text(args, "kind"), Some("ops-request"));
    // The subject is an identity this rule alone files, so the silence
    // sweep derives `ops-request/suspended-chores`.
    assert_eq!(text(args, "subject"), Some("suspended-chores"));
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
    assert_eq!(
        v["argv"],
        serde_json::json!(["infra/forge/check-suspended-chores.sh"])
    );
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
        "closed_on": "2026-11-12",
        "kind": "ops-request",
        "outcome": outcome,
        "title": "read which cluster chores stand suspended, and since when",
        "subject_id": "suspended-chores",
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

/// The verdict is the check's own READ line, and `when` holds for no
/// suspension and for one younger than 24 hours — nothing else.
#[test]
fn the_watch_alarms_on_a_chore_suspended_for_24_hours() {
    let a = watch_invocations("answered").remove(0);
    let pattern = text(&a, "verdict_pattern").expect("verdict_pattern");
    assert!(!pattern.contains('\\'), "no backslash survives boss-expr");
    assert!(
        pattern.starts_with("^check-suspended-chores: READ "),
        "anchored at the line's start, in the check's voice: {pattern}"
    );
    let re = regex::Regex::new(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
    let script = std::fs::read_to_string(
        boss_testing::repo_root().join("infra/forge/check-suspended-chores.sh"),
    )
    .expect("the script");
    assert!(
        script.contains(
            r#"echo "$ME: READ — suspended $n, oldest_hours $oldest, chores ${names:-none}""#
        ),
        "the check's verdict line moved; the watch's pattern reads it"
    );
    let caps = re
        .captures("check-suspended-chores: READ — suspended 2, oldest_hours 30, chores boss/boss-search-reindex,boss/boss-files-gc")
        .expect("the verdict matches");
    assert_eq!(
        (&caps["suspended"], &caps["oldest_hours"], &caps["chores"]),
        ("2", "30", "boss/boss-search-reindex,boss/boss-files-gc")
    );
    assert!(
        re.is_match("check-suspended-chores: READ — suspended 0, oldest_hours 0, chores none"),
        "nothing suspended is still a verdict"
    );
    assert!(
        !re.is_match("check-suspended-chores: SUSPENDED — boss/boss-search-reindex since 2026-11-11T05:02:13Z (4 hour(s))"),
        "the human line is not the verdict"
    );

    let when_src = text(&a, "when").expect("when");
    let when = boss_dispatcher::rules::expr::parse(when_src)
        .unwrap_or_else(|e| panic!("when {when_src:?}: {e}"));
    let holds = |suspended: i64, oldest_hours: i64| {
        boss_dispatcher::rules::expr::eval(
            &when,
            &boss_dispatcher::rules::expr::Context {
                payload: &serde_json::json!({
                    "suspended": suspended, "oldest_hours": oldest_hours, "chores": "x"
                }),
                helpers: &NoHelpers,
            },
        )
        .unwrap_or_else(|e| panic!("{when_src:?}: {e}"))
    };
    assert_eq!(holds(0, 0), Value::Bool(true), "nothing suspended");
    assert_eq!(holds(1, 0), Value::Bool(true), "a fresh suspension");
    assert_eq!(holds(3, 23), Value::Bool(true));
    assert_eq!(
        holds(1, 24),
        Value::Bool(false),
        "a day suspended is the alarm"
    );
    assert_eq!(holds(2, 400), Value::Bool(false));
}
