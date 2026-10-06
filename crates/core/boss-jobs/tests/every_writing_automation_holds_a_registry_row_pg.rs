//! Every writing automation holds a row in the agents registry, at the
//! role it signs as today (backlog ddf0773e, design abf9eeae car 1).
//!
//! What this file proves is the half the unit tests in
//! `agents::automations` cannot: that the IN-TREE bundle
//! (`infra/platform/automations/`) lands in a real database through the
//! adapter the platform seed calls, that it changes no one — every row
//! at the `platform-admin` its caller already asserts — that a second
//! start inserts nothing, and that a row an operator narrowed survives
//! the next start (the instance is the truth, design e187198f). The
//! schema's CHECKs are proved by raw INSERTs that go around the adapter,
//! because the question there is whether the DATABASE refuses.

use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use boss_jobs::agents::automations::{load_automations_dir, platform_automations_path};
use boss_jobs::agents::{AUTOMATION_DECLARED, AgentsRegistry, PgAgents};
use boss_testing::TestDb;

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("platform-workflow-seed".into()))
}

fn bundle() -> Vec<boss_jobs::agents::AutomationActor> {
    load_automations_dir(std::path::Path::new(platform_automations_path()))
        .expect("the in-tree automations bundle loads")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bundle_lands_once_at_the_role_its_callers_assert_and_a_narrowed_row_is_kept() {
    let db = TestDb::new().await;
    let registry = PgAgents::new(db.pool.clone());
    let declared = bundle();

    let first = registry
        .declare_automations(&declared, &stamp())
        .await
        .expect("a fresh database takes the bundle");
    assert_eq!(first.inserted.len(), declared.len(), "{first}");
    assert!(first.present.is_empty() && first.kept.is_empty(), "{first}");

    let rows = registry.list_automations().await.expect("listing");
    assert_eq!(rows, {
        let mut want = declared.clone();
        want.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
        want
    });
    // Changes no one: every row carries the role its caller sends
    // today, which on 2026-10-01 is platform-admin for every one.
    let other: Vec<_> = rows
        .iter()
        .filter(|a| a.role != "platform-admin")
        .map(|a| (&a.id, &a.role))
        .collect();
    assert!(other.is_empty(), "car 1 narrows no one: {other:?}");

    // One fact per inserted row, on the outbox with the row.
    let staged: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT source, payload FROM event_outbox WHERE kind = $1")
            .bind(AUTOMATION_DECLARED)
            .fetch_all(&db.pool)
            .await
            .expect("outbox reads");
    assert_eq!(staged.len(), declared.len());
    assert!(
        staged.iter().all(|(source, p)| source == "jobs"
            && p["declared_by"] == "automation:platform-workflow-seed"
            && p["role"] == "platform-admin"),
        "{staged:?}"
    );

    // A second start inserts nothing and records nothing.
    let again = registry
        .declare_automations(&declared, &stamp())
        .await
        .expect("a second start");
    assert!(again.inserted.is_empty(), "{again}");
    assert_eq!(again.present.len(), declared.len(), "{again}");

    // An operator narrows a role in the instance; the next start keeps
    // it and NAMES the difference rather than reverting it.
    sqlx::query("UPDATE automation_actors SET role = 'audit-readonly' WHERE id = $1")
        .bind("automation:recovery-kit")
        .execute(&db.pool)
        .await
        .expect("narrow one row");
    let kept = registry
        .declare_automations(&declared, &stamp())
        .await
        .expect("a third start");
    assert_eq!(kept.kept.len(), 1, "{kept}");
    assert_eq!(kept.kept[0].id, "automation:recovery-kit");
    assert_eq!(kept.kept[0].differs, ["role"]);
    let role: String = sqlx::query_scalar(
        "SELECT role FROM automation_actors WHERE id = 'automation:recovery-kit'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("read back");
    assert_eq!(role, "audit-readonly", "the instance is the truth");
    let facts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind = $1")
        .bind(AUTOMATION_DECLARED)
        .fetch_one(&db.pool)
        .await
        .expect("outbox count");
    assert_eq!(facts as usize, declared.len(), "no fact for a kept row");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_schema_refuses_a_family_member_a_bad_prefix_and_a_shared_family() {
    let db = TestDb::new().await;
    let insert = |id: &'static str, signs_for: Option<&'static str>| {
        let pool = db.pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO automation_actors (id, role, description, signs_for) \
                 VALUES ($1, 'platform-admin', 'x', $2)",
            )
            .bind(id)
            .bind(signs_for)
            .execute(&pool)
            .await
        }
    };
    for bad in [
        "automation:rule:converge-on-merge",
        "rule:converge-on-merge",
        "agent-claude",
        "automation:",
        "automation:Gate",
    ] {
        assert!(insert(bad, None).await.is_err(), "{bad} must be refused");
    }
    for bad in ["automation:rule", "rule:", "automation:rule:x:"] {
        assert!(
            insert("automation:x", Some(bad)).await.is_err(),
            "signs_for {bad} must be refused"
        );
    }
    insert("automation:a", Some("automation:rule:"))
        .await
        .expect("a family declared once");
    assert!(
        insert("automation:b", Some("automation:rule:"))
            .await
            .is_err(),
        "a family signs as ONE automation"
    );
    // The control: a well-shaped row lands, so the refusals above are
    // the CHECKs, not a table that refuses everything.
    insert("automation:gate-runner", None)
        .await
        .expect("a well-shaped row lands");
}

/// The in-memory double refuses what the schema refuses and lands what
/// it lands — the adapters agree on the bundle and on a shared family.
#[tokio::test(flavor = "multi_thread")]
async fn the_adapters_agree_on_the_bundle() {
    let db = TestDb::new().await;
    let pg = PgAgents::new(db.pool.clone());
    let mem = boss_jobs::agents::InMemoryAgents::new();
    let declared = bundle();
    let a = pg.declare_automations(&declared, &stamp()).await.unwrap();
    let b = mem.declare_automations(&declared, &stamp()).await.unwrap();
    assert_eq!(a, b);
    assert_eq!(
        pg.list_automations().await.unwrap(),
        mem.list_automations().await.unwrap()
    );
    let stolen = vec![boss_jobs::agents::AutomationActor {
        id: "automation:thief".into(),
        role: "platform-admin".into(),
        description: "claims the rule family".into(),
        signs_for: Some("automation:rule:".into()),
    }];
    assert!(pg.declare_automations(&stolen, &stamp()).await.is_err());
    assert!(mem.declare_automations(&stolen, &stamp()).await.is_err());
    assert_eq!(
        pg.list_automations().await.unwrap().len(),
        declared.len(),
        "a refused bundle lands nothing"
    );
}
