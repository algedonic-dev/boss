//! End-to-end test for the people → audit_log chain. See the
//! catalog audit_log_e2e.rs for the rationale.

#![cfg(feature = "postgres")]

mod common;

use axum::http::StatusCode;
use boss_testing::{TestDb, TestRequest};
use common::{PeopleTestApp, employee_fixture};

#[tokio::test(flavor = "multi_thread")]
async fn post_employee_lands_in_audit_log() {
    let db = TestDb::new().await;
    let app = PeopleTestApp::with_audit_pool(db.pool.clone());

    let emp = employee_fixture("emp-audit-test");
    TestRequest::post("/api/people")
        .json(&emp)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let row: (String, String) = sqlx::query_as(
        "SELECT source, kind FROM audit_log \
         WHERE kind = 'people.employee.created' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST");

    assert_eq!(row.0, "people");
    assert_eq!(row.1, "people.employee.created");
}
