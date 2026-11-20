mod common;
use boss_core::{actor::ActorId, port::EventBus};
use boss_jobs::pools::{
    InMemoryPools, PgPools, PoolCapacity, PoolRegistry, PoolSpec, refresh_roster,
};
use boss_people::{PeopleRepository, PgPeople};
use boss_testing::{RecordingEventBus, TestDb};
use serde_json::json;
use std::sync::Arc;

async fn contract(registry: &dyn PoolRegistry, db: &TestDb) {
    sqlx::query("INSERT INTO classes(subject_kind,code,display_name,member_attribute,metadata,sort_order) VALUES ('employee','publication-fixture-role','Publication fixture','role','{}',0)").execute(&db.pool).await.unwrap();
    let people = PgPeople::new(db.pool.clone());
    let actor = ActorId::automation("roster-publication-fixture");
    let now = chrono::Utc::now();
    let declaration: PoolSpec = serde_json::from_value(json!({"name":"people:publication-fixture-role","version":1,"status":"active","unit":"people","kind":"flow","capacity":{"state":"unknown"},"roster":{"eligibility":[{"attribute":"role","codes":["publication-fixture-role"]}],"basis":null}})).unwrap();
    let initial = registry
        .publish_declared(declaration, &actor, now)
        .await
        .unwrap();
    let empty = refresh_roster(registry, &people, &initial, &actor, now)
        .await
        .unwrap();
    assert_eq!(
        empty.capacity,
        PoolCapacity::Configured { amount: 0.into() }
    );
    let mut employee = common::employee_fixture("private-publication-id");
    employee.role = Some("publication-fixture-role".into());
    employee.name = Some("PRIVATE-PUBLICATION-NAME".into());
    employee.annual_salary_cents = Some(999888777);
    people.create_employee(&employee).await.unwrap();
    let unknown = refresh_roster(registry, &people, &empty, &actor, now)
        .await
        .unwrap();
    assert_eq!(unknown.capacity, PoolCapacity::Unknown {});
    assert_eq!(
        unknown.roster.as_ref().unwrap().basis,
        empty.roster.as_ref().unwrap().basis
    );
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 1000)
        .await
        .unwrap();
    let known = refresh_roster(registry, &people, &unknown, &actor, now)
        .await
        .unwrap();
    assert_eq!(
        known.capacity,
        PoolCapacity::Configured { amount: 1.into() }
    );
    let basis = known.roster.as_ref().unwrap().basis.as_ref().unwrap();
    assert_eq!(basis.count, Some(1));
    assert_eq!(
        basis.revision,
        sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(id) FROM audit_log WHERE source='people'")
            .fetch_one(&db.pool)
            .await
            .unwrap()
    );
    assert_eq!(
        refresh_roster(registry, &people, &known, &actor, now)
            .await
            .unwrap(),
        known,
        "same revision/count creates no new version"
    );
    assert!(
        refresh_roster(registry, &people, &empty, &actor, now)
            .await
            .is_err(),
        "stale reader cannot overwrite a newer observation"
    );
    employee.name = Some("PRIVATE-NAME-CHANGED".into());
    people
        .update_employee(&employee.id, &employee)
        .await
        .unwrap();
    let pending = refresh_roster(registry, &people, &known, &actor, now)
        .await
        .unwrap();
    assert_eq!(
        pending.capacity,
        PoolCapacity::Unknown {},
        "same audit N with a newly staged fact must not be ignored as idempotent"
    );
    assert_eq!(
        pending.roster.as_ref().unwrap().basis,
        known.roster.as_ref().unwrap().basis
    );
    assert_eq!(
        refresh_roster(registry, &people, &pending, &actor, now)
            .await
            .unwrap(),
        pending,
        "repeated same Unknown is idempotent only under current declaration"
    );
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 1000)
        .await
        .unwrap();
    let newest = refresh_roster(registry, &people, &pending, &actor, now)
        .await
        .unwrap();
    assert_eq!(
        newest.capacity,
        PoolCapacity::Configured { amount: 1.into() }
    );
    assert!(
        newest
            .roster
            .as_ref()
            .unwrap()
            .basis
            .as_ref()
            .unwrap()
            .revision
            > basis.revision
    );
    assert!(
        registry
            .observe_roster(
                &pending,
                Err(
                    boss_core::roster_capacity::RosterSnapshotError::Unavailable(
                        "people outbox undrained".into()
                    )
                ),
                &actor,
                now
            )
            .await
            .is_err(),
        "delayed old Unknown cannot overwrite fresh Known"
    );
    let wire = serde_json::to_string(&registry.list_versions(&known.name).await.unwrap()).unwrap();
    for private in [
        "private-publication-id",
        "PRIVATE-PUBLICATION-NAME",
        "999888777",
        "salary",
    ] {
        assert!(!wire.contains(private));
    }
    assert_eq!(registry.list_versions(&known.name).await.unwrap().len(), 6);
    // Eligibility is registry data. Retiring a configured Class cannot leave
    // the last Known number admissible merely because no people fact changed.
    sqlx::query("UPDATE classes SET retired_at=now() WHERE subject_kind='employee' AND code='publication-fixture-role'").execute(&db.pool).await.unwrap();
    let unavailable = refresh_roster(registry, &people, &newest, &actor, now).await;
    assert!(
        unavailable.is_ok(),
        "unqualifiable current Class predicate must record Unknown rather than retain stale Known: {unavailable:?}"
    );
    assert_eq!(unavailable.unwrap().capacity, PoolCapacity::Unknown {});
}

#[tokio::test]
async fn people_snapshot_publishes_through_the_memory_pool_owner() {
    let db = TestDb::new().await;
    contract(&InMemoryPools::default(), &db).await;
}
#[tokio::test]
async fn people_snapshot_publishes_through_the_pg_pool_owner() {
    let db = TestDb::new().await;
    contract(&PgPools::new(db.pool.clone()), &db).await;
}

/// Fire the people door the dispatcher's clock rule fires, as that rule's
/// actor, and read the pool's active row back.
async fn fire_the_door(db: &TestDb, registry: Arc<PgPools>, name: &str) -> PoolSpec {
    use boss_people::roster_refresh::{RosterRefreshState, roster_refresh_router};
    use tower::ServiceExt;
    let response = roster_refresh_router(RosterRefreshState {
        registry: registry.clone(),
        source: Arc::new(PgPeople::new(db.pool.clone())),
        policy: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    })
    .oneshot(
        axum::http::Request::post("/api/people/roster-capacity/refresh")
            .header(
                "x-boss-user",
                r#"{"id":"rule:roster-pools-are-observed-every-five-minutes","role":"platform-admin","access_tier":"operator"}"#,
            )
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    registry
        .list_active()
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.name == name)
        .unwrap()
}

/// Hire one employee into `role` and drain the people outbox, as the
/// relay would, so the snapshot qualifies.
async fn hire(db: &TestDb, people: &PgPeople, id: &str, role: &str) {
    let mut employee = common::employee_fixture(id);
    employee.role = Some(role.into());
    people.create_employee(&employee).await.unwrap();
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 1000)
        .await
        .unwrap();
}

/// The production caller (backlog a6cb448a): the door the dispatcher's
/// clock rule fires observes a roster pool through the real snapshot and
/// the real owner, and a hire moves its capacity. `unknown` is what every
/// roster pool read before this door existed.
#[tokio::test]
async fn the_people_door_moves_a_roster_pool_when_the_roster_changes() {
    let db = TestDb::new().await;
    sqlx::query("INSERT INTO classes(subject_kind,code,display_name,member_attribute,metadata,sort_order) VALUES ('employee','door-fixture-role','Door fixture','role','{}',0)").execute(&db.pool).await.unwrap();
    let registry = Arc::new(PgPools::new(db.pool.clone()));
    let declaration: PoolSpec = serde_json::from_value(json!({"name":"people:door-fixture-role","version":1,"status":"active","unit":"people","kind":"flow","capacity":{"state":"unknown"},"roster":{"eligibility":[{"attribute":"role","codes":["door-fixture-role"]}],"basis":null}})).unwrap();
    let name = "people:door-fixture-role";
    registry
        .publish_declared(
            declaration,
            &ActorId::automation("roster-door-fixture"),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let people = PgPeople::new(db.pool.clone());

    hire(&db, &people, "door-fixture-first", "door-fixture-role").await;
    let first = fire_the_door(&db, registry.clone(), name).await;
    assert_eq!(
        first.capacity,
        PoolCapacity::Configured { amount: 1.into() }
    );
    let basis = first.roster.as_ref().unwrap().basis.as_ref().unwrap();
    assert!(basis.as_of.is_some() && basis.revision.is_some());

    hire(&db, &people, "door-fixture-second", "door-fixture-role").await;
    let second = fire_the_door(&db, registry, name).await;
    assert_eq!(
        second.capacity,
        PoolCapacity::Configured { amount: 2.into() }
    );
    assert_eq!(second.version, first.version + 1);
}
