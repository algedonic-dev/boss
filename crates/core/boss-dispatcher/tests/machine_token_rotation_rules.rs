//! The estate machine token's two broker rules (design 6805c764, car 3;
//! backlog 2710c8fc).
//!
//! `broker-rotates-the-machine-token` fires the self-issued handler on a
//! rotation packet's scope step; `broker-advances-the-machine-token-
//! rotation` fires it every fifteen minutes with `phase = "advance"`,
//! because a rotation waits on kubelet's Secret refresh and a day-long
//! drain window, and one delivery holds its message for 30 seconds.
//! Two things about the pair are data the handler cannot check for
//! itself:
//!
//! 1. THE SHAPE. The advance rule is scheduled (no `on_event`), runs the
//!    one handler, and says `phase = "advance"` — the handler refuses a
//!    clock firing without it, and a rule that lost it would refuse every
//!    quarter-hour on the dead-letter shelf while every rotation sat.
//! 2. THE PIN. Its declaration lives in a second file (CLAUDE.md §9a). A
//!    sweep reading a different Secret would promote into, or revoke
//!    from, a Secret the gates do not mount; a different drain window
//!    would revoke on one firing what the other still holds.

use boss_core::calendar::Cadence;
use boss_dispatcher::rules::registry::{RawRegistry, RawRule, parse_raw_path};
use boss_testing::dispatcher_rules_dir;

const ROTATION: &str = "broker-rotates-the-machine-token";
const ADVANCE: &str = "broker-advances-the-machine-token-rotation";
const HANDLER: &str = "credential.rotate.self-issued";

fn rules() -> RawRegistry {
    parse_raw_path(dispatcher_rules_dir()).expect("parse the rule directory")
}

fn rule<'a>(reg: &'a RawRegistry, name: &str) -> &'a RawRule {
    reg.rules
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("rule {name} is in the authored directory"))
}

#[test]
fn the_rotation_fires_on_the_scope_step_of_a_packet_about_the_machine_token() {
    let reg = rules();
    let r = rule(&reg, ROTATION);
    assert_eq!(r.on_event.as_deref(), Some("step.done.credential-rotation"));
    let when = r.when.as_deref().unwrap_or_default();
    assert!(
        when.contains(r#"subject_id = "boss-machine-token""#)
            && when.contains(r#"metadata.credential = "boss-machine-token""#),
        "both spellings every broker rule matches: {when}"
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
    assert_eq!(
        schedule.cadence,
        Cadence::EveryNMinutes(15),
        "kubelet refreshes a Secret in about a minute; a quarter-hour is the wait a promotion \
         costs, not a day"
    );
    assert_eq!(advance.do_steps.len(), 1);
    let (a, r) = (&advance.do_steps[0], &rule(&reg, ROTATION).do_steps[0]);
    assert_eq!(a.handler, HANDLER);
    assert_eq!(a.args.get("phase").map(String::as_str), Some("\"advance\""));
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
