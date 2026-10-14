//! A writer that signs one id per firing resolves to the ONE row that
//! declares its family (backlog ddf0773e, design abf9eeae; the registry
//! half is car 1's `signs_for`).
//!
//! WHY. Measured live 2026-10-07 on `GET /api/jobs/actor-role-reports`:
//! 34 of the 40 actors the tally called `unregistered` were dispatcher
//! rules, `rule:<name>` on the wire — 2,294 observations, every one a
//! write or read the dispatcher made. The registry had answered for them
//! since car 1: `automation:dispatcher` declares `signs_for =
//! "automation:rule:"`, because a row per rule would copy the
//! `dispatcher_rules` registry (tenant rules included) into this one. The
//! resolver compared ids for equality only, so it never read that field,
//! and the design's "zero unregistered writers" could not be met by any
//! number of rows. These tests hold the resolver to the registry's own
//! family rule, and to the wire spelling the audit log already normalises
//! (`User::ambient_actor`: `rule:<name>` is `automation:rule:<name>`).
use boss_core::role_of_record::{RoleLookupError, RoleOfRecord};
use boss_policy_client::role_reader::RegistryRoles;
use serde_json::{Value, json};

fn none() -> Value {
    json!({"data":[],"total":0})
}

fn automations() -> Value {
    json!({"data":[
        {"id":"automation:dispatcher","role":"platform-admin","description":"d","signs_for":"automation:rule:"},
        {"id":"automation:ops-runner","role":"platform-admin","description":"o","signs_for":"automation:ops-runner:"},
        {"id":"automation:gate-runner","role":"audit-readonly","description":"g"}
    ],"total":3})
}

fn roles() -> RegistryRoles {
    RegistryRoles::from_sources(none(), automations(), json!([])).unwrap()
}

#[tokio::test]
async fn a_firing_rule_resolves_to_the_dispatchers_row_in_both_spellings() {
    let roles = roles();
    for wire in [
        "rule:auto-park-on-gate-green",
        "automation:rule:auto-park-on-gate-green",
        "rule:a-tenant-declared-rule-no-tree-names",
    ] {
        let got = roles.role_for(wire).await.unwrap();
        let got = got.unwrap_or_else(|| panic!("{wire} has no role of record"));
        assert_eq!(got.actor_id, "automation:dispatcher", "{wire}");
        assert_eq!(got.role.as_deref(), Some("platform-admin"), "{wire}");
    }
    let run = roles
        .role_for("automation:ops-runner:forge:20260930T201448Z-2040087")
        .await
        .unwrap()
        .expect("an ops-runner pass resolves to its signer");
    assert_eq!(run.actor_id, "automation:ops-runner");
}

#[tokio::test]
async fn a_row_of_its_own_still_answers_for_itself_and_nothing_else_widens() {
    let roles = roles();
    let own = roles.role_for("automation:dispatcher").await.unwrap();
    assert_eq!(own.unwrap().actor_id, "automation:dispatcher");
    let gate = roles.role_for("automation:gate-runner").await.unwrap();
    assert_eq!(gate.unwrap().role.as_deref(), Some("audit-readonly"));
    // The legacy bare spelling the audit log records as
    // `automation:gate-runner` is judged under that row, by the same
    // mapping (`automation_slug`).
    let bare = roles.role_for("gate-runner").await.unwrap();
    assert_eq!(bare.unwrap().actor_id, "automation:gate-runner");
    // The prefix alone names no member; a near miss is no member; a row
    // with no family answers only for its own id.
    for stranger in [
        "rule:",
        "automation:rule:",
        "automation:rules-engine",
        "automation:gate-runner:extra",
        "another-runner",
        "ruler:x",
        "agent-claude",
    ] {
        assert_eq!(
            roles.role_for(stranger).await.unwrap(),
            None,
            "{stranger} must stay unregistered"
        );
    }
}

/// Two well-formed prefixes cannot nest (a slug holds no colon), so a
/// response that declares a nested one has a malformed row, and is a
/// registry that could not answer — never a narrower family that
/// quietly wins (review ed9181b4, F2). Two rows declaring the same
/// prefix are ambiguous: this reads a response, not the table.
#[tokio::test]
async fn a_nested_family_is_refused_and_a_family_declared_twice_is_ambiguous() {
    let nested = json!({"data":[
        {"id":"automation:dispatcher","role":"platform-admin","signs_for":"automation:rule:"},
        {"id":"automation:tenant-rules","role":"audit-readonly","signs_for":"automation:rule:tenant-"}
    ],"total":2});
    assert!(matches!(
        RegistryRoles::from_sources(none(), nested, json!([])),
        Err(RoleLookupError::Unavailable(_))
    ));

    let twice = json!({"data":[
        {"id":"automation:a","role":"platform-admin","signs_for":"automation:rule:"},
        {"id":"automation:b","role":"audit-readonly","signs_for":"automation:rule:"}
    ],"total":2});
    let roles = RegistryRoles::from_sources(none(), twice, json!([])).unwrap();
    assert!(matches!(
        roles.role_for("rule:x").await,
        Err(RoleLookupError::Ambiguous(_))
    ));
}

/// A family is an automation's to declare. An agents row or a people row
/// carrying the key answers for no one but itself, and a malformed
/// `signs_for` is a registry that could not answer, never a quiet "no
/// family".
#[tokio::test]
async fn only_an_automation_row_declares_a_family_and_a_malformed_one_is_refused() {
    let agents = json!({"data":[
        {"id":"agent-example","aliases":[],"role":"tenant-engineer","signs_for":"automation:rule:"}
    ],"total":1});
    let people = json!([
        {"id":"emp-a","status":"active","role":"platform-admin","signs_for":"automation:rule:"}
    ]);
    let roles = RegistryRoles::from_sources(agents, none(), people).unwrap();
    assert_eq!(roles.role_for("rule:x").await.unwrap(), None);

    // The last four are strings of the wrong SHAPE. The write side refuses
    // each three ways, and measured 2026-10-07 (review ed9181b4, F2) this
    // reader accepted them: `automation:` answered for every
    // automation-spelled caller with no row of its own, at platform-admin.
    for bad in [
        json!(42),
        json!(""),
        json!("   "),
        json!(["automation:rule:"]),
        json!("a"),
        json!("automation:"),
        json!("automation:ops-"),
        json!("rule:"),
    ] {
        let rows = json!({"data":[
            {"id":"automation:dispatcher","role":"platform-admin","signs_for":bad}
        ],"total":1});
        assert!(matches!(
            RegistryRoles::from_sources(none(), rows, json!([])),
            Err(RoleLookupError::Unavailable(_))
        ));
    }
}

/// A family vouches only for a member whose tail is `:`-separated
/// `[A-Za-z0-9-]` segments — every one of the 124 live dispatcher rule
/// names and the ops runner's `<host>:<moment>-<pid>`, read 2026-10-10.
/// Measured 2026-10-07 (review ed9181b4, F5), each id below resolved to
/// the dispatcher's platform-admin row. Report mode only states it today;
/// under enforce it would be a grant to whatever the caller typed.
#[tokio::test]
async fn a_member_tail_of_any_other_shape_is_unregistered() {
    let roles = roles();
    for typed in [
        "rule: ",
        "rule:x ",
        "rule:../../x",
        "rule:a/b",
        "rule:x\u{202e}",
        "automation:rule:x\u{202e}gnp.exe",
        "rule::x",
        "rule:x:",
        "automation:ops-runner:forge:",
    ] {
        assert_eq!(
            roles.role_for(typed).await.unwrap(),
            None,
            "{typed:?} must stay unregistered"
        );
    }
}

/// An exact row wins over a family, so registering a member by name is
/// never made ambiguous by its signer.
#[tokio::test]
async fn an_exact_row_wins_over_a_family() {
    let agents = json!({"data":[
        {"id":"agent-example","aliases":["rule:borrowed-name"],"role":"tenant-engineer"}
    ],"total":1});
    let roles = RegistryRoles::from_sources(agents, automations(), json!([])).unwrap();
    let got = roles.role_for("rule:borrowed-name").await.unwrap().unwrap();
    assert_eq!(got.actor_id, "agent-example");
}
