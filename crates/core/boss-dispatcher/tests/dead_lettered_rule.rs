//! The `open-a-packet-when-an-event-is-dead-lettered` rule
//! (`infra/dispatcher/rules/open-a-packet-when-an-event-is-dead-lettered.toml`,
//! backlog e4019cbc).
//!
//! The outbox relay used to retry a row the bus refused forever, at the
//! head of the queue, stalling every event behind it with only a warn
//! line to say so. It now dead-letters that row and stages
//! `events.outbox.dead_lettered`; this rule is what makes a person read
//! it: one backlog-item per dead letter, carrying the outbox row, the
//! refused event's id, kind, source, size and the bus's refusal — the
//! scalars of the event, bound by name.
//!
//! The payload below is the shape `boss_events::outbox`'s `dead_letter`
//! stages; `rule_payload_contract` holds the rule's bindings to the
//! roster migration 20260928041216 declares for the topic.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "open-a-packet-when-an-event-is-dead-lettered";
const TOPIC: &str = "events.outbox.dead_lettered";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn dead_lettered() -> serde_json::Value {
    serde_json::json!({
        "outbox_id": 4242,
        "event_id": "33333333-0000-0000-0000-000000000003",
        "event_kind": "jobs.step.updated",
        "event_source": "jobs",
        "payload_bytes": 1310851,
        "reason": "max payload size exceeded: Payload size limit of 1048576 exceeded by message size of 1310851",
        "_actor": "automation:event-relay",
    })
}

#[test]
fn a_dead_letter_files_one_backlog_item_naming_the_row_and_its_kind() {
    let reg = rule();
    let hits = match_event(&reg, TOPIC, &dead_lettered(), &NoHelpers).matched;
    assert_eq!(hits.len(), 1, "one dead letter, one packet");
    let inv = &hits[0].invocations;
    assert_eq!(inv.len(), 1);
    assert_eq!(inv[0].handler, "jobs.spawn");

    let args = &inv[0].args;
    let get = |k: &str| {
        args.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
    };
    assert_eq!(get("kind"), Value::String("backlog-item".into()));
    assert_eq!(get("subject_kind"), Value::String("custom".into()));
    assert_eq!(get("subject"), Value::String("events".into()));
    assert_eq!(get("metadata.area"), Value::String("events".into()));
    assert_eq!(get("metadata.outbox_id"), Value::Int(4242));
    assert_eq!(
        get("metadata.event_id"),
        Value::String("33333333-0000-0000-0000-000000000003".into())
    );
    assert_eq!(
        get("metadata.event_kind"),
        Value::String("jobs.step.updated".into())
    );
    assert_eq!(get("metadata.event_source"), Value::String("jobs".into()));
    assert_eq!(get("metadata.payload_bytes"), Value::Int(1310851));
    assert_eq!(
        get("metadata.reason"),
        Value::String(
            "max payload size exceeded: Payload size limit of 1048576 exceeded by message size of 1310851"
                .into()
        )
    );
    assert_eq!(
        get("metadata.input_channel"),
        Value::String("telemetry/monitoring".into())
    );
}

#[test]
fn no_other_topic_wakes_it() {
    let reg = rule();
    for topic in [
        "jobs.step.updated",
        "jobs.job.closed",
        "jobs.step.hold_lost",
    ] {
        let hits = match_event(&reg, topic, &dead_lettered(), &NoHelpers).matched;
        assert!(hits.is_empty(), "{topic} must not file a dead-letter item");
    }
}
