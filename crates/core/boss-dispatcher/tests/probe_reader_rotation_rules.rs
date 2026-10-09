//! The probe-reader credential's two broker rules (design b35c22b4;
//! backlog d26515c5).
//!
//! `broker-rotates-the-probe-reader` fires the self-issued handler on a
//! rotation packet's scope step; `broker-advances-the-probe-reader-
//! rotation` fires it every fifteen minutes with `phase = "advance"` —
//! the machine token's pair (machine_token_rotation_rules.rs), for a
//! second credential the same handler rotates. What is data here, and
//! nothing the handler can check for itself:
//!
//! 1. THE SHAPE. One fires on a person's scope step and on nothing else;
//!    the other is a clock rule that says it advances. Neither can mint
//!    without a packet somebody scoped.
//! 2. THE PIN. The declaration lives in two files (CLAUDE.md §9a) and
//!    must be one: the Secret, the slot set, the registry row, the drain.
//! 3. THE SLOT SET. `gate_slots = "reader"` is what makes the handler
//!    wait for `reader.next` and `reader.current`. Without it the rule
//!    would wait for a bare `next` — the answer a gate gives only when
//!    this credential's value EQUALS an estate machine token — and
//!    promote on exactly the collision it must refuse.
//! 4. ONE NAMESPACE, AND NOT THE MACHINE TOKEN'S SECRET. The gates that
//!    read this credential run in the boss pod alone, and its rule must
//!    never name the Secret the estate token lives in.
//! 5. THE DRAIN is the machine token's ("the same drain", b35c22b4).

use boss_core::calendar::Cadence;
use boss_dispatcher::rules::registry::{RawRegistry, RawRule, parse_raw_path};
use boss_testing::dispatcher_rules_dir;

const ROTATION: &str = "broker-rotates-the-probe-reader";
const ADVANCE: &str = "broker-advances-the-probe-reader-rotation";
const ESTATE_ROTATION: &str = "broker-rotates-the-machine-token";
const HANDLER: &str = "credential.rotate.self-issued";
const CREDENTIAL: &str = "boss-probe-reader";

fn rules() -> RawRegistry {
    parse_raw_path(dispatcher_rules_dir()).expect("parse the rule directory")
}

fn rule<'a>(reg: &'a RawRegistry, name: &str) -> &'a RawRule {
    reg.rules
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("rule {name} is in the authored directory"))
}

/// An arg as the rule file spells it: a quoted string literal.
fn arg<'a>(r: &'a RawRule, key: &str) -> Option<&'a str> {
    r.do_steps[0].args.get(key).map(|v| {
        v.strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or_else(|| panic!("{}: `{key}` is not a string literal: {v}", r.name))
    })
}

#[test]
fn the_rotation_fires_on_the_scope_step_of_a_packet_about_the_probe_reader_and_on_nothing_else() {
    let reg = rules();
    let r = rule(&reg, ROTATION);
    assert_eq!(r.on_event.as_deref(), Some("step.done.credential-rotation"));
    assert!(
        r.schedule.is_none(),
        "the rotation rule is never clock-driven: a mint is a person's scope step"
    );
    let when = r.when.as_deref().unwrap_or_default();
    assert_eq!(
        when,
        format!(r#"subject_id = "{CREDENTIAL}" OR metadata.credential = "{CREDENTIAL}""#),
        "both spellings every broker rule matches, and no third way in"
    );
    assert_eq!(r.do_steps.len(), 1);
    assert_eq!(r.do_steps[0].handler, HANDLER);
    assert!(
        !r.do_steps[0].args.contains_key("phase"),
        "a scope firing runs the rotation, and declares no phase"
    );
}

#[test]
fn the_advance_is_a_quarter_hour_clock_rule_carrying_the_rotations_declaration() {
    let reg = rules();
    let advance = rule(&reg, ADVANCE);
    assert!(
        advance.on_event.is_none(),
        "clock-driven: {:?}",
        advance.on_event
    );
    let schedule = advance.schedule.as_ref().expect("a schedule");
    assert_eq!(schedule.cadence, Cadence::EveryNMinutes(15));
    assert_eq!(advance.do_steps.len(), 1);
    let (a, r) = (&advance.do_steps[0], &rule(&reg, ROTATION).do_steps[0]);
    assert_eq!(a.handler, HANDLER);
    assert_eq!(
        a.args.get("phase").map(String::as_str),
        Some("\"advance\""),
        "the one phase the handler runs from a clock: it advances packets already scoped, and \
         mints for none that was not"
    );
    let mut keys: Vec<&String> = a.args.keys().chain(r.args.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys.into_iter().filter(|k| *k != "phase") {
        assert_eq!(
            a.args.get(key),
            r.args.get(key),
            "{ADVANCE} and {ROTATION} disagree on `{key}`"
        );
    }
}

#[test]
fn both_rules_declare_the_reader_slot_set_one_secret_and_their_own_clock_rule() {
    let reg = rules();
    let estate = rule(&reg, ESTATE_ROTATION);
    for name in [ROTATION, ADVANCE] {
        let r = rule(&reg, name);
        assert_eq!(
            arg(r, "gate_slots"),
            Some("reader"),
            "{name}: without it the handler waits for a bare slot name, which a gate gives this \
             credential's value only when it equals an estate token"
        );
        assert_eq!(arg(r, "secret_namespace"), Some("boss"), "{name}");
        assert_eq!(arg(r, "secret_name"), Some(CREDENTIAL), "{name}");
        assert_eq!(arg(r, "credential_id"), Some(CREDENTIAL), "{name}");
        assert_eq!(
            arg(r, "also_in_namespaces"),
            None,
            "{name}: no other namespace holds a copy of the reader Secret"
        );
        assert_ne!(
            arg(r, "secret_name"),
            arg(estate, "secret_name"),
            "{name} must never write the Secret the estate machine token lives in"
        );
        assert_eq!(
            arg(r, "advance_rule"),
            Some(ADVANCE),
            "{name}: a deferral names the clock rule that resumes THIS credential"
        );
        assert_eq!(
            arg(r, "drain_minutes"),
            arg(estate, "drain_minutes"),
            "{name}: the same drain as the machine token's (design b35c22b4)"
        );
    }
    // The estate token's rules say nothing about a slot set, and so fill
    // the estate's — the default the handler reads for an absent arg.
    assert_eq!(arg(estate, "gate_slots"), None);
    assert_eq!(arg(estate, "advance_rule"), None);
}
