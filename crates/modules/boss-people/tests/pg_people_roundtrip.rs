//! Integration test that exercises the full PgPeople adapter against
//! a real Postgres database via TestDb.
//!
//! The primary purpose is to validate the TestDb scaffolding itself —
//! if this test runs green, we know `boss-testing::TestDb` correctly
//! creates a fresh database, loads the embedded schema, and tears
//! down on Drop. Every other TestDb-based test in the workspace
//! relies on this infrastructure, so it gets its own sanity check.
//!
//! Secondarily, this is the end-to-end test of the PgPeople adapter's
//! SQL. It catches schema/code drift (missing columns, wrong types,
//! etc.) that pure in-memory tests can't see.

#![cfg(feature = "postgres")]

mod common;

use boss_people::PeopleRepository;
use boss_people::postgres::PgPeople;
use boss_testing::TestDb;
use common::employee_fixture;

#[tokio::test]
async fn pg_people_crud_round_trip() {
    let db = TestDb::new().await;
    let people = PgPeople::new(db.pool.clone());

    // The operator-baseline seed (boss-operator-baseline-seed, from
    // infra/operator-baseline/operator_hires.toml) may materialize
    // platform-operator employees via the audit log, so a fresh deploy
    // never locks the operator team out. The roundtrip assertions below
    // scope by id rather than count to stay resilient to baseline rows.
    let initial = people
        .all_employees()
        .await
        .expect("all_employees on empty db");
    let initial_baseline = initial.len();
    assert!(
        initial.iter().all(|e| e.id != "emp-rt-1"),
        "fresh TestDb should not yet contain the test fixture id"
    );

    // Create
    let fixture = employee_fixture("emp-rt-1");
    let created_id = people
        .create_employee(&fixture)
        .await
        .expect("create_employee should succeed against TestDb");
    assert_eq!(created_id, "emp-rt-1");

    // Read-all sees the new row
    let after_create = people
        .all_employees()
        .await
        .expect("all_employees after create");
    assert_eq!(after_create.len(), initial_baseline + 1);
    let row = after_create
        .iter()
        .find(|e| e.id == "emp-rt-1")
        .expect("created row should be visible");
    assert_eq!(row.name, fixture.name);

    // Read-by-id sees the new row
    let by_id = people
        .employee_by_id("emp-rt-1")
        .await
        .expect("employee_by_id query");
    let found = by_id.expect("employee_by_id should return Some for a just-created row");
    assert_eq!(found.name, fixture.name);
    assert_eq!(found.role, fixture.role);
    assert_eq!(found.department, fixture.department);

    // Read-by-id returns None for an unknown id
    let missing = people
        .employee_by_id("emp-does-not-exist")
        .await
        .expect("employee_by_id query for missing row");
    assert!(missing.is_none());

    // Update
    let mut updated = fixture.clone();
    updated.name = Some("Updated Tester".to_string());
    people
        .update_employee("emp-rt-1", &updated)
        .await
        .expect("update_employee should succeed");
    let after_update = people
        .employee_by_id("emp-rt-1")
        .await
        .expect("employee_by_id after update")
        .expect("row still present after update");
    assert_eq!(after_update.name.as_deref(), Some("Updated Tester"));

    // Delete
    people
        .delete_employee("emp-rt-1")
        .await
        .expect("delete_employee should succeed");
    let after_delete = people
        .employee_by_id("emp-rt-1")
        .await
        .expect("employee_by_id after delete");
    assert!(after_delete.is_none(), "row should be gone after delete");

    // Delete of a missing row should surface NotFound
    let missing_delete = people.delete_employee("emp-ghost").await;
    assert!(
        matches!(missing_delete, Err(boss_people::PeopleError::NotFound(_))),
        "delete of unknown employee should be NotFound, got {missing_delete:?}"
    );
}

#[tokio::test]
async fn test_db_instances_are_isolated() {
    // Prove TestDb hands out independent databases: two instances
    // created in parallel should not see each other's writes.
    let db_a = TestDb::new().await;
    let db_b = TestDb::new().await;
    assert_ne!(
        db_a.name(),
        db_b.name(),
        "TestDb should use distinct database names"
    );

    let people_a = PgPeople::new(db_a.pool.clone());
    let people_b = PgPeople::new(db_b.pool.clone());

    let baseline_a = people_a
        .all_employees()
        .await
        .expect("read db_a baseline")
        .len();
    let baseline_b = people_b
        .all_employees()
        .await
        .expect("read db_b baseline")
        .len();

    people_a
        .create_employee(&employee_fixture("emp-a"))
        .await
        .expect("create in db_a");

    let a_rows = people_a.all_employees().await.expect("read db_a");
    let b_rows = people_b.all_employees().await.expect("read db_b");

    // the operator-baseline seed materializes the same baseline into
    // both DBs; isolation means db_a sees its own write on top of that
    // baseline while db_b's count stays unchanged.
    assert_eq!(a_rows.len(), baseline_a + 1, "db_a should see its own row");
    assert_eq!(b_rows.len(), baseline_b, "db_b must not see db_a's writes");
    assert!(
        a_rows.iter().any(|e| e.id == "emp-a"),
        "db_a should contain emp-a"
    );
    assert!(
        b_rows.iter().all(|e| e.id != "emp-a"),
        "db_b should not contain emp-a"
    );
}

#[tokio::test]
async fn skills_and_certifications_survive_create_and_fetch() {
    let db = TestDb::new().await;
    let people = PgPeople::new(db.pool.clone());

    let mut fixture = employee_fixture("emp-skills-1");
    fixture.skills = vec![
        "network-calibration".to_string(),
        "handpiece-rebuild".to_string(),
        "rf-troubleshooting".to_string(),
    ];
    fixture.certifications = vec![
        boss_people::Certification {
            name: "BICSI Network Cabling Specialist".to_string(),
            issuing_body: "BICSI".to_string(),
            issued_on: chrono::NaiveDate::from_ymd_opt(2024, 3, 15).unwrap(),
            expires_on: Some(chrono::NaiveDate::from_ymd_opt(2027, 3, 15).unwrap()),
        },
        boss_people::Certification {
            name: "OSHA 10-Hour General Industry".to_string(),
            issuing_body: "OSHA".to_string(),
            issued_on: chrono::NaiveDate::from_ymd_opt(2023, 9, 1).unwrap(),
            expires_on: None,
        },
    ];

    people
        .create_employee(&fixture)
        .await
        .expect("create with skills + certs");

    let fetched = people
        .employee_by_id("emp-skills-1")
        .await
        .expect("fetch query")
        .expect("row should exist");

    assert_eq!(fetched.skills.len(), 3);
    assert!(fetched.skills.contains(&"network-calibration".to_string()));
    assert!(fetched.skills.contains(&"handpiece-rebuild".to_string()));
    assert!(fetched.skills.contains(&"rf-troubleshooting".to_string()));

    assert_eq!(fetched.certifications.len(), 2);
    let lso = fetched
        .certifications
        .iter()
        .find(|c| c.name.contains("Network Cabling"))
        .expect("LSO cert should round-trip");
    assert_eq!(lso.issuing_body, "BICSI");
    assert_eq!(
        lso.expires_on,
        Some(chrono::NaiveDate::from_ymd_opt(2027, 3, 15).unwrap())
    );

    let osha = fetched
        .certifications
        .iter()
        .find(|c| c.name.contains("OSHA"))
        .expect("OSHA cert should round-trip");
    assert_eq!(osha.expires_on, None);
}
