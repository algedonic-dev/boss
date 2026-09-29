//! The two rules that close a dead letter's backlog-item when an
//! operator answers the dead letter (backlog ac0a0abd):
//! `close-the-dead-letter-item-when-the-row-is-redelivered` and
//! `close-the-dead-letter-item-when-the-row-is-resolved`.
//!
//! `open-a-packet-when-an-event-is-dead-lettered` files one
//! backlog-item per dead letter, carrying the outbox row as
//! `metadata.outbox_id`. `boss events redeliver` (e22b692e) answers the
//! dead letter and records `events.outbox.redelivered` or
//! `events.outbox.resolved` naming the same outbox id — and until these
//! rules nothing read that fact back onto the item, so the item's
//! triage and the act that answered it were two disconnected records.
//!
//! The payloads below are the shapes `boss_events::outbox`'s
//! `redeliver_dead_letter` / `resolve_dead_letter` stage;
//! `rule_payload_contract` holds the bindings to the rosters migration
//! 20260928070119 declares for the two topics.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const REDELIVERED_RULE: &str = "close-the-dead-letter-item-when-the-row-is-redelivered";
const RESOLVED_RULE: &str = "close-the-dead-letter-item-when-the-row-is-resolved";
const FILING_RULE: &str = "open-a-packet-when-an-event-is-dead-lettered";
const REDELIVERED: &str = "events.outbox.redelivered";
const RESOLVED: &str = "events.outbox.resolved";

fn letter() -> serde_json::Value {
    serde_json::json!({
        "outbox_id": 4242,
        "event_id": "33333333-0000-0000-0000-000000000003",
        "event_kind": "jobs.step.updated",
        "event_source": "jobs",
        "payload_bytes": 1310851,
        "dead_lettered_at": "2026-09-28T04:30:00+00:00",
        "dead_letter_reason": "max payload size exceeded",
        "_actor": "emp-david",
    })
}

fn redelivered() -> serde_json::Value {
    let mut p = letter();
    p["overtaken_by"] = serde_json::json!(17);
    p
}

fn resolved() -> serde_json::Value {
    let mut p = letter();
    p["resolution"] = serde_json::json!("audit-only: the writer was shrunk in 1a2b3c4d");
    p
}

/// The one invocation `rule` makes on `topic` for `payload`, as
/// `(handler, args)`.
fn fire(
    reg: &Registry,
    topic: &str,
    payload: &serde_json::Value,
) -> (String, Vec<(String, Value)>) {
    let hits = match_event(reg, topic, payload, &NoHelpers).matched;
    assert_eq!(hits.len(), 1, "one act, one firing on {topic}");
    let inv = &hits[0].invocations;
    assert_eq!(inv.len(), 1, "one handler call");
    (inv[0].handler.clone(), inv[0].args.clone())
}

fn get(args: &[(String, Value)], k: &str) -> Value {
    args.iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
}

/// A redelivery withdraws the item filed for that row, recording the
/// act and how many later events it now arrives behind.
#[test]
fn a_redelivery_retracts_the_item_filed_for_that_outbox_row() {
    let (handler, args) = fire(
        &common::authored_rule(REDELIVERED_RULE),
        REDELIVERED,
        &redelivered(),
    );
    assert_eq!(handler, "jobs.retract_matching");
    assert_eq!(get(&args, "kind"), Value::String("backlog-item".into()));
    assert_eq!(get(&args, "match_key"), Value::String("outbox_id".into()));
    assert_eq!(get(&args, "value"), Value::Int(4242));
    assert_eq!(get(&args, "act"), Value::String("redelivered".into()));
    assert_eq!(get(&args, "note.overtaken_by"), Value::Int(17));
    assert!(
        matches!(get(&args, "because"), Value::String(s) if s.contains("redeliver")),
        "the evidence says what the act was"
    );
}

/// A resolution withdraws it too, recording the operator's reason.
#[test]
fn a_resolution_retracts_the_item_filed_for_that_outbox_row() {
    let (handler, args) = fire(&common::authored_rule(RESOLVED_RULE), RESOLVED, &resolved());
    assert_eq!(handler, "jobs.retract_matching");
    assert_eq!(get(&args, "kind"), Value::String("backlog-item".into()));
    assert_eq!(get(&args, "match_key"), Value::String("outbox_id".into()));
    assert_eq!(get(&args, "value"), Value::Int(4242));
    assert_eq!(get(&args, "act"), Value::String("resolved".into()));
    assert_eq!(
        get(&args, "note.resolution"),
        Value::String("audit-only: the writer was shrunk in 1a2b3c4d".into())
    );
    assert!(
        matches!(get(&args, "because"), Value::String(s) if s.contains("resolve")),
        "the evidence says what the act was"
    );
}

/// A FACT THAT LIVES TWICE GETS AN EQUALITY TEST (CLAUDE.md §9a). The
/// filing rule writes the outbox row onto the item under one metadata
/// key; the closing rules look the item up by a key they name. If the
/// two ever disagree the closing rules match nothing, silently, and
/// every answered dead letter's item stays open. So the key and the
/// kind the closers search are read off the filing rule here.
#[test]
fn the_closers_search_the_key_and_kind_the_filing_rule_writes() {
    let (filer, filed) = fire(
        &common::authored_rule(FILING_RULE),
        "events.outbox.dead_lettered",
        &serde_json::json!({
            "outbox_id": 4242,
            "event_id": "33333333-0000-0000-0000-000000000003",
            "event_kind": "jobs.step.updated",
            "event_source": "jobs",
            "payload_bytes": 1310851,
            "reason": "max payload size exceeded",
        }),
    );
    assert_eq!(filer, "jobs.spawn");
    for (rule, topic, payload) in [
        (REDELIVERED_RULE, REDELIVERED, redelivered()),
        (RESOLVED_RULE, RESOLVED, resolved()),
    ] {
        let (_, args) = fire(&common::authored_rule(rule), topic, &payload);
        assert_eq!(get(&args, "kind"), get(&filed, "kind"), "{rule}: same kind");
        let Value::String(key) = get(&args, "match_key") else {
            panic!("{rule}: match_key is a string");
        };
        assert_eq!(
            get(&filed, &format!("metadata.{key}")),
            get(&args, "value"),
            "{rule}: the filing rule writes `metadata.{key}` with the value the closer matches"
        );
    }
}

#[test]
fn no_other_topic_wakes_them() {
    for rule in [REDELIVERED_RULE, RESOLVED_RULE] {
        let reg = common::authored_rule(rule);
        for topic in [
            "events.outbox.dead_lettered",
            "jobs.step.updated",
            "jobs.job.closed",
        ] {
            let hits = match_event(&reg, topic, &redelivered(), &NoHelpers).matched;
            assert!(hits.is_empty(), "{topic} must not wake {rule}");
        }
    }
    // And each wakes only on its own act.
    let hits = match_event(
        &common::authored_rule(REDELIVERED_RULE),
        RESOLVED,
        &resolved(),
        &NoHelpers,
    )
    .matched;
    assert!(hits.is_empty(), "a resolution is not a redelivery");
}
