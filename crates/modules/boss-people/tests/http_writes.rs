//! HTTP-level write path tests for the people service.
//!
//! Each test verifies one business contract via the actual HTTP router.

mod common;

use axum::http::StatusCode;
use boss_testing::TestRequest;
use common::{PeopleTestApp, employee_fixture};

// ---------------------------------------------------------------------------
// POST /api/people — create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_employee_returns_201_on_valid_input() {
    let app = PeopleTestApp::new();
    let emp = employee_fixture("emp-create-1");

    let resp = TestRequest::post("/api/people")
        .json(&emp)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_employee_emits_people_employee_created_event() {
    let app = PeopleTestApp::new();
    let emp = employee_fixture("emp-event-1");

    TestRequest::post("/api/people")
        .json(&emp)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let event = app.bus.assert_event_emitted("people.employee.created");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("emp-event-1"),
        "expected event payload to include the created employee ID"
    );
}

#[tokio::test]
async fn post_duplicate_employee_returns_409_conflict() {
    let emp = employee_fixture("emp-dup-1");
    let app = PeopleTestApp::with_employees(vec![emp.clone()]);

    let resp = TestRequest::post("/api/people")
        .json(&emp)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn post_duplicate_employee_does_not_emit_event() {
    let emp = employee_fixture("emp-dup-2");
    let app = PeopleTestApp::with_employees(vec![emp.clone()]);

    TestRequest::post("/api/people")
        .json(&emp)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CONFLICT);

    app.bus.assert_event_not_emitted("people.employee.created");
}

// ---------------------------------------------------------------------------
// PUT /api/people/{id} — update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_existing_employee_returns_204() {
    let emp = employee_fixture("emp-upd-1");
    let app = PeopleTestApp::with_employees(vec![emp.clone()]);

    let mut updated = emp.clone();
    updated.name = Some("Updated Name".to_string());

    let resp = TestRequest::put("/api/people/emp-upd-1")
        .json(&updated)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn put_existing_employee_emits_updated_event() {
    let emp = employee_fixture("emp-upd-2");
    let app = PeopleTestApp::with_employees(vec![emp.clone()]);

    TestRequest::put("/api/people/emp-upd-2")
        .json(&emp)
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    app.bus.assert_event_emitted("people.employee.updated");
}

#[tokio::test]
async fn put_nonexistent_employee_returns_404() {
    let app = PeopleTestApp::new();
    let emp = employee_fixture("emp-missing");

    let resp = TestRequest::put("/api/people/emp-missing")
        .json(&emp)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// DELETE /api/people/{id} — delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_existing_employee_returns_204() {
    let emp = employee_fixture("emp-del-1");
    let app = PeopleTestApp::with_employees(vec![emp]);

    let resp = TestRequest::delete("/api/people/emp-del-1")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_existing_employee_emits_deleted_event() {
    let emp = employee_fixture("emp-del-2");
    let app = PeopleTestApp::with_employees(vec![emp]);

    TestRequest::delete("/api/people/emp-del-2")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.bus.assert_event_emitted("people.employee.deleted");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("emp-del-2"),
    );
}

#[tokio::test]
async fn delete_nonexistent_employee_returns_404() {
    let app = PeopleTestApp::new();

    let resp = TestRequest::delete("/api/people/emp-missing")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_employee_after_create_returns_same_data() {
    let app = PeopleTestApp::new();
    let emp = employee_fixture("emp-roundtrip-1");

    TestRequest::post("/api/people")
        .json(&emp)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let resp = TestRequest::get("/api/people/emp-roundtrip-1")
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let fetched: boss_people::types::Employee = resp.assert_json();
    assert_eq!(fetched.id, emp.id);
    assert_eq!(fetched.name, emp.name);
    assert_eq!(fetched.email, emp.email);
}

#[tokio::test]
async fn get_employee_after_delete_returns_404() {
    let emp = employee_fixture("emp-del-get-1");
    let app = PeopleTestApp::with_employees(vec![emp]);

    TestRequest::delete("/api/people/emp-del-get-1")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let resp = TestRequest::get("/api/people/emp-del-get-1")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}
