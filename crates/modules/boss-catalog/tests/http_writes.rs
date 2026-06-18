//! HTTP-level write path tests for the knowledge-base service.
//!
//! Each test verifies one business contract via the actual HTTP router.
//! Test names describe the expected behavior so failures point at the
//! exact rule that was violated.

mod common;

use axum::http::StatusCode;
use boss_testing::TestRequest;
use common::{KbTestApp, model_fixture};

// ---------------------------------------------------------------------------
// POST /api/catalog/models — create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_model_returns_201_on_valid_input() {
    let app = KbTestApp::new();
    let model = model_fixture("Boss-TEST-CREATE-1");

    let resp = TestRequest::post("/api/catalog/models")
        .json(&model)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_model_emits_kb_model_created_event() {
    let app = KbTestApp::new();
    let model = model_fixture("Boss-TEST-EVENT-1");

    TestRequest::post("/api/catalog/models")
        .json(&model)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let event = app.bus.assert_event_emitted("kb.model.created");
    assert_eq!(
        event.payload.get("sku").and_then(|v| v.as_str()),
        Some("Boss-TEST-EVENT-1"),
        "expected event payload to include the created SKU"
    );
}

#[tokio::test]
async fn post_duplicate_model_returns_409_conflict() {
    let model = model_fixture("Boss-TEST-DUP-1");
    let app = KbTestApp::with_models(vec![model.clone()]);

    let resp = TestRequest::post("/api/catalog/models")
        .json(&model)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn post_duplicate_model_does_not_emit_event() {
    let model = model_fixture("Boss-TEST-DUP-2");
    let app = KbTestApp::with_models(vec![model.clone()]);

    TestRequest::post("/api/catalog/models")
        .json(&model)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CONFLICT);

    app.bus.assert_event_not_emitted("kb.model.created");
}

#[tokio::test]
async fn post_model_with_invalid_json_returns_4xx() {
    let app = KbTestApp::new();

    let resp = TestRequest::post("/api/catalog/models")
        .raw_body("{not valid json")
        .send(&app.router)
        .await;

    assert!(
        resp.status.is_client_error(),
        "expected 4xx for malformed JSON, got {}",
        resp.status,
    );
}

// ---------------------------------------------------------------------------
// PUT /api/catalog/models/{sku} — update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_existing_model_returns_204_no_content() {
    let model = model_fixture("Boss-TEST-UPD-1");
    let app = KbTestApp::with_models(vec![model.clone()]);

    let mut updated = model.clone();
    updated.name = "Updated Test Model".to_string();

    let resp = TestRequest::put("/api/catalog/models/Boss-TEST-UPD-1")
        .json(&updated)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn put_existing_model_emits_updated_event() {
    let model = model_fixture("Boss-TEST-UPD-2");
    let app = KbTestApp::with_models(vec![model.clone()]);

    TestRequest::put("/api/catalog/models/Boss-TEST-UPD-2")
        .json(&model)
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    app.bus.assert_event_emitted("kb.model.updated");
}

#[tokio::test]
async fn put_nonexistent_model_returns_404_not_found() {
    let app = KbTestApp::new();
    let model = model_fixture("Boss-TEST-MISSING");

    let resp = TestRequest::put("/api/catalog/models/Boss-TEST-MISSING")
        .json(&model)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn put_nonexistent_model_does_not_emit_event() {
    let app = KbTestApp::new();
    let model = model_fixture("Boss-TEST-MISSING-2");

    TestRequest::put("/api/catalog/models/Boss-TEST-MISSING-2")
        .json(&model)
        .send(&app.router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.bus.assert_event_not_emitted("kb.model.updated");
}

// ---------------------------------------------------------------------------
// DELETE /api/catalog/models/{sku} — delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_existing_model_returns_204_no_content() {
    let model = model_fixture("Boss-TEST-DEL-1");
    let app = KbTestApp::with_models(vec![model]);

    let resp = TestRequest::delete("/api/catalog/models/Boss-TEST-DEL-1")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_existing_model_emits_deleted_event() {
    let model = model_fixture("Boss-TEST-DEL-2");
    let app = KbTestApp::with_models(vec![model]);

    TestRequest::delete("/api/catalog/models/Boss-TEST-DEL-2")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.bus.assert_event_emitted("kb.model.deleted");
    assert_eq!(
        event.payload.get("sku").and_then(|v| v.as_str()),
        Some("Boss-TEST-DEL-2"),
    );
}

#[tokio::test]
async fn delete_nonexistent_model_returns_404_not_found() {
    let app = KbTestApp::new();

    let resp = TestRequest::delete("/api/catalog/models/Boss-TEST-MISSING")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_then_get_returns_404() {
    let model = model_fixture("Boss-TEST-DEL-GET-1");
    let app = KbTestApp::with_models(vec![model]);

    TestRequest::delete("/api/catalog/models/Boss-TEST-DEL-GET-1")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let resp = TestRequest::get("/api/catalog/models/Boss-TEST-DEL-GET-1")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Idempotency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_then_get_returns_same_model() {
    let app = KbTestApp::new();
    let model = model_fixture("Boss-TEST-IDEMP-1");

    TestRequest::post("/api/catalog/models")
        .json(&model)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let resp = TestRequest::get("/api/catalog/models/Boss-TEST-IDEMP-1")
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let fetched: boss_catalog::types::AssetModel = resp.assert_json();
    assert_eq!(fetched.sku, model.sku);
    assert_eq!(fetched.name, model.name);
    assert_eq!(fetched.manufacturer, model.manufacturer);
}
