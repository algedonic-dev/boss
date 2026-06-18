//! HTTP-level write path tests for the shipping service.
//!
//! Each test verifies one business contract via the actual HTTP router.
//! Test names describe the expected behavior so failures point at the
//! exact rule that was violated.

mod common;

use axum::http::StatusCode;
use boss_testing::TestRequest;
use common::{ShippingTestApp, shipment_fixture};

// ---------------------------------------------------------------------------
// POST /api/shipping/shipments — create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_shipment_returns_201_on_valid_input() {
    let app = ShippingTestApp::new();
    let ship = shipment_fixture("ship-create-1");

    let resp = TestRequest::post("/api/shipping/shipments")
        .json(&ship)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_shipment_emits_shipment_created_event() {
    let app = ShippingTestApp::new();
    let ship = shipment_fixture("ship-create-event-1");

    TestRequest::post("/api/shipping/shipments")
        .json(&ship)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let event = app.bus.assert_event_emitted("shipping.shipment.created");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("ship-create-event-1"),
    );
}

#[tokio::test]
async fn post_duplicate_shipment_returns_409_conflict() {
    let ship = shipment_fixture("ship-dup-1");
    let app = ShippingTestApp::with_shipments(vec![ship.clone()]);

    let resp = TestRequest::post("/api/shipping/shipments")
        .json(&ship)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn post_duplicate_shipment_does_not_emit_event() {
    let ship = shipment_fixture("ship-dup-2");
    let app = ShippingTestApp::with_shipments(vec![ship.clone()]);

    TestRequest::post("/api/shipping/shipments")
        .json(&ship)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CONFLICT);

    app.bus
        .assert_event_not_emitted("shipping.shipment.created");
}

#[tokio::test]
async fn post_shipment_with_invalid_json_returns_4xx() {
    let app = ShippingTestApp::new();

    let resp = TestRequest::post("/api/shipping/shipments")
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
// PUT /api/shipping/shipments/{id} — update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_existing_shipment_returns_204_no_content() {
    let ship = shipment_fixture("ship-upd-1");
    let app = ShippingTestApp::with_shipments(vec![ship.clone()]);

    let mut updated = ship.clone();
    updated.origin = "Updated Origin".to_string();

    let resp = TestRequest::put("/api/shipping/shipments/ship-upd-1")
        .json(&updated)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn put_existing_shipment_emits_shipment_updated_event() {
    let ship = shipment_fixture("ship-upd-2");
    let app = ShippingTestApp::with_shipments(vec![ship.clone()]);

    TestRequest::put("/api/shipping/shipments/ship-upd-2")
        .json(&ship)
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.bus.assert_event_emitted("shipping.shipment.updated");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("ship-upd-2"),
    );
}

#[tokio::test]
async fn put_nonexistent_shipment_returns_404_not_found() {
    let app = ShippingTestApp::new();
    let ship = shipment_fixture("ship-missing");

    let resp = TestRequest::put("/api/shipping/shipments/ship-missing")
        .json(&ship)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn put_nonexistent_shipment_does_not_emit_event() {
    let app = ShippingTestApp::new();
    let ship = shipment_fixture("ship-missing-2");

    TestRequest::put("/api/shipping/shipments/ship-missing-2")
        .json(&ship)
        .send(&app.router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.bus
        .assert_event_not_emitted("shipping.shipment.updated");
}

// ---------------------------------------------------------------------------
// DELETE /api/shipping/shipments/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_existing_shipment_returns_204_no_content() {
    let ship = shipment_fixture("ship-del-1");
    let app = ShippingTestApp::with_shipments(vec![ship]);

    let resp = TestRequest::delete("/api/shipping/shipments/ship-del-1")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_existing_shipment_emits_shipment_deleted_event() {
    let ship = shipment_fixture("ship-del-2");
    let app = ShippingTestApp::with_shipments(vec![ship]);

    TestRequest::delete("/api/shipping/shipments/ship-del-2")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.bus.assert_event_emitted("shipping.shipment.deleted");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("ship-del-2"),
    );
}

#[tokio::test]
async fn delete_nonexistent_shipment_returns_404_not_found() {
    let app = ShippingTestApp::new();

    let resp = TestRequest::delete("/api/shipping/shipments/ship-missing")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_nonexistent_shipment_does_not_emit_event() {
    let app = ShippingTestApp::new();

    TestRequest::delete("/api/shipping/shipments/ship-missing-2")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.bus
        .assert_event_not_emitted("shipping.shipment.deleted");
}

#[tokio::test]
async fn delete_then_get_returns_404() {
    let ship = shipment_fixture("ship-del-get-1");
    let app = ShippingTestApp::with_shipments(vec![ship]);

    TestRequest::delete("/api/shipping/shipments/ship-del-get-1")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let resp = TestRequest::get("/api/shipping/shipments/ship-del-get-1")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// POST /api/shipping/shipments/batch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn batch_shipments_returns_success_with_inserted_count() {
    let app = ShippingTestApp::new();
    let batch = vec![
        shipment_fixture("ship-batch-1"),
        shipment_fixture("ship-batch-2"),
        shipment_fixture("ship-batch-3"),
    ];

    let resp = TestRequest::post("/api/shipping/shipments/batch")
        .json(&batch)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json();
    assert_eq!(body.get("inserted").and_then(|v| v.as_u64()), Some(3));
}

// ---------------------------------------------------------------------------
// Idempotency / round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_then_get_returns_same_shipment() {
    let app = ShippingTestApp::new();
    let ship = shipment_fixture("ship-idemp-1");

    TestRequest::post("/api/shipping/shipments")
        .json(&ship)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let resp = TestRequest::get("/api/shipping/shipments/ship-idemp-1")
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let fetched: boss_shipping::types::Shipment = resp.assert_json();
    assert_eq!(fetched.id, ship.id);
    assert_eq!(fetched.origin, ship.origin);
    assert_eq!(fetched.destination, ship.destination);
}
