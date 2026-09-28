//! The `prune-registry-versions-daily` rule (`infra/dispatcher/rules/`):
//! every day, file one `prune-registry-versions-daily` ops-request for
//! the forge, so the registry's growth has a bound that holds with
//! nobody present.
//!
//! WHY IT EXISTS (backlog 8d77d670, design 97add747, Q1 authorised by
//! David 2026-09-27). `/opt/forgejo/data` was 17 GB after the
//! 2026-09-18 prune and 92 GB on 2026-09-26 — about 9 GB a day — and
//! the only thing that shrinks it, `prune-registry-versions`, ran only
//! when someone filed it by hand. disk-floor-sweep ended FLOOR UNMET
//! every hour from 2026-09-24 12:01Z (estate alarm c4da71b0).
//!
//! WHAT IS PINNED HERE. The rule fires daily with no `when` (the
//! idempotence is in the script's derived keep set, and a dedup guard is
//! `a-dedup-guard-silently-retires-a-cadence`), and it files a verb whose
//! argv FIXES `--for-real` on the same script the hand verb runs, with
//! no packet-supplied input: the rule sets no `metadata.args` (a
//! `jobs.spawn` CAN carry a list since 4d53fae2; this one deliberately
//! does not), so the verb's own argv is the only thing that selects the
//! mode, and the keep count is the script's one default rather than a
//! second number here. The
//! verb file's authorisation text is pinned beside its hand-filed twin,
//! in `crates/core/boss-testing/tests/prune_registry_versions_sh.rs`.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::parse_raw_path;
use boss_dispatcher::rules::schedule_runner::ScheduleRunner;
use chrono::NaiveDate;
use std::collections::HashMap;

mod common;

const RULE: &str = "prune-registry-versions-daily";
const VERB: &str = "prune-registry-versions-daily";

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 28).expect("a day")
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

fn verb_file(name: &str) -> serde_json::Value {
    let path = boss_testing::repo_root().join(format!("infra/ops/verbs/{name}.json"));
    serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("{} is JSON: {e}", path.display()))
}

#[test]
fn the_day_files_one_registry_prune_for_the_forge() {
    let reg = common::authored_rule(RULE);
    let (matched, _) = ScheduleRunner::matched_for_day(&reg, &HashMap::new(), day(), &NoHelpers);
    assert_eq!(matched.len(), 1, "the rule fires once a day: {matched:?}");
    let inv = &matched[0].invocations;
    assert_eq!(inv.len(), 1, "one spawn: {inv:?}");
    assert_eq!(inv[0].handler, "jobs.spawn");
    let args = &inv[0].args;
    assert_eq!(
        arg(args, "kind"),
        Some(&Value::String("ops-request".into()))
    );
    // The subject is the REGISTRY, an identity this rule alone files, so
    // the silence sweep derives `ops-request/forge-registry` rather than
    // a cadence hidden in the forge's ordinary ops traffic.
    assert_eq!(
        arg(args, "subject"),
        Some(&Value::String("forge-registry".into()))
    );
    // host + verb ride as flattened metadata.* args; asserted BY VALUE so
    // the check never depends on how the loader renders a dotted key.
    let vals: Vec<&Value> = args.iter().map(|(_, v)| v).collect();
    assert!(
        vals.contains(&&Value::String(VERB.into())),
        "the daily verb rides the spawn: {args:?}"
    );
    assert!(
        vals.contains(&&Value::String("forge".into())),
        "the host the ops-runner routes on rides the spawn: {args:?}"
    );
    // Never the hand verb: it has no default mode, so a rule filing it
    // is refused by the runner for a missing argument every day.
    assert!(
        !vals.contains(&&Value::String("prune-registry-versions".into())),
        "the rule must file the fixed-argv verb, not the hand verb: {args:?}"
    );
    assert!(
        args.iter().all(|(n, _)| !n.contains("args")),
        "the rule must not set metadata.args — the verb's argv fixes the mode: {args:?}"
    );
}

/// No `when`: the script is idempotent (a second run finds nothing
/// outside the keep set), and a `NOT open_*` guard here would let one
/// unfinished packet retire the cadence — design 97add747, decision 2.
#[test]
fn the_prune_carries_no_dedup_guard() {
    let raw = parse_raw_path(boss_testing::dispatcher_rules_dir())
        .expect("parse the shipped rule directory");
    let rule = raw
        .rules
        .iter()
        .find(|r| r.name == RULE)
        .unwrap_or_else(|| panic!("{RULE} is not in the shipped directory"));
    assert_eq!(
        rule.when, None,
        "`{RULE}` acquired a `when`. The prune's idempotence lives in the script's derived keep \
         set; a guard here is `a-dedup-guard-silently-retires-a-cadence` again (design 97add747)."
    );
}

// ---------------------------------------------------------------------
// THE WATCH (backlog 8d77d670, review H1): the daily delete runs with
// nobody reading it, and its request closes `answered` whether the verb
// deleted, refused (exit 2) or failed part-way (exit 1). The silence
// sweep sees only that a packet arrived. `watch-prune-registry-versions-
// daily` hands every close to `ops.judge` in watch mode, which files an
// alarm for any run that did not do its job.
// ---------------------------------------------------------------------

const WATCH: &str = "watch-prune-registry-versions-daily";

fn closed(outcome: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "closed_on": "2026-09-28",
        "kind": "ops-request",
        "outcome": outcome,
        "title": "prune the forge registry's image versions outside the derived keep set",
        "subject_id": "forge-registry",
        "parent_step_id": null,
    })
}

fn watch_invocations(outcome: &str) -> Vec<Vec<(String, Value)>> {
    let reg = common::authored_rule(WATCH);
    boss_dispatcher::rules::registry::match_event(
        &reg,
        "jobs.job.closed",
        &closed(outcome),
        &NoHelpers,
    )
    .matched
    .iter()
    .flat_map(|h| h.invocations.iter())
    .filter(|i| i.handler == "ops.judge")
    .map(|i| i.args.clone())
    .collect()
}

fn text<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a str> {
    match arg(args, k) {
        Some(Value::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// Every close of an ops-request reaches the watch — `answered` (the
/// verb ran, whatever its exit) AND `refused` (the runner would not run
/// it) — and the declaration is a WATCH of the daily verb: no follow-on.
#[test]
fn the_watch_reads_every_close_of_the_daily_prune_and_files_no_follow_on() {
    for outcome in ["answered", "refused"] {
        let inv = watch_invocations(outcome);
        assert_eq!(inv.len(), 1, "{outcome}: one ops.judge do: {inv:?}");
        let a = &inv[0];
        assert_eq!(text(a, "verb"), Some(VERB), "{a:?}");
        for k in ["then_verb", "then_host", "then_args"] {
            assert!(
                arg(a, k).is_none(),
                "the watch must file no follow-on, but names {k}: {a:?}"
            );
        }
    }
    assert!(
        watch_invocations("cancelled").is_empty(),
        "a cancelled request ran nothing and was a person's act"
    );
}

/// The verdict the watch reads is the prune's OWN closing line, read
/// out of the script — and only its clean real-run form: a dry run, a
/// part-way failure and a refusal do not match, so each of those is an
/// alarm (a failure by its exit first; a dry run by having no verdict).
#[test]
fn the_watch_verdict_is_the_prunes_own_ok_line() {
    let a = watch_invocations("answered").remove(0);
    let pattern = text(&a, "verdict_pattern").expect("verdict_pattern");
    assert!(
        !pattern.contains('\\'),
        "no backslash survives boss-expr: {pattern}"
    );
    let re = regex::Regex::new(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
    let script = std::fs::read_to_string(
        boss_testing::repo_root().join("infra/forge/prune-registry-versions.sh"),
    )
    .expect("the prune script");
    // The count the watch compares is DONE — the planned versions a
    // re-list after the deletes no longer holds — never the DELETEs'
    // answers: a concurrent 404 used to read `deleted 311 of 312` and
    // file a false alarm (backlog 1bef55a6, LOW), and counting every 404
    // as done let a route that answers 404 to everything pass `8 of 8`
    // (its review, finding 1).
    assert!(
        script.contains(r#"say "OK — deleted $DONE_TOTAL of $PLANNED_TOTAL planned version(s)"#),
        "the prune's closing line moved; the watch's pattern reads it"
    );
    assert!(
        script.contains("DONE_TOTAL=$((PLANNED_TOTAL - STILL_TOTAL))"),
        "the verdict's count must be what the re-list no longer holds"
    );
    // The line as it reads now: the groups bind to the one `deleted N of
    // M planned`, not to `indexes read N of M`, and to what the run could
    // judge (review finding 2). The real script's line is run through this
    // pattern in crates/core/boss-testing/tests/prune_registry_versions_sh.rs.
    let ok = "prune-registry-versions: OK — deleted 312 of 312 planned version(s) across boss boss-ci, shown gone by a re-list, 2 of them already gone (404); listed 1400 of 1400 versions, indexes read 140 of 140 tag(s), 100 percent; 3 unclassified kept; df /opt/forgejo/data before 70000000 KB";
    let caps = re.captures(ok).expect("the clean line matches");
    assert_eq!(
        (
            &caps["deleted"],
            &caps["planned"],
            &caps["listed"],
            &caps["total"],
            &caps["read_pct"],
            &caps["unclassified"]
        ),
        ("312", "312", "1400", "1400", "100", "3")
    );
    // Anchored at the line's start (re-review finding 3): the verdict's
    // own text carried inside another line — a forge header echoed into a
    // `registry:` line, say — is no verdict.
    assert!(
        pattern.starts_with("^prune-registry-versions: OK "),
        "the verdict pattern is not anchored: {pattern}"
    );
    assert!(
        !re.is_match(&format!(
            "prune-registry-versions: registry: 25 version(s) listed (the forge counts {})",
            ok.trim_start_matches("prune-registry-versions: ")
        )),
        "a verdict embedded in another line reads as one"
    );
    // A line of the shape before v2 carries no judgement, so it is no
    // verdict: the watch alarms `no verdict` rather than passing it.
    assert!(
        !re.is_match(
            "prune-registry-versions: OK — deleted 312 of 312 planned version(s) across boss boss-ci; df /opt/forgejo/data before 70000000 KB"
        ),
        "a verdict that says nothing of what the run could judge passes"
    );
    // A run that could read no index FAILS (exit 1) — the watch alarms
    // on the exit before it reads a line — and its line is no verdict.
    assert!(
        !re.is_match(
            "prune-registry-versions: FAILED — no manifest index could be read (indexes read 0 of 140 tag(s): /v2/token minted no bearer); 0 of 0 planned were deleted"
        ),
        "the no-index failure reads as a clean verdict"
    );
    for not_a_verdict in [
        "prune-registry-versions:   deleted 1 of 8 planned before the failure (HTTP 500); the rest stay for the next pass.",
        "prune-registry-versions: DRY RUN — would delete 8 version(s) across boss boss-ci; Nothing was deleted.",
        "prune-registry-versions:   0 of 8 deleted before the refusal. Nothing was deleted.",
    ] {
        assert!(!re.is_match(not_a_verdict), "{not_a_verdict}");
    }
    let when_src = text(&a, "when").expect("when");
    let when = boss_dispatcher::rules::expr::parse(when_src)
        .unwrap_or_else(|e| panic!("when {when_src:?}: {e}"));
    let holds = |g: serde_json::Value| {
        boss_dispatcher::rules::expr::eval(
            &when,
            &boss_dispatcher::rules::expr::Context {
                payload: &g,
                helpers: &NoHelpers,
            },
        )
        .unwrap_or_else(|e| panic!("{when_src:?} over {g}: {e}"))
    };
    let g =
        |deleted: i64, planned: i64, listed: i64, total: i64, read_pct: i64, unclassified: i64| {
            serde_json::json!({
                "deleted": deleted, "planned": planned, "listed": listed, "total": total,
                "read_pct": read_pct, "unclassified": unclassified,
            })
        };
    // A clean night.
    assert_eq!(holds(g(312, 312, 1400, 1400, 100, 3)), Value::Bool(true));
    assert_eq!(
        holds(g(312, 312, 1400, 1400, 90, 3)),
        Value::Bool(true),
        "90% is the floor, inclusive"
    );
    // A push between two pages lists one version twice: more read than
    // counted is still a whole listing.
    assert_eq!(holds(g(312, 312, 1401, 1400, 100, 3)), Value::Bool(true));
    // Not everything planned went.
    assert_eq!(holds(g(3, 312, 1400, 1400, 100, 3)), Value::Bool(false));
    // Re-review finding 1: a listing short of the forge's count — the
    // reviewer's `trunc` read 5 of 25 and planned 0 (the script refuses
    // it too; this is the second reader).
    assert_eq!(holds(g(0, 0, 5, 25, 100, 0)), Value::Bool(false));
    // Review finding 2: read too little of the registry (one index of
    // thirteen reads 7 percent; the script also fails that run).
    assert_eq!(holds(g(0, 0, 25, 25, 7, 13)), Value::Bool(false));
    assert_eq!(holds(g(312, 312, 1400, 1400, 89, 3)), Value::Bool(false));
    // Re-review finding 5: NO clause on a plan of 0. The live registry
    // always holds permanent non-sha tags (rust1.96), so a quiet day
    // plans 0 with unclassified > 0, and a WHOLE listing read at >= 90%
    // is what makes that 0 honest — so this is not an alarm.
    assert_eq!(holds(g(0, 0, 1400, 1400, 100, 2)), Value::Bool(true));
    assert_eq!(holds(g(0, 0, 1400, 1400, 100, 0)), Value::Bool(true));
}

/// The watch's `when` changed (v2, review finding 2), and a rule change
/// is a `version` bump or the live registry keeps the old row
/// (rules::seed writes a row at the version its FILE declares).
#[test]
fn the_watch_is_at_the_version_that_judges_what_the_run_could_read() {
    let raw = parse_raw_path(boss_testing::dispatcher_rules_dir())
        .expect("parse the shipped rule directory");
    let watch = raw
        .rules
        .iter()
        .find(|r| r.name == WATCH)
        .expect("the watch is in the shipped directory");
    assert!(
        watch.version >= 2,
        "the watch's args changed at v2: {}",
        watch.version
    );
}

/// A request that never CLOSES reaches no watch — `jobs.job.closed` is
/// the watch's only topic, and the silence sweep counts arrivals — so
/// it is the OPS QUEUE's (backlog 1bef55a6 (2), measured on origin/main
/// 2026-09-27): `ops-runner-queue-watched-every-5-minutes` reads every
/// open ops-request by `metadata.host` and files an urgent alarm when a
/// host's oldest request whose `execute` is ready or active has waited
/// past five minutes — a dead forge runner, a wedged one, a host that is
/// down. A request waiting on a passkey approval is outside that view (it
/// waits on a person), so the one thing that would take the daily
/// request out of it is `requires_approval`, on the spawn or on the verb.
/// Pinned here rather than restated as a second, prune-only age bound.
#[test]
fn a_daily_request_that_never_closes_is_the_ops_queue_watchs() {
    let raw = parse_raw_path(boss_testing::dispatcher_rules_dir())
        .expect("parse the shipped rule directory");
    let queue = raw
        .rules
        .iter()
        .find(|r| r.name == "ops-runner-queue-watched-every-5-minutes")
        .expect("the ops-queue watch is in the shipped directory");
    assert!(queue.schedule.is_some(), "the queue watch runs on a clock");
    assert!(
        queue
            .do_steps
            .iter()
            .any(|d| d.handler == "ops.queue.alarm"),
        "the queue watch runs ops.queue.alarm: {:?}",
        queue.do_steps
    );
    let daily = raw
        .rules
        .iter()
        .find(|r| r.name == RULE)
        .expect("the daily prune");
    let spawn = daily
        .do_steps
        .iter()
        .find(|d| d.handler == "jobs.spawn")
        .expect("the daily prune spawns");
    assert_eq!(
        spawn.args.get("metadata.host").map(String::as_str),
        Some("\"forge\""),
        "the queue watch keys on metadata.host: {:?}",
        spawn.args
    );
    assert!(
        spawn.args.keys().all(|k| !k.contains("requires_approval")),
        "an approval-gated request waits on a person and is outside the queue watch: {:?}",
        spawn.args
    );
    assert!(
        verb_file(VERB).get("requires_approval").is_none(),
        "the daily verb declares requires_approval; its open request would leave the queue watch"
    );
}

/// The verb the rule files serves the forge, runs the SAME script as the
/// hand verb with `--for-real` fixed in its argv, passes no keep count
/// (the script's default, one number in one place), and admits no
/// packet-supplied input — read from both verb files, not restated.
#[test]
fn the_verb_the_rule_files_is_the_hand_prune_with_its_mode_fixed() {
    let daily = verb_file(VERB);
    let hand = verb_file("prune-registry-versions");
    assert_eq!(
        daily["hosts"], hand["hosts"],
        "the daily verb serves exactly the hand verb's hosts"
    );
    assert_eq!(daily["hosts"], serde_json::json!(["forge"]));
    let script = hand["argv"][0]
        .as_str()
        .expect("the hand verb names a script");
    assert_eq!(
        daily["argv"],
        serde_json::json!([script, "--for-real"]),
        "the mode must be fixed in the argv on the hand verb's own script, and keep_trains left \
         to the script's default: {daily}"
    );
    // The fixed word is one the hand verb itself admits for its mode.
    assert!(
        hand["params"][0]["one_of"]
            .as_array()
            .is_some_and(|w| w.contains(&serde_json::json!("--for-real"))),
        "the hand verb no longer admits --for-real: {hand}"
    );
    assert_eq!(
        daily["params"].as_array().map(Vec::len),
        Some(0),
        "the daily verb must admit no packet-supplied input: {daily}"
    );
    assert_eq!(
        daily["timeout"], hand["timeout"],
        "the same run needs the same time"
    );
}
