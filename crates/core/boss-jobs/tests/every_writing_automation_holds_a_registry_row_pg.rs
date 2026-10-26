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
    // Widens no one. On 2026-10-01 every row carried platform-admin, the
    // role each writer sends. The readers registered on 2026-10-07 (the
    // converge and observer reads, the probe's reader, the sweeps' train
    // read) hold the read role. ca356c79 adds only the existing anonymous
    // default for the previously unnamed ML batch, not a privileged grant.
    // Any role beyond those existing authorities still needs its own change.
    let other: Vec<_> = rows
        .iter()
        .filter(|a| {
            a.role != "platform-admin"
                && a.role != "audit-readonly"
                && a.role != boss_policy_client::User::ANONYMOUS_ROLE
        })
        .map(|a| (&a.id, &a.role))
        .collect();
    assert!(
        other.is_empty(),
        "a row holds a role the bundle has never spelled: {other:?}"
    );
    // And records no tier (backlog f21804d8): the column is a mechanism,
    // and recording one is an owner's act with its own review — no
    // bundle row carries one until that act is made.
    let tiered: Vec<_> = rows
        .iter()
        .filter(|a| a.access_tier.is_some())
        .map(|a| &a.id)
        .collect();
    assert!(tiered.is_empty(), "a bundle row records a tier: {tiered:?}");
    // The family rows stay with their signers, where car 1 put them.
    let families: Vec<_> = rows
        .iter()
        .filter_map(|a| a.signs_for.as_deref().map(|p| (a.id.as_str(), p)))
        .collect();
    assert_eq!(
        families,
        [
            ("automation:dispatcher", "automation:rule:"),
            ("automation:ops-runner", "automation:ops-runner:")
        ]
    );

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
            && declared
                .iter()
                .any(|d| p["id"] == d.id.as_str() && p["role"] == d.role.as_str())),
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

/// A RECORDED TIER ROUND-TRIPS, THE SCHEMA ADMITS ONLY AUDITOR, AND THE
/// RESOLVER READS IT FROM THE ROW (backlog f21804d8, design 600c07d7).
/// A row inserted with `auditor` reads back with it, its fact carries it,
/// and the resolver over the real adapter makes an Auditor reader of the
/// row's own role. `operator` and any other word are refused by the
/// database itself. The seed never stamps a tier over a held row nor
/// revokes one. A revocation in the instance stops a re-resolution; a
/// value the CHECK did not cover (the constraint dropped, as a future
/// schema might) is refused by the adapter rather than read as no tier.
#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_tier_round_trips_and_only_auditor_is_recordable() {
    use boss_jobs::agents::{RecordedTier, reresolve_recorded_reader, resolve_recorded_reader};
    use boss_policy_client::AccessTier;
    let db = TestDb::new().await;
    let pg = PgAgents::new(db.pool.clone());
    let observer = boss_jobs::agents::AutomationActor {
        id: "automation:yard-mover".into(),
        role: "audit-readonly".into(),
        description: "the movement observer".into(),
        signs_for: None,
        access_tier: Some(RecordedTier::Auditor),
    };
    let untiered = boss_jobs::agents::AutomationActor {
        id: "automation:gate-runner".into(),
        access_tier: None,
        ..observer.clone()
    };
    let out = pg
        .declare_automations(&[observer.clone(), untiered.clone()], &stamp())
        .await
        .expect("declares");
    assert_eq!(out.inserted.len(), 2, "{out}");
    assert_eq!(
        pg.list_automations().await.unwrap(),
        vec![untiered.clone(), observer.clone()]
    );
    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind = $1 AND payload->>'id' = $2",
    )
    .bind(AUTOMATION_DECLARED)
    .bind("automation:yard-mover")
    .fetch_one(&db.pool)
    .await
    .expect("the declared fact");
    assert_eq!(payload["access_tier"], "auditor", "{payload}");

    let reader = resolve_recorded_reader(&pg, "automation:yard-mover")
        .await
        .expect("the recorded reader");
    assert_eq!(reader.role, "audit-readonly");
    assert_eq!(reader.access_tier, AccessTier::Auditor);
    let why = resolve_recorded_reader(&pg, "automation:gate-runner")
        .await
        .unwrap_err();
    assert!(why.contains("records no access tier"), "{why}");

    // The database refuses every tier but auditor.
    for bad in ["operator", "user", "Auditor", ""] {
        let refused = sqlx::query(
            "INSERT INTO automation_actors (id, role, description, access_tier) \
             VALUES ('automation:x', 'audit-readonly', 'x', $1)",
        )
        .bind(bad)
        .execute(&db.pool)
        .await;
        assert!(refused.is_err(), "{bad:?} must be refused");
    }

    // The seed keeps the instance's tier both ways, naming it.
    let flipped = [
        boss_jobs::agents::AutomationActor {
            access_tier: None,
            ..observer.clone()
        },
        boss_jobs::agents::AutomationActor {
            access_tier: Some(RecordedTier::Auditor),
            ..untiered.clone()
        },
    ];
    let kept = pg.declare_automations(&flipped, &stamp()).await.unwrap();
    assert_eq!(kept.kept.len(), 2, "{kept}");
    assert!(kept.kept.iter().all(|k| k.differs == ["access_tier"]));
    assert_eq!(
        pg.list_automations().await.unwrap(),
        vec![untiered, observer],
        "the instance is the truth"
    );

    // A revocation in the instance stops the reader on its next ask.
    sqlx::query("UPDATE automation_actors SET access_tier = NULL WHERE id = $1")
        .bind("automation:yard-mover")
        .execute(&db.pool)
        .await
        .expect("revoke");
    let why = reresolve_recorded_reader(&pg, &reader).await.unwrap_err();
    assert!(why.contains("records no access tier"), "{why}");

    // A value outside the set this build knows is refused, not dropped.
    sqlx::query(
        "ALTER TABLE automation_actors DROP CONSTRAINT automation_actors_access_tier_check",
    )
    .execute(&db.pool)
    .await
    .expect("drop the CHECK for the adapter's own refusal");
    sqlx::query("UPDATE automation_actors SET access_tier = 'operator' WHERE id = $1")
        .bind("automation:yard-mover")
        .execute(&db.pool)
        .await
        .expect("an unknown value");
    let why = pg.list_automations().await.unwrap_err().to_string();
    assert!(
        why.contains("automation:yard-mover") && why.contains("operator"),
        "{why}"
    );
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
        access_tier: None,
    }];
    assert!(pg.declare_automations(&stolen, &stamp()).await.is_err());
    assert!(mem.declare_automations(&stolen, &stamp()).await.is_err());
    assert_eq!(
        pg.list_automations().await.unwrap().len(),
        declared.len(),
        "a refused bundle lands nothing"
    );
}
