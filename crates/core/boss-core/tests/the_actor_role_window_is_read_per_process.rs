//! The actor-role report's 72-hour window, read from the log (backlog
//! e0bdba74; the enforce arm of design abf9eeae rests on it).
//!
//! Each test states one way the window could read clean when a process
//! that was watching says otherwise, or when nothing was watching — the
//! per-process lesson of review 6858ef1d on the machine gate's window:
//! a rule judged over facts merged across services must not let one
//! service's good start stand in for another's.

use boss_core::event::Event;
use boss_core::gate_evidence::{
    Fact, Gate, GateEvidenceLog, InMemoryGateEvidence, actor_role_would_refuse,
};
use boss_core::gate_window::{JoinedWindow, LiveRead, join_window};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

const GATE: Gate = Gate::ActorRole;

fn at(hours: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap() + Duration::hours(hours)
}

fn minutes(hours: i64, minutes: i64) -> DateTime<Utc> {
    at(hours) + Duration::minutes(minutes)
}

fn fact(service: &str, fact: Fact, when: DateTime<Utc>, mut payload: Value) -> Event {
    payload["service"] = json!(service);
    Event {
        id: Uuid::new_v4(),
        timestamp: when,
        source: service.into(),
        kind: GATE.kind(fact),
        payload,
    }
}

fn began(service: &str, instance: &str, when: DateTime<Utc>, mode: &str) -> Event {
    fact(
        service,
        Fact::RecordingBegan,
        when,
        json!({"mode": mode, "since": when, "instance": instance}),
    )
}

fn ended(service: &str, instance: &str, when: DateTime<Utc>) -> Event {
    fact(
        service,
        Fact::RecordingEnded,
        when,
        json!({"instance": instance, "clean": true, "lost": 0, "unstated": 0}),
    )
}

/// An agent registered as `engineering-agent` asserting `platform-admin`
/// on a write the record's role is not granted.
fn denied_shape() -> Value {
    json!({
        "actor": "agent-claude", "asserted_role": "platform-admin",
        "recorded_actor": "agent-claude", "recorded_role": "engineering-agent",
        "action": "update", "resource": "job", "lookup_status": "registered",
        "asserted_allowed": true, "recorded_allowed": false,
        "would_deny": true, "would_change_scope": false
    })
}

fn would_refuse(service: &str, instance: &str, since: DateTime<Utc>, when: DateTime<Utc>) -> Event {
    fact(
        service,
        Fact::WouldRefuse,
        when,
        json!({"instance": instance, "mode": "report", "recording_since": since,
               "reason": "would-deny", "key": denied_shape()}),
    )
}

/// What a service's `actor-role-reports` route answers.
fn live(service: &str, instance: &str, since: DateTime<Utc>, rows: Value) -> LiveRead {
    let dirty = rows
        .as_array()
        .map(|rows| rows.iter().filter(|r| !r["would_refuse"].is_null()).count())
        .unwrap_or(0);
    let not_clean = if dirty == 0 {
        json!([])
    } else {
        json!([format!(
            "{dirty} shape(s), {} observation(s), that `enforce` answers differently",
            dirty * 3
        )])
    };
    LiveRead {
        service: service.into(),
        answer: Ok(json!({
            "service": service,
            "mode": "report",
            "snapshot": {"state": "ready"},
            "report": {
                "mode": "report", "recording_since": since, "rows": rows,
                "overflow": 0, "not_clean": not_clean,
                "evidence": {"recorder": true, "instance": instance, "lost": 0,
                             "unstated": 0, "retrying": false, "last_error": null}
            }
        })),
    }
}

fn row(shape: Value, reason: Value, first: DateTime<Utc>, last: DateTime<Utc>) -> Value {
    json!({"observation": shape, "count": 3, "first_seen": first, "last_seen": last,
           "would_refuse": reason})
}

/// One service, restarted once at hour 74 across a clean end: the shape
/// every train leaves on the log.
fn restarted(service: &str) -> Vec<Event> {
    vec![
        began(service, "old", at(0), "report"),
        ended(service, "old", at(74)),
        began(service, "current", minutes(74, 1), "report"),
    ]
}

fn join(services: &[&str], facts: Vec<Event>, reads: Vec<LiveRead>) -> JoinedWindow {
    let required: Vec<String> = services.iter().map(|s| s.to_string()).collect();
    join_window(GATE, &required, at(3), at(75), Ok(facts), reads)
}

fn refuses(v: &JoinedWindow, naming: &str) {
    assert!(!v.covers_requested_window, "{v:#?}");
    assert!(v.clean_since.is_none(), "{v:#?}");
    assert!(
        v.not_clean.iter().any(|why| why.contains(naming)),
        "no reason names `{naming}`: {:#?}",
        v.not_clean
    );
}

#[test]
fn the_gate_states_five_kinds_under_one_prefix() {
    assert_eq!(
        GATE.kinds(),
        [
            "actor_role.recording_began",
            "actor_role.would_refuse",
            "actor_role.tally_overflowed",
            "actor_role.recording_ended",
            "actor_role.facts_lost",
        ]
    );
    assert_eq!(serde_json::to_value(GATE).unwrap(), json!("actor-role"));
    for kind in GATE.kinds() {
        assert!(boss_core::gate_evidence::KINDS.contains(&kind.as_str()));
    }
}

#[test]
fn a_restart_does_not_reset_the_window() {
    let v = join(
        &["jobs"],
        restarted("jobs"),
        vec![live("jobs", "current", minutes(74, 1), json!([]))],
    );
    assert!(v.covers_requested_window, "{v:#?}");
    assert_eq!(v.clean_since, Some(at(3)));
    assert_eq!(v.log.unwrap().coverage[0].recording_since, Some(at(0)));
}

#[test]
fn a_would_refuse_inside_the_window_ends_it_and_names_the_shape() {
    let mut facts = restarted("jobs");
    facts.push(would_refuse("jobs", "old", at(0), at(70)));
    let v = join(
        &["jobs"],
        facts,
        vec![live("jobs", "current", minutes(74, 1), json!([]))],
    );
    assert!(!v.covers_requested_window, "{v:#?}");
    assert!(v.clean_since.is_some_and(|since| since > at(70)));
    let log = v.log.unwrap();
    assert_eq!(log.dirty.len(), 1);
    assert_eq!(log.dirty[0].payload["key"]["actor"], "agent-claude");
}

/// The sibling review's finding, in this gate's terms: `people` never
/// restarted and its one process has held a would-refuse shape since
/// before the window opened, so the LOG's window holds no dirty fact —
/// and `jobs`, restarted and clean, must not make the answer clean.
#[test]
fn one_long_lived_process_holding_a_shape_is_not_hidden_by_anothers_clean_restart() {
    let mut facts = restarted("jobs");
    facts.push(began("people", "only", at(0), "report"));
    facts.push(would_refuse("people", "only", at(0), at(1)));
    let held = json!([row(denied_shape(), json!("would-deny"), at(1), at(60))]);
    let v = join(
        &["jobs", "people"],
        facts.clone(),
        vec![
            live("jobs", "current", minutes(74, 1), json!([])),
            live("people", "only", at(0), held),
        ],
    );
    assert!(
        v.log.as_ref().unwrap().dirty.is_empty(),
        "the fixture's point is that the log half alone is clean"
    );
    refuses(&v, "people:");
    // Control: the same log with `people` holding nothing is clean.
    let v = join(
        &["jobs", "people"],
        facts
            .into_iter()
            .filter(|e| e.kind != GATE.kind(Fact::WouldRefuse))
            .collect(),
        vec![
            live("jobs", "current", minutes(74, 1), json!([])),
            live("people", "only", at(0), json!([])),
        ],
    );
    assert!(v.covers_requested_window, "{v:#?}");
}

/// A first sighting says when a shape was FIRST seen, never when it was
/// last used. A process that sighted one before the window opened and
/// ended inside it may have answered that shape for every hour between,
/// and no live tally is left to say — so the window is not clean until
/// that process's end is itself older than the window.
#[tokio::test]
async fn a_shape_first_seen_before_the_window_by_a_process_that_ended_inside_it_is_not_forgiven() {
    let mut facts = restarted("jobs");
    let sighting = would_refuse("jobs", "old", at(0), at(1));
    facts.push(sighting.clone());
    let log = InMemoryGateEvidence::new(facts.clone());
    let read = log.facts(GATE, at(3)).await.unwrap();
    assert!(read.iter().any(|e| e.id == sighting.id));
    let v = join_window(
        GATE,
        &["jobs".into()],
        at(3),
        at(75),
        Ok(read),
        vec![live("jobs", "current", minutes(74, 1), json!([]))],
    );
    refuses(&v, "unknown final usage");

    // Aged out: read from after the old process ended, the newest start
    // before the window is the current process's, and nothing of the
    // old one is in the read.
    let later = log.facts(GATE, at(75)).await.unwrap();
    assert!(!later.iter().any(|e| e.id == sighting.id));
    let v = join_window(
        GATE,
        &["jobs".into()],
        at(75),
        at(150),
        Ok(later),
        vec![live("jobs", "current", minutes(74, 1), json!([]))],
    );
    assert!(v.covers_requested_window, "{v:#?}");
}

#[test]
fn a_stretch_where_no_process_recorded_is_not_covered() {
    // The old process ended cleanly; the next started three hours on.
    let facts = vec![
        began("jobs", "old", at(0), "report"),
        ended("jobs", "old", at(40)),
        began("jobs", "current", at(43), "report"),
    ];
    let v = join(
        &["jobs"],
        facts,
        vec![live("jobs", "current", at(43), json!([]))],
    );
    assert!(!v.covers_requested_window, "{v:#?}");
    assert_eq!(v.clean_since, Some(at(43)));

    // A process that stated no end may have died holding observations.
    let facts = vec![
        began("jobs", "old", at(0), "report"),
        began("jobs", "current", at(43), "report"),
    ];
    let v = join(
        &["jobs"],
        facts,
        vec![live("jobs", "current", at(43), json!([]))],
    );
    assert!(!v.covers_requested_window, "{v:#?}");
    assert_eq!(v.clean_since, Some(at(43)));
}

#[test]
fn a_service_that_never_stated_a_start_is_a_named_gap() {
    let v = join(
        &["jobs", "ledger"],
        restarted("jobs"),
        vec![
            live("jobs", "current", minutes(74, 1), json!([])),
            live("ledger", "unrecorded", at(0), json!([])),
        ],
    );
    refuses(&v, "ledger");
}

#[test]
fn a_dark_log_or_a_process_with_no_recorder_is_never_clean() {
    let v = join_window(
        GATE,
        &["jobs".into()],
        at(3),
        at(75),
        Err("audit_log is unreachable".into()),
        vec![live("jobs", "current", minutes(74, 1), json!([]))],
    );
    refuses(&v, "audit_log is unreachable");

    let mut read = live("jobs", "current", minutes(74, 1), json!([]));
    if let Ok(body) = &mut read.answer {
        body["report"]["evidence"]["recorder"] = json!(false);
    }
    refuses(&join(&["jobs"], restarted("jobs"), vec![read]), "recorder");
}

#[test]
fn the_live_answer_is_bound_to_the_process_the_log_last_heard_start() {
    // The log's newest start is `current`; the answering process is not.
    let v = join(
        &["jobs"],
        restarted("jobs"),
        vec![live("jobs", "someone-else", minutes(74, 1), json!([]))],
    );
    refuses(&v, "does not match sourced recording_began");
    // An answer for another service, or with no report at all.
    let mut read = live("jobs", "current", minutes(74, 1), json!([]));
    if let Ok(body) = &mut read.answer {
        body["service"] = json!("people");
    }
    refuses(&join(&["jobs"], restarted("jobs"), vec![read]), "names");
    let read = LiveRead {
        service: "jobs".into(),
        answer: Ok(json!({"service": "jobs", "mode": "report"})),
    };
    refuses(&join(&["jobs"], restarted("jobs"), vec![read]), "report");
}

#[test]
fn off_watches_nothing_and_a_report_word_moving_does_not_reset_the_clock() {
    // The same process began its tally again at hour 50, still `report`
    // (the mounted word was rewritten): the watch reaches back to 0.
    let mut facts = restarted("jobs");
    facts.insert(1, began("jobs", "old", at(50), "report"));
    let v = join(
        &["jobs"],
        facts,
        vec![live("jobs", "current", minutes(74, 1), json!([]))],
    );
    assert!(v.covers_requested_window, "{v:#?}");

    // Through `off` it does not.
    let mut facts = restarted("jobs");
    facts.insert(1, began("jobs", "old", at(40), "off"));
    facts.insert(2, began("jobs", "old", at(41), "report"));
    let v = join(
        &["jobs"],
        facts,
        vec![live("jobs", "current", minutes(74, 1), json!([]))],
    );
    assert!(!v.covers_requested_window, "{v:#?}");
    assert_eq!(v.clean_since, Some(at(41)));
}

#[test]
fn a_live_row_is_judged_by_the_rule_and_not_by_what_it_says_of_itself() {
    // A would-deny shape whose row claims it is no would-refuse.
    let lying = json!([row(denied_shape(), Value::Null, at(74), at(74))]);
    let mut read = live("jobs", "current", minutes(74, 1), lying);
    if let Ok(body) = &mut read.answer {
        body["report"]["not_clean"] = json!([]);
    }
    refuses(
        &join(&["jobs"], restarted("jobs"), vec![read]),
        "disagrees with its shape",
    );
    // A row from before the tally began.
    let early = json!([row(
        json!({"actor": "emp-david", "asserted_role": "platform-admin",
               "recorded_actor": "emp-david", "recorded_role": "platform-admin",
               "action": "read", "resource": "job", "lookup_status": "registered",
               "asserted_allowed": true, "recorded_allowed": true,
               "would_deny": false, "would_change_scope": false}),
        Value::Null,
        at(70),
        at(74)
    )]);
    refuses(
        &join(
            &["jobs"],
            restarted("jobs"),
            vec![live("jobs", "current", minutes(74, 1), early)],
        ),
        "invalid row",
    );
    // A MATCHING row touched after the observation's instant is what a
    // busy service looks like, and is not a reason.
    let busy = json!([row(
        matching_shape(),
        Value::Null,
        at(74) + Duration::minutes(2),
        at(76)
    )]);
    let v = join(
        &["jobs"],
        restarted("jobs"),
        vec![live("jobs", "current", minutes(74, 1), busy)],
    );
    assert!(v.covers_requested_window, "{v:#?}");
}

fn matching_shape() -> Value {
    json!({"actor": "emp-david", "asserted_role": "platform-admin",
           "recorded_actor": "emp-david", "recorded_role": "platform-admin",
           "action": "read", "resource": "job", "lookup_status": "registered",
           "asserted_allowed": true, "recorded_allowed": true,
           "would_deny": false, "would_change_scope": false})
}

/// What `enforce` answers differently, as the design's precondition
/// names it (abf9eeae Q2: zero would-deny for every registered actor and
/// zero unregistered writers) — and an unjudged comparison is no pass.
#[test]
fn the_rule_names_what_enforce_would_answer_differently() {
    let rule = |status: &str,
                action: &str,
                asserted: Option<bool>,
                recorded: Option<bool>,
                deny: Option<bool>,
                scope: Option<bool>| {
        actor_role_would_refuse(status, action, asserted, recorded, deny, scope)
    };
    let (yes, no) = (Some(true), Some(false));
    // A registered actor whose asserted and recorded answers agree.
    assert_eq!(rule("registered", "update", yes, yes, no, no), None);
    assert_eq!(
        rule("registered", "attribution", yes, None, None, None),
        None
    );
    // Enforce would allow what is denied today: no refusal.
    assert_eq!(rule("registered", "update", no, yes, no, no), None);
    assert_eq!(
        rule("registered", "update", yes, no, yes, no),
        Some("would-deny")
    );
    // A guard that states no `would_deny` (a visibility guard) but whose
    // recorded answer hides what the asserted one showed.
    assert_eq!(
        rule("registered", "visibility", yes, no, None, None),
        Some("would-deny")
    );
    assert_eq!(
        rule("registered", "read", yes, yes, no, yes),
        Some("would-change-scope")
    );
    // No registry row: the read-only floor at enforce.
    for write in [
        "create",
        "update",
        "close",
        "sign-off",
        "delete",
        "publish",
        "retire",
        "admission",
        "attribution",
        "a-door-named-later",
    ] {
        assert_eq!(
            rule("unregistered", write, yes, None, None, None),
            Some("unregistered-writer"),
            "{write}"
        );
    }
    for read in ["read", "visibility", "selection"] {
        assert_eq!(rule("unregistered", read, yes, None, None, None), None);
    }
    // The comparison could not be made: no evidence is not a pass.
    for status in [
        "unavailable",
        "ambiguous",
        "missing-role",
        "policy-unavailable",
        "comparison-timeout",
        "comparison-expired",
        "context-unavailable",
        "a-status-named-later",
    ] {
        assert_eq!(
            rule(status, "read", yes, None, None, None),
            Some("unjudged"),
            "{status}"
        );
    }
    // The asserted request itself got no answer; enforce changes nothing.
    assert_eq!(
        rule(
            "asserted-policy-unavailable",
            "update",
            None,
            None,
            None,
            None
        ),
        None
    );
}

/// A process start that states the launch roster it was started under,
/// as `boss_events::role_tally::durable` has every recording tally state.
fn began_under(service: &str, instance: &str, when: DateTime<Utc>, generation: &str) -> Event {
    let mut start = began(service, instance, when, "report");
    start.payload[boss_core::gate_window::ROSTER_FIELD] =
        json!({"generation": generation, "commit": "fixture"});
    start
}

/// AN EXCUSE IS BOUND TO THE SELECTION THAT MADE IT (backlog b13bd730,
/// from delta review 72bd1c05 of car 35baf5b0).
///
/// Until this car the actor-role window took its excuses from the launch
/// record and judged them on three checks only — the port refuses, the
/// tally read fails, no fact inside the window — because an actor-role
/// start stated no roster stamp and the generation rule was the machine
/// gate's alone. So a service the OLD selection ran, whose last process
/// started before the window opened and stated nothing inside it, and
/// which the NEW selection skips, was excused with its unread tally, its
/// lost facts and its overflow inherited as clean: under a row H enforce
/// that licenses the flip while that service's callers were never
/// observed. The first case below is that input, and it read clean.
///
/// Now every recording tally states the stamp with its start, and this
/// window is judged by `judged_generations` exactly as the machine
/// gate's is.
#[test]
fn an_excuse_holds_only_for_hours_watched_under_the_selection_that_made_it() {
    use boss_core::gate_window::{
        PortProbe, join_launched_window, launch_roster, roster_generation,
    };
    let old = roster_generation(
        "boss-launch-record v1\n\
         start boss-jobs-api\n\
         start boss-assets-api\n\
         end 2\n",
    )
    .unwrap();
    let record = "boss-launch-record v1\n\
        start boss-jobs-api\n\
        skip boss-assets-api module equipment is not on in the tenant manifest\n\
        end 2\n";
    let roster = launch_roster(
        &[
            ("jobs".to_string(), vec!["boss-jobs-api".to_string()]),
            ("assets".to_string(), vec!["boss-assets-api".to_string()]),
        ],
        Ok(record),
    );
    assert_eq!(roster.required, vec!["jobs".to_string()]);
    let new = roster.generation.clone().expect("a whole record digests");
    assert_ne!(old, new);
    let probes = [PortProbe {
        service: "assets".into(),
        listening: Ok(false),
    }];
    let reads = || {
        vec![
            live("jobs", "current", minutes(74, 1), json!([])),
            LiveRead {
                service: "assets".into(),
                answer: Err("connection refused".into()),
            },
        ]
    };
    let judge = |facts: Vec<Event>| {
        join_launched_window(GATE, &roster, &probes, at(3), at(75), Ok(facts), reads())
    };

    // The item's own input. The old selection ran both; assets' last
    // process started at hour 0, before the window opened at hour 3, and
    // stated nothing after; the relaunch at hour 74 skips it.
    let v = judge(vec![
        began_under("jobs", "old", at(0), &old),
        began_under("assets", "gone", at(0), &old),
        ended("jobs", "old", at(74)),
        began_under("jobs", "current", minutes(74, 1), &new),
    ]);
    refuses(
        &v,
        "launch roster: the window spans a change of launch roster",
    );
    let named: Vec<Option<&str>> = v
        .roster_generations
        .iter()
        .map(|g| g.generation.as_deref())
        .collect();
    assert_eq!(named, [Some(old.as_str()), Some(new.as_str())], "{v:#?}");

    // Starts that state no stamp — every start a build before this car
    // wrote — are not hours watched under this selection either: the
    // mechanism's first boot does not backdate itself.
    let v = judge(restarted("jobs"));
    refuses(&v, "launch roster:");

    // Every hour under the selection that makes the excuse: clean, and
    // the answer names that one generation.
    let under_new = || {
        vec![
            began_under("jobs", "old", at(0), &new),
            ended("jobs", "old", at(74)),
            began_under("jobs", "current", minutes(74, 1), &new),
        ]
    };
    let v = judge(under_new());
    assert_eq!(v.not_clean, Vec::<String>::new(), "{v:#?}");
    assert!(v.covers_requested_window, "{v:#?}");
    let named: Vec<Option<&str>> = v
        .roster_generations
        .iter()
        .map(|g| g.generation.as_deref())
        .collect();
    assert_eq!(named, [Some(new.as_str())], "{v:#?}");

    // The excuse is still a claim this window checks: an excused
    // service that left an actor-role fact inside the window is refused.
    let mut facts = under_new();
    facts.push(began_under("assets", "stray", at(10), &new));
    refuses(&judge(facts), "assets");
}

/// Review 72bd1c05, F1. The generation rule's match ended in a wildcard
/// that was OPEN for every gate but the machine gate: a policy-check
/// window handed a roster read from a record — excuses and all — was
/// judged under no generation and said nothing. Unreachable in
/// production (`LocalTallies::roster` consults no record for that gate),
/// and exactly the pattern the next gate added there would inherit. No
/// gate is excepted now: a roster with a generation is judged, and a
/// gate whose starts state no stamp reads never-clean and says why.
#[test]
fn a_roster_with_a_generation_is_judged_whatever_the_gate() {
    use boss_core::gate_window::{LaunchRoster, NotLaunched, join_launched_window};
    let roster = LaunchRoster {
        required: vec!["policy".into()],
        not_launched: vec![NotLaunched {
            service: "assets".into(),
            binary: "boss-assets-api".into(),
            reason: "module equipment is not on in the tenant manifest".into(),
        }],
        errors: Vec::new(),
        generation: Some("sha256:fixture".into()),
    };
    for gate in Gate::ALL {
        let v = join_launched_window(
            gate,
            &roster,
            &[],
            at(3),
            at(75),
            Ok(Vec::new()),
            Vec::new(),
        );
        assert!(
            v.not_clean
                .iter()
                .any(|why| why.starts_with("launch roster:")),
            "{gate:?} was handed a roster generation and no reason names the launch roster: {:#?}",
            v.not_clean
        );
    }
}
