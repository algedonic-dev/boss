//! An employee's `department` is a row of the DEPARTMENTS REGISTRY, not
//! an `employee` department Class (backlog c87e3d6d, decided
//! 2026-09-27 on page audit 9f7ba57d).
//!
//! Run through the adapter the service binary wires
//! (`PgPeople::with_registries`) against the real schema, so the
//! question is asked of the `departments` table the jobs API serves as
//! `GET /api/departments` — the one every protocol reader uses. The
//! Class drawer here holds `operations` as a department Class, the
//! shape the live instance had that day: it no longer admits a row.

use std::sync::Arc;

use boss_classes_client::FakeClassesClient;
use boss_core::primitives::Class;
use boss_locations_client::FakeLocationsClient;
use boss_people::postgres::PgPeople;
use boss_people::{Employee, PeopleError, PeopleRepository};
use boss_testing::TestDb;

fn in_department(id: &str, department: &str) -> Employee {
    Employee {
        id: id.into(),
        name: Some(id.into()),
        email: None,
        role: None,
        department: Some(department.into()),
        skill_level: None,
        skills: vec![],
        hire_date: None,
        location: None,
        manager_id: None,
        employment_type: None,
        status: None,
        certifications: vec![],
        annual_salary_cents: None,
    }
}

/// The drawer as it stood live on 2026-09-27: `operations` a department
/// Class, `executive` not one.
fn drawer() -> FakeClassesClient {
    FakeClassesClient::with_classes(vec![Class {
        subject_kind: "employee".into(),
        code: "operations".into(),
        display_name: "Operations / IT".into(),
        parent_code: None,
        member_attribute: Some("department".into()),
        metadata: serde_json::Value::Null,
        sort_order: 80,
        retired_at: None,
    }])
}

async fn people(db: &TestDb) -> PgPeople {
    PgPeople::with_registries(
        db.pool.clone(),
        Arc::new(drawer()),
        Arc::new(FakeLocationsClient::permissive()),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_registry_department_is_admitted_though_no_class_names_it() {
    let db = TestDb::new().await;
    let people = people(&db).await;
    people
        .create_employee(&in_department("emp-dept-exec", "executive"))
        .await
        .expect("`executive` is a departments row, so the write lands");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_department_class_the_registry_lacks_is_refused_by_name() {
    let db = TestDb::new().await;
    let people = people(&db).await;
    match people
        .create_employee(&in_department("emp-dept-ops", "operations"))
        .await
    {
        Err(PeopleError::Conflict(msg)) => assert!(
            msg.contains("`operations`") && msg.contains("departments registry"),
            "{msg}"
        ),
        other => panic!("`operations` is only a Class, so it must be refused: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retired_department_admits_no_one() {
    let db = TestDb::new().await;
    sqlx::query("UPDATE departments SET retired_at = NOW() WHERE id = 'qa'")
        .execute(&db.pool)
        .await
        .expect("retire qa");
    let people = people(&db).await;
    assert!(
        matches!(
            people
                .create_employee(&in_department("emp-dept-qa", "qa"))
                .await,
            Err(PeopleError::Conflict(_))
        ),
        "a withdrawn department is not one an employee can join"
    );
}
