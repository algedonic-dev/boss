//! The `open-doc-flatten-weekly` rule (backlog ede02a78) — a
//! documentation flatten opens every Monday on the clock, beside the
//! one a cut release opens (`open-doc-flatten-on-release-cut`,
//! 04c6e143), pinned over the shipped rule directory, not a copy.
//!
//! WHY (David, 2026-09-27: "let us have a weekly timer, but we can also
//! do an ad hoc trigger just to test it"). `doc-flatten` had 0 packets
//! ever until the ad hoc run c98e5183 was opened by hand that day, and
//! the release reactor only fires when a release is cut — which, on the
//! day it landed, no release had been. A weekly timer is the cadence
//! that holds with nobody remembering.
//!
//! THE DEDUP IS THE DECISION RECORD. Each weekly flatten is opened
//! ABOUT `docs-architecture-decisions` — the subject the ad hoc run
//! c98e5183 was filed under — and the guard asks whether a flatten of
//! THAT subject is open, so a week with one still open opens no second.
//! The guard does not make the cadence silent when a flatten is left
//! open: the rule's literal `(kind, subject)` and its dedup guard are
//! exactly the shape `cadence_roster::clock_cadences` derives a watched
//! cadence from, so `cadence-silence-sweep-daily` reports the
//! suppression and names the packet holding it (pinned on the handlers
//! side, in `the_silence_roster_covers_the_clock_rules.rs`).

use boss_core::calendar::Cadence;
use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::registry::{MatchedRule, parse_raw_path};
use boss_dispatcher::rules::schedule_runner::ScheduleRunner;
use chrono::{Datelike, NaiveDate, Weekday};
use std::collections::HashMap;

mod common;

const RULE: &str = "open-doc-flatten-weekly";
const SUBJECT: &str = "docs-architecture-decisions";

/// `open_job_exists` answering a fixed value, recording the
/// `(kind, subject)` it was asked about so the dedup KEY is asserted,
/// not only the verdict.
struct StubOpen {
    answer: bool,
    asked: std::sync::Mutex<Vec<(String, String)>>,
}

impl StubOpen {
    fn new(answer: bool) -> Self {
        Self {
            answer,
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn asked_about(&self) -> Vec<(String, String)> {
        self.asked.lock().expect("stub lock").clone()
    }
}

impl HelperResolver for StubOpen {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "open_job_exists" => {
                if let (Some(Value::String(kind)), Some(Value::String(subject))) =
                    (args.first(), args.get(1))
                {
                    self.asked
                        .lock()
                        .expect("stub lock")
                        .push((kind.clone(), subject.clone()));
                }
                Ok(Value::Bool(self.answer))
            }
            other => Err(EvalError::UnknownHelper(other.to_string())),
        }
    }
}

/// A Monday after the anchor.
fn monday() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 5).expect("a day")
}

fn fire(day: NaiveDate, stub: &StubOpen) -> Vec<MatchedRule> {
    let reg = common::authored_rule(RULE);
    ScheduleRunner::matched_for_day(&reg, &HashMap::new(), day, stub).0
}

fn arg(args: &[(String, Value)], k: &str) -> Value {
    args.iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
}

#[test]
fn a_monday_opens_one_doc_flatten_about_the_decision_record() {
    assert_eq!(monday().weekday(), Weekday::Mon);
    let stub = StubOpen::new(false);
    let matched = fire(monday(), &stub);
    assert_eq!(
        matched.len(),
        1,
        "the rule fires once on a Monday: {matched:?}"
    );
    let inv = &matched[0].invocations;
    assert_eq!(inv.len(), 1, "one spawn: {inv:?}");
    assert_eq!(inv[0].handler, "jobs.spawn");
    let args = &inv[0].args;
    assert_eq!(arg(args, "kind"), Value::String("doc-flatten".into()));
    assert_eq!(arg(args, "subject_kind"), Value::String("custom".into()));
    assert_eq!(
        arg(args, "subject"),
        Value::String(SUBJECT.into()),
        "the weekly flatten is about the decision record, the subject the ad hoc run c98e5183 \
         was filed under, so that run counts as this week's"
    );
    assert_eq!(
        stub.asked_about(),
        vec![("doc-flatten".to_string(), SUBJECT.to_string())],
        "the dedup asks about the SAME identity the rule spawns — the shape the silence sweep \
         reads as a watched cadence"
    );
}

/// A week with a flatten still open opens no second one.
#[test]
fn a_flatten_still_open_suppresses_the_week() {
    let matched = fire(monday(), &StubOpen::new(true));
    assert!(
        matched.is_empty(),
        "an open flatten must suppress a twin: {matched:?}"
    );
}

/// Weekly means one day of seven: the other six open nothing.
#[test]
fn no_other_weekday_opens_a_flatten() {
    for offset in 1..7 {
        let day = monday() + chrono::Days::new(offset);
        let matched = fire(day, &StubOpen::new(false));
        assert!(
            matched.is_empty(),
            "{day} ({:?}) must not open a flatten: {matched:?}",
            day.weekday()
        );
    }
}

/// The schedule as authored: weekly, on a Monday anchor. The runner
/// fires day rules at the sim-day boundary, 00:00Z — Sunday 17:00 PDT —
/// so the week's flatten is waiting before Monday morning in Pacific
/// time; the schedule has no finer grain to express 09:00 with.
#[test]
fn the_schedule_is_weekly_on_a_monday() {
    let raw = parse_raw_path(boss_testing::dispatcher_rules_dir())
        .expect("parse the shipped rule directory");
    let rule = raw
        .rules
        .iter()
        .find(|r| r.name == RULE)
        .unwrap_or_else(|| panic!("{RULE} is not in the shipped directory"));
    let schedule = rule.schedule.as_ref().expect("a clock rule");
    assert_eq!(schedule.cadence, Cadence::Weekly);
    assert_eq!(schedule.anchor_date.weekday(), Weekday::Mon);
    assert_eq!(
        rule.when.as_deref(),
        Some(r#"NOT open_job_exists("doc-flatten", "docs-architecture-decisions")"#),
        "the guard must stay the one shape the silence sweep can name a holding packet by"
    );
}

/// The kind and subject kind this rule spawns are the ones the platform
/// bundle declares for `doc-flatten` — read from that file, so a rename
/// on either side is caught at the tree rather than as a refused POST on
/// the first Monday.
#[test]
fn the_spawned_kind_is_the_platform_bundles_doc_flatten() {
    let path = boss_testing::repo_root().join("infra/platform/workflows/doc-flatten.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let doc: toml::Value = toml::from_str(&text).expect("doc-flatten.toml parses");
    let wf = &doc["workflow"][0];
    let subject_kinds: Vec<&str> = wf["subject_kinds"]
        .as_array()
        .expect("subject_kinds is a list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    let matched = fire(monday(), &StubOpen::new(false));
    assert_eq!(matched.len(), 1, "{matched:?}");
    let args = &matched[0].invocations[0].args;
    assert_eq!(
        arg(args, "kind"),
        Value::String(wf["kind"].as_str().unwrap_or_default().into())
    );
    match arg(args, "subject_kind") {
        Value::String(sk) => assert!(
            subject_kinds.contains(&sk.as_str()),
            "doc-flatten admits {subject_kinds:?}, the rule spawns {sk}"
        ),
        other => panic!("subject_kind is not a string: {other:?}"),
    }
}
