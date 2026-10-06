//! The `department-retros-weekly` rule (design 3613f0af, backlog
//! 1dffde5d), pinned over the shipped rule directory the way every
//! other clock rule is (`doc_flatten_weekly_rule.rs`,
//! `prune_registry_versions_daily_rule.rs`): the file the dispatcher
//! boots from, run through `ScheduleRunner::matched_for_day`, never a
//! copy of it.
//!
//! WHY IT IS PINNED NOW (backlog a4fda30b, decided 2026-09-25; backlog
//! da600d10). Its first firing, W39, opened a retro for all thirteen
//! departments the registry then held; ten declared no protocol, and
//! seven of those were later retired, leaving seven open packets nobody
//! could work. The handler's half — read the roster at fire time, open
//! only for a department whose readiness reads `protocols.has = true`,
//! record each skip on the platform retro — is unit-tested beside
//! `retro_open.rs`. This half pins what the RULE hands it: the day, the
//! handler, and the args that turn the protocol filter on — for every
//! department alike, IT included since a8458043 gave it protocols
//! (backlog ab6861a1 deleted its exemption).

use boss_core::calendar::Cadence;
use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{MatchedRule, parse_raw_path};
use boss_dispatcher::rules::schedule_runner::ScheduleRunner;
use chrono::{Datelike, NaiveDate, Weekday};
use std::collections::HashMap;

mod common;

const RULE: &str = "department-retros-weekly";

/// The first Monday after this car: W40, the firing that would have
/// reopened the dead retros.
fn monday() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 28).expect("a day")
}

fn fire(day: NaiveDate) -> Vec<MatchedRule> {
    let reg = common::authored_rule(RULE);
    ScheduleRunner::matched_for_day(&reg, &HashMap::new(), day, &NoHelpers).0
}

fn arg(args: &[(String, Value)], k: &str) -> Value {
    args.iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
}

#[test]
fn a_monday_runs_retro_open_once_with_the_protocol_filters_args() {
    assert_eq!(monday().weekday(), Weekday::Mon);
    let matched = fire(monday());
    assert_eq!(
        matched.len(),
        1,
        "the rule fires once on a Monday: {matched:?}"
    );
    let inv = &matched[0].invocations;
    assert_eq!(inv.len(), 1, "one handler: {inv:?}");
    assert_eq!(inv[0].handler, "retro.open");
    let args = &inv[0].args;
    assert_eq!(
        arg(args, "department_kind"),
        Value::String("department-retro".into())
    );
    assert_eq!(
        arg(args, "platform_kind"),
        Value::String("protocol-retro".into()),
        "the platform retro rides the same firing and carries the skip list"
    );
    assert_eq!(
        arg(args, "platform_subject"),
        Value::String("infra/protocol-retro".into())
    );
    // IT opens like every other department (backlog ab6861a1,
    // 2026-10-01): a8458043 stamped IT onto its 61 platform workflows,
    // so its readiness reads `protocols.has = true` and the exemption
    // that kept the filter from dropping it names a state that is gone.
    assert!(
        args.iter().all(|(n, _)| n != "open_without_protocol"),
        "no department is exempt from the protocol filter: {args:?}"
    );
    assert_eq!(args.len(), 3, "exactly the three retro args: {args:?}");
}

/// Weekly means one day of seven: the other six open nothing.
#[test]
fn no_other_weekday_opens_a_retro() {
    for offset in 1..7 {
        let day = monday() + chrono::Days::new(offset);
        let matched = fire(day);
        assert!(
            matched.is_empty(),
            "{day} ({:?}) must not open retros: {matched:?}",
            day.weekday()
        );
    }
}

/// Weekly on a Monday anchor, and NO `when`: an open-packet guard
/// retires a cadence the first time one retro is left open
/// (a-dedup-guard-silently-retires-a-cadence); the handler's week
/// window is the dedup.
#[test]
fn the_schedule_is_weekly_on_a_monday_with_no_guard() {
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
    assert_eq!(rule.when, None, "the dedup is the handler's week window");
}

/// Both kinds the rule opens are the platform bundle's — read from
/// those files, so a rename on either side is caught at the tree rather
/// than as a refused POST on a Monday.
#[test]
fn the_opened_kinds_are_the_platform_bundles_retros() {
    let matched = fire(monday());
    assert_eq!(matched.len(), 1, "{matched:?}");
    let args = &matched[0].invocations[0].args;
    for (key, file) in [
        ("department_kind", "department-retro.toml"),
        ("platform_kind", "protocol-retro.toml"),
    ] {
        let path = boss_testing::repo_root()
            .join("infra/platform/workflows")
            .join(file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let doc: toml::Value = toml::from_str(&text).expect("the workflow file parses");
        let kind = doc["workflow"][0]["kind"].as_str().unwrap_or_default();
        assert_eq!(
            arg(args, key),
            Value::String(kind.into()),
            "{key} vs {file}"
        );
    }
}
