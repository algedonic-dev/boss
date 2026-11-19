#![cfg(feature = "postgres")]
mod common;

use boss_core::{
    port::EventBus,
    roster_capacity::{ClassRequirement, RosterCapacitySnapshot},
};
use boss_people::{PeopleRepository, PgPeople};
use boss_testing::{RecordingEventBus, TestDb};
use std::sync::Arc;

fn eligibility() -> Vec<ClassRequirement> {
    vec![ClassRequirement {
        attribute: "role".into(),
        codes: vec!["snapshot-fixture-role".into()],
    }]
}

async fn classes(db: &TestDb) {
    sqlx::query("INSERT INTO classes(subject_kind,code,display_name,member_attribute,metadata,sort_order) VALUES ('employee','snapshot-fixture-role','Snapshot fixture','role','{}',0)")
        .execute(&db.pool).await.unwrap();
}

async fn drain(db: &TestDb) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 1000)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_qualified_empty_snapshot_is_zero_and_a_pending_people_fact_is_unknown() {
    let db = TestDb::new().await;
    classes(&db).await;
    let people = PgPeople::new(db.pool.clone());
    let empty = people.snapshot(&eligibility()).await;
    assert!(
        empty.is_ok(),
        "qualified empty roster must be Known(0), not unavailable: {empty:?}"
    );
    assert_eq!(empty.unwrap().count, 0);
    let mut employee = common::employee_fixture("snapshot-private-id");
    employee.role = Some("snapshot-fixture-role".into());
    employee.name = Some("PRIVATE-NAME-SENTINEL".into());
    employee.annual_salary_cents = Some(123456789);
    people.create_employee(&employee).await.unwrap();
    assert!(
        people.snapshot(&eligibility()).await.is_err(),
        "undrained people fact must refuse a qualified count"
    );
    drain(&db).await;
    let known = people.snapshot(&eligibility()).await.unwrap();
    assert_eq!(known.count, 1);
    let newest: Option<i64> =
        sqlx::query_scalar("SELECT MAX(id) FROM audit_log WHERE source='people'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(known.revision, newest);
    let wire = serde_json::to_string(&known).unwrap();
    for private in [
        "snapshot-private-id",
        "PRIVATE-NAME-SENTINEL",
        "123456789",
        "annual_salary",
    ] {
        assert!(
            !wire.contains(private),
            "private profile escaped the aggregate DTO"
        );
    }
    assert_eq!(people.snapshot(&eligibility()).await.unwrap(), known);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_concurrent_people_write_cannot_mix_roster_and_audit_basis() {
    let db = TestDb::new().await;
    classes(&db).await;
    let people = Arc::new(PgPeople::new(db.pool.clone()));
    let before = people.snapshot(&eligibility()).await.unwrap();
    let mut writer = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE audit_log IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *writer)
        .await
        .unwrap();
    let reader = people.clone();
    let reading = tokio::spawn(async move { reader.snapshot(&eligibility()).await });
    // Witness the reader at its real second source, rather than hoping a sleep
    // put it between two reads. Its first snapshot is already established.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND relation='audit_log'::regclass AND mode='AccessShareLock' AND NOT granted)")
                .fetch_one(&db.pool).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("snapshot reached the actual audit lock");
    let mut employee = common::employee_fixture("snapshot-concurrent");
    employee.role = Some("snapshot-fixture-role".into());
    people.create_employee(&employee).await.unwrap();
    // The producer really staged this fact. Complete its audit half in the
    // isolated writer transaction while holding the audit lock; no live DB.
    let copied = sqlx::query("INSERT INTO audit_log(timestamp,source,kind,payload,event_id) SELECT timestamp,source,kind,payload,event_id FROM event_outbox WHERE source='people' AND payload->>'id'='snapshot-concurrent'")
        .execute(&mut *writer).await.unwrap();
    assert_eq!(
        copied.rows_affected(),
        1,
        "the actual producer fact was copied, not an empty query"
    );
    writer.commit().await.unwrap();
    assert_eq!(
        reading.await.unwrap().unwrap(),
        before,
        "RR must return one old snapshot, not old roster/new basis or lag"
    );
    drain(&db).await;
    let after = people.snapshot(&eligibility()).await.unwrap();
    assert_eq!(after.count, before.count + 1);
    assert!(after.revision > before.revision);
}

#[tokio::test]
async fn only_exact_people_outbox_matches_qualify_the_snapshot() {
    let db = TestDb::new().await;
    classes(&db).await;
    let people = PgPeople::new(db.pool.clone());
    // An unrelated producer's pending fact must not make people unavailable.
    sqlx::query("INSERT INTO event_outbox(event_id,timestamp,source,kind,payload) VALUES ($1,now(),'fixture-other','fixture.other','{}')")
        .bind(uuid::Uuid::new_v4()).execute(&db.pool).await.unwrap();
    assert_eq!(people.snapshot(&eligibility()).await.unwrap().count, 0);
    let mut employee = common::employee_fixture("snapshot-mismatch");
    employee.role = Some("snapshot-fixture-role".into());
    people.create_employee(&employee).await.unwrap();
    // Same event identity alone is not drainage: the canonical body must agree.
    sqlx::query("INSERT INTO audit_log(timestamp,source,kind,payload,event_id) SELECT timestamp,source,kind,payload || '{\"fixture_corrupt\":true}'::jsonb,event_id FROM event_outbox WHERE source='people'")
        .execute(&db.pool).await.unwrap();
    let refusal = people.snapshot(&eligibility()).await.unwrap_err();
    assert!(refusal.to_string().contains("outbox undrained"));
}
