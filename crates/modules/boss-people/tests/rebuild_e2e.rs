//! End-to-end: drive people writes through PgPeople +
//! PgAuditWriter, snapshot all three projections, drop, rebuild
//! from `audit_log`, assert exact match.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_people::PgPeople;
use boss_people::http::{PeopleApiState, router};
use boss_people::rebuild_people;
use boss_people::types::*;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct EmployeeRow {
    id: String,
    name: String,
    email: String,
    role: String,
    department: String,
    skill_level: Option<i16>,
    hire_date: NaiveDate,
    location: String,
    manager_id: Option<String>,
    employment_type: String,
    status: String,
    annual_salary_cents: Option<i64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct SkillRow {
    employee_id: String,
    skill: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct CertRow {
    employee_id: String,
    name: String,
    issuing_body: String,
    issued_on: NaiveDate,
    expires_on: Option<NaiveDate>,
}

async fn snapshot_employees(pool: &PgPool) -> Vec<EmployeeRow> {
    sqlx::query_as("SELECT id, name, email, role, department, skill_level, hire_date, location, manager_id, employment_type, status, annual_salary_cents, created_at, updated_at FROM employees ORDER BY id")
        .fetch_all(pool).await.unwrap()
}
async fn snapshot_skills(pool: &PgPool) -> Vec<SkillRow> {
    sqlx::query_as("SELECT employee_id, skill FROM employee_skills ORDER BY employee_id, skill")
        .fetch_all(pool)
        .await
        .unwrap()
}
async fn snapshot_certs(pool: &PgPool) -> Vec<CertRow> {
    sqlx::query_as("SELECT employee_id, name, issuing_body, issued_on, expires_on FROM employee_certifications ORDER BY employee_id, name")
        .fetch_all(pool).await.unwrap()
}

async fn seed_location(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone, created_at) \
         VALUES ('loc-test', 'Test Location', 'office', 'America/Chicago', NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
}

fn build_app(pool: PgPool) -> Router {
    let people = Arc::new(PgPeople::new(pool.clone()));
    let publisher = DomainPublisher::new(RecordingEventBus::new(), "people")
        .with_audit(Arc::new(PgAuditWriter::new(pool)));
    let state = PeopleApiState {
        people,
        publisher: Some(publisher),
        policy: None,
        subject_kinds: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

fn fixture(
    id: &str,
    role: &str,
    dept: &str,
    skills: Vec<&str>,
    certs: Vec<Certification>,
) -> Employee {
    Employee {
        id: id.into(),
        name: Some(format!("Test {id}")),
        email: Some(format!("{id}@boss.example")),
        role: Some(role.into()),
        department: Some(dept.into()),
        skill_level: Some(3),
        skills: skills.into_iter().map(String::from).collect(),
        hire_date: Some(NaiveDate::from_ymd_opt(2025, 6, 15).unwrap()),
        location: Some("loc-test".into()),
        manager_id: None,
        employment_type: Some("full-time".to_string()),
        status: Some("active".to_string()),
        certifications: certs,
        annual_salary_cents: Some(8_500_000),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_employees_skills_certs() {
    let db = TestDb::new().await;
    seed_location(&db.pool).await;
    let app = build_app(db.pool.clone());

    // 1. Two employees with skills + certs.
    let emp_a = fixture(
        "emp-a-001",
        "service-tech",
        "service",
        vec!["soldering", "diagnostics"],
        vec![Certification {
            name: "Switchsafe-1".into(),
            issuing_body: "OSHA".into(),
            issued_on: NaiveDate::from_ymd_opt(2025, 1, 10).unwrap(),
            expires_on: Some(NaiveDate::from_ymd_opt(2028, 1, 10).unwrap()),
        }],
    );
    let emp_b = fixture(
        "emp-b-002",
        "warehouse-mgr",
        "operations",
        vec!["forklift"],
        vec![],
    );
    for emp in [&emp_a, &emp_b] {
        TestRequest::post("/api/people")
            .json(emp)
            .send(&app)
            .await
            .assert_status(StatusCode::CREATED);
    }

    // 2. Update emp_a — add a skill, revise role.
    let mut emp_a_updated = emp_a.clone();
    emp_a_updated.role = Some("senior-service-tech".into());
    emp_a_updated.skills = vec!["soldering".into(), "diagnostics".into(), "training".into()];
    TestRequest::put(format!("/api/people/{}", emp_a.id))
        .json(&emp_a_updated)
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // 3. Snapshot.
    let employees_before = snapshot_employees(&db.pool).await;
    let skills_before = snapshot_skills(&db.pool).await;
    let certs_before = snapshot_certs(&db.pool).await;

    // The schema seeds emp-cto/coo/owner; filter to just our test ids.
    let mine: Vec<&EmployeeRow> = employees_before
        .iter()
        .filter(|e| e.id.starts_with("emp-a-") || e.id.starts_with("emp-b-"))
        .collect();
    assert_eq!(mine.len(), 2);
    let my_skills: Vec<&SkillRow> = skills_before
        .iter()
        .filter(|s| s.employee_id.starts_with("emp-a-") || s.employee_id.starts_with("emp-b-"))
        .collect();
    assert_eq!(my_skills.len(), 4, "emp-a 3 skills + emp-b 1");

    // 4. Wipe + rebuild. Keeps the seed employees (they have no
    //    audit_log events, so rebuild loses them — that's expected
    //    for the seed, which is out-of-band data this rebuild
    //    doesn't try to recreate). Filter both before+after to our
    //    test ids for equality.
    sqlx::query("DELETE FROM employee_skills")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM employee_certifications")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM employees WHERE id LIKE 'emp-a-%' OR id LIKE 'emp-b-%'")
        .execute(&db.pool)
        .await
        .unwrap();

    // We can't run a full rebuild_people because it'd wipe the
    // platform-operator seed rows (emp-cto/emp-coo/emp-owner) that
    // have no events. For the equality test, hand-roll a slice.
    sqlx::query("DELETE FROM employees WHERE id LIKE 'emp-a-%' OR id LIKE 'emp-b-%'")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = rebuild_people(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.employees_upserted, 3, "2 created + 1 updated");

    // 5. Reconstructed projections must match originals exactly
    //    (filtered to our test rows).
    let employees_after = snapshot_employees(&db.pool).await;
    let skills_after = snapshot_skills(&db.pool).await;
    let certs_after = snapshot_certs(&db.pool).await;
    let mine_after: Vec<&EmployeeRow> = employees_after
        .iter()
        .filter(|e| e.id.starts_with("emp-a-") || e.id.starts_with("emp-b-"))
        .collect();
    assert_eq!(
        mine.iter().collect::<Vec<_>>(),
        mine_after.iter().collect::<Vec<_>>(),
        "employees mismatch"
    );
    let my_skills_after: Vec<&SkillRow> = skills_after
        .iter()
        .filter(|s| s.employee_id.starts_with("emp-a-") || s.employee_id.starts_with("emp-b-"))
        .collect();
    assert_eq!(
        my_skills.iter().collect::<Vec<_>>(),
        my_skills_after.iter().collect::<Vec<_>>(),
        "employee_skills mismatch"
    );
    let my_certs: Vec<&CertRow> = certs_before
        .iter()
        .filter(|c| c.employee_id.starts_with("emp-a-") || c.employee_id.starts_with("emp-b-"))
        .collect();
    let my_certs_after: Vec<&CertRow> = certs_after
        .iter()
        .filter(|c| c.employee_id.starts_with("emp-a-") || c.employee_id.starts_with("emp-b-"))
        .collect();
    assert_eq!(my_certs, my_certs_after, "employee_certifications mismatch");
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_handles_employee_delete() {
    let db = TestDb::new().await;
    seed_location(&db.pool).await;
    let app = build_app(db.pool.clone());

    let emp = fixture("emp-doomed", "service-tech", "service", vec![], vec![]);
    TestRequest::post("/api/people")
        .json(&emp)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::delete(format!("/api/people/{}", emp.id))
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let report = rebuild_people(&db.pool).await.unwrap();
    assert!(report.employees_upserted >= 1);
    assert!(report.employees_deleted >= 1);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM employees WHERE id = 'emp-doomed'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0, "rebuild should reproduce post-delete state");
}
