//! HTTP-level write path tests for the inventory service.
//!
//! Each test verifies one business contract via the actual HTTP router.
//! Test names describe the expected behavior so failures point at the
//! exact rule that was violated.

mod common;

use axum::http::StatusCode;
use boss_testing::TestRequest;
use common::{InventoryTestApp, purchase_order_fixture, vendor_fixture};
use serde_json::json;

// ---------------------------------------------------------------------------
// POST /api/inventory/vendors — create
// ---------------------------------------------------------------------------

fn create_vendor_body() -> serde_json::Value {
    json!({
        "name": "Acme Parts Co",
        "contact_name": "Jane Tester",
        "contact_email": "jane@acme.example",
        "city": "Austin",
        "state": "TX",
        "lead_time_days": 14,
        "payment_terms": "Net 30",
        "category": "parts",
    })
}

fn create_vendor_body_with_id(id: &str) -> serde_json::Value {
    let mut body = create_vendor_body();
    body.as_object_mut()
        .unwrap()
        .insert("id".to_string(), json!(id));
    body
}

#[tokio::test]
async fn post_vendor_returns_201_on_valid_input() {
    let app = InventoryTestApp::new();

    let resp = TestRequest::post("/api/inventory/vendors")
        .json(&create_vendor_body())
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_vendor_emits_vendor_created_event() {
    let app = InventoryTestApp::new();

    TestRequest::post("/api/inventory/vendors")
        .json(&create_vendor_body())
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let event = app.assert_event_recorded("inventory.vendor.created");
    assert!(
        event
            .payload
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .starts_with("VND-"),
        "expected event payload to include a generated VND- id"
    );
}

#[tokio::test]
async fn post_vendor_with_invalid_json_returns_4xx() {
    let app = InventoryTestApp::new();

    let resp = TestRequest::post("/api/inventory/vendors")
        .raw_body("{not valid json")
        .send(&app.router)
        .await;

    assert!(
        resp.status.is_client_error(),
        "expected 4xx for malformed JSON, got {}",
        resp.status,
    );
}

#[tokio::test]
async fn post_vendor_with_client_supplied_id_uses_it() {
    let app = InventoryTestApp::new();

    TestRequest::post("/api/inventory/vendors")
        .json(&create_vendor_body_with_id("VND-TEST01"))
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let event = app.assert_event_recorded("inventory.vendor.created");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("VND-TEST01"),
    );
}

#[tokio::test]
async fn post_vendor_duplicate_id_returns_409() {
    let app = InventoryTestApp::new();

    TestRequest::post("/api/inventory/vendors")
        .json(&create_vendor_body_with_id("VND-DUP01"))
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let resp = TestRequest::post("/api/inventory/vendors")
        .json(&create_vendor_body_with_id("VND-DUP01"))
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn post_vendor_create_then_get_lists_it() {
    let app = InventoryTestApp::new();

    let resp = TestRequest::post("/api/inventory/vendors")
        .json(&create_vendor_body())
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::CREATED);

    let list_resp = TestRequest::get("/api/inventory/vendors")
        .send(&app.router)
        .await;
    list_resp.assert_status(StatusCode::OK);

    let vendors: Vec<boss_inventory::types::Vendor> = list_resp.assert_json();
    assert_eq!(vendors.len(), 1, "expected one vendor after create");
    assert_eq!(vendors[0].name.as_deref(), Some("Acme Parts Co"));
}

// ---------------------------------------------------------------------------
// PUT /api/inventory/vendors/{id} — update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_existing_vendor_returns_204_no_content() {
    let vendor = vendor_fixture("VND-UPD-1");
    let app = InventoryTestApp::with_vendors(vec![vendor]);

    let resp = TestRequest::put("/api/inventory/vendors/VND-UPD-1")
        .json(&create_vendor_body())
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn put_existing_vendor_emits_vendor_updated_event() {
    let vendor = vendor_fixture("VND-UPD-2");
    let app = InventoryTestApp::with_vendors(vec![vendor]);

    TestRequest::put("/api/inventory/vendors/VND-UPD-2")
        .json(&create_vendor_body())
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.assert_event_recorded("inventory.vendor.updated");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("VND-UPD-2"),
    );
}

#[tokio::test]
async fn put_nonexistent_vendor_returns_404_not_found() {
    let app = InventoryTestApp::new();

    let resp = TestRequest::put("/api/inventory/vendors/VND-MISSING")
        .json(&create_vendor_body())
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn put_nonexistent_vendor_does_not_emit_event() {
    let app = InventoryTestApp::new();

    TestRequest::put("/api/inventory/vendors/VND-MISSING-2")
        .json(&create_vendor_body())
        .send(&app.router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.assert_event_not_recorded("inventory.vendor.updated");
}

// ---------------------------------------------------------------------------
// DELETE /api/inventory/vendors/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_existing_vendor_returns_204_no_content() {
    let vendor = vendor_fixture("VND-DEL-1");
    let app = InventoryTestApp::with_vendors(vec![vendor]);

    let resp = TestRequest::delete("/api/inventory/vendors/VND-DEL-1")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_existing_vendor_emits_vendor_deleted_event() {
    let vendor = vendor_fixture("VND-DEL-2");
    let app = InventoryTestApp::with_vendors(vec![vendor]);

    TestRequest::delete("/api/inventory/vendors/VND-DEL-2")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.assert_event_recorded("inventory.vendor.deleted");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("VND-DEL-2"),
    );
}

#[tokio::test]
async fn delete_nonexistent_vendor_returns_404_not_found() {
    let app = InventoryTestApp::new();

    let resp = TestRequest::delete("/api/inventory/vendors/VND-MISSING")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_nonexistent_vendor_does_not_emit_event() {
    let app = InventoryTestApp::new();

    TestRequest::delete("/api/inventory/vendors/VND-MISSING-2")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.assert_event_not_recorded("inventory.vendor.deleted");
}

// ---------------------------------------------------------------------------
// POST /api/inventory/items/{part_sku}/consume
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consume_part_emits_item_consumed_event() {
    let app = InventoryTestApp::new();

    let resp = TestRequest::post("/api/inventory/items/PART-001/consume")
        .json(&json!({ "qty": 2, "reason": "test" }))
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::OK);
    // The post-consume row state lands on `inventory.item.consumed`
    // — the legacy `inventory.part.consumed` marker was retired
    // since rebuild already uses the full-row state event.
    let event = app.assert_event_recorded("inventory.item.consumed");
    assert_eq!(
        event.payload.get("part_sku").and_then(|v| v.as_str()),
        Some("PART-001"),
    );
}

// ---------------------------------------------------------------------------
// POST /api/inventory/orders/create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_order_returns_201_and_emits_po_upserted_event() {
    let app = InventoryTestApp::new();

    let body = json!({
        "vendor": "Acme Parts Co",
        "lines": [
            { "part_sku": "PART-001", "qty": 10, "unit_cost_cents": 50 }
        ]
    });

    let resp = TestRequest::post("/api/inventory/orders/create")
        .json(&body)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
    // Full PO row lands on `inventory.purchase_order.upserted` (the
    // `PO_UPSERTED` event kind); the legacy `inventory.po.created`
    // marker was retired.
    let event = app.assert_event_recorded("inventory.purchase_order.upserted");
    assert!(
        event
            .payload
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .starts_with("PO-"),
        "expected event payload to contain a generated PO- id"
    );
}

// ---------------------------------------------------------------------------
// POST /api/inventory/orders/batch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn batch_create_orders_preserves_backdated_fields() {
    let app = InventoryTestApp::new();

    let body = json!([
        {
            "id": "PO-BATCH-1",
            "vendor": "Optica Components",
            "status": "received",
            "placed_on": "2022-06-01",
            "expected_on": "2022-06-15",
            "received_on": "2022-06-14",
            "lines": [
                { "part_sku": "PART-001", "qty": 10, "unit_cost_cents": 50 },
                { "part_sku": "PART-002", "qty": 5,  "unit_cost_cents": 120 }
            ]
        },
        {
            "id": "PO-BATCH-2",
            "vendor": "NetParts Direct",
            "status": "draft",
            "placed_on": "2026-04-01",
            "expected_on": "2026-04-11",
            "received_on": null,
            "lines": [
                { "part_sku": "PART-003", "qty": 20, "unit_cost_cents": 75 }
            ]
        }
    ]);

    let resp = TestRequest::post("/api/inventory/orders/batch")
        .json(&body)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let list = TestRequest::get("/api/inventory/orders")
        .send(&app.router)
        .await;
    list.assert_status(StatusCode::OK);
    let pos: Vec<boss_inventory::types::PurchaseOrder> = list.assert_json();
    assert_eq!(pos.len(), 2, "expected both POs persisted");

    let one = pos.iter().find(|p| p.id == "PO-BATCH-1").unwrap();
    assert_eq!(one.vendor.as_deref(), Some("Optica Components"));
    assert!(matches!(
        one.status,
        boss_inventory::types::PoStatus::Received
    ));
    assert_eq!(
        one.placed_on,
        Some(chrono::NaiveDate::from_ymd_opt(2022, 6, 1).unwrap())
    );
    assert_eq!(
        one.received_on,
        Some(chrono::NaiveDate::from_ymd_opt(2022, 6, 14).unwrap())
    );

    let two = pos.iter().find(|p| p.id == "PO-BATCH-2").unwrap();
    assert!(matches!(two.status, boss_inventory::types::PoStatus::Draft));
    assert_eq!(two.received_on, None);
}

// ---------------------------------------------------------------------------
// PUT /api/inventory/orders/{id}/status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_order_status_emits_po_status_changed_event() {
    let po = purchase_order_fixture("PO-UPD-1");
    let app = InventoryTestApp::with_orders(vec![po]);

    let resp = TestRequest::put("/api/inventory/orders/PO-UPD-1/status")
        .json(&json!({ "status": "in-transit" }))
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::OK);
    let event = app.assert_event_recorded("inventory.po.status_changed");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("PO-UPD-1"),
    );
}
