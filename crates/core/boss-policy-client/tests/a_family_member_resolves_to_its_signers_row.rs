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

#[tokio::test]
async fn the_longest_family_answers_and_a_family_declared_twice_is_ambiguous() {
    let nested = json!({"data":[
        {"id":"automation:dispatcher","role":"platform-admin","signs_for":"automation:rule:"},
        {"id":"automation:tenant-rules","role":"audit-readonly","signs_for":"automation:rule:tenant-"}
    ],"total":2});
    let roles = RegistryRoles::from_sources(none(), nested, json!([])).unwrap();
    let got = roles.role_for("rule:tenant-x").await.unwrap().unwrap();
    assert_eq!(got.actor_id, "automation:tenant-rules");
    let got = roles.role_for("rule:other").await.unwrap().unwrap();
    assert_eq!(got.actor_id, "automation:dispatcher");

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

    for bad in [
        json!(42),
        json!(""),
        json!("   "),
        json!(["automation:rule:"]),
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
