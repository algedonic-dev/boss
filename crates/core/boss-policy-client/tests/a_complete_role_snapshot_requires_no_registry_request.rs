//! Request-time role resolution must terminate even on the registry services.
//! Complete immutable data is the lookup dependency, not another HTTP read.
use boss_core::role_of_record::{RoleLookupError, RoleOfRecord};
use boss_policy_client::role_reader::RegistryRoles;
use serde_json::{Value, json};

fn agents() -> Value {
    json!({"data":[{"id":"agent-example","aliases":["agent-alias"],"role":"tenant-engineer"}],"total":1})
}

fn automations() -> Value {
    json!({"data":[{"id":"automation:people","role":"platform-admin"}],"total":1})
}

fn people() -> Value {
    json!([
        {"id":"emp-active","email":"Reader@Example.test","status":"active","role":null},
        {"id":"emp-retired","email":"retired@example.test","status":"inactive","role":"platform-admin"}
    ])
}

#[tokio::test]
async fn an_owned_complete_snapshot_resolves_aliases_people_and_automations_without_io() {
    let roles = RegistryRoles::from_sources(agents(), automations(), people()).unwrap();
    let alias = roles.role_for("agent-alias").await.unwrap().unwrap();
    assert_eq!(alias.actor_id, "agent-example");
    assert_eq!(alias.role.as_deref(), Some("tenant-engineer"));
    assert_eq!(
        roles
            .role_for("automation:people")
            .await
            .unwrap()
            .unwrap()
            .role
            .as_deref(),
        Some("platform-admin")
    );
    let person = roles
        .role_for("reader@example.test")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(person.actor_id, "emp-active");
    assert_eq!(person.role, None);
    assert_eq!(roles.role_for("emp-retired").await.unwrap(), None);
    assert_eq!(roles.role_for("missing").await.unwrap(), None);
}

#[tokio::test]
async fn overlapping_sources_and_aliases_remain_ambiguous() {
    let people =
        json!([{"id":"emp-active","email":"agent-alias","status":"active","role":"reviewer"}]);
    let roles = RegistryRoles::from_sources(agents(), automations(), people).unwrap();
    assert!(matches!(
        roles.role_for("agent-alias").await,
        Err(RoleLookupError::Ambiguous(_))
    ));
    assert_eq!(
        roles
            .role_for("agent-example")
            .await
            .unwrap()
            .unwrap()
            .actor_id,
        "agent-example"
    );
}

#[test]
fn partial_or_malformed_sources_cannot_be_published_as_a_complete_snapshot() {
    for invalid in [
        json!({"data":[],"total":1}),
        json!({"data":[{"id":"unrelated","aliases":[],"role":42}],"total":1}),
        json!({"data":[{"id":"unrelated","aliases":[42],"role":"reviewer"}],"total":1}),
        json!({"data":[{"id":"   ","aliases":["agent-alias"],"role":"reviewer"}],"total":1}),
        json!({"data":[{"id":"unrelated","aliases":["   "],"role":"reviewer"}],"total":1}),
    ] {
        assert!(matches!(
            RegistryRoles::from_sources(invalid, automations(), people()),
            Err(RoleLookupError::Unavailable(_))
        ));
    }
}

#[test]
fn malformed_active_person_login_is_unknown_instead_of_complete_absence() {
    for email in [
        json!(42),
        json!({"login":"reader@example.test"}),
        json!("   "),
    ] {
        let people = json!([{"id":"emp-active","email":email,"status":"active","role":"reviewer"}]);
        assert!(matches!(
            RegistryRoles::from_sources(agents(), automations(), people),
            Err(RoleLookupError::Unavailable(_))
        ));
    }
}
