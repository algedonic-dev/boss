//! End-to-end test for the inventory → audit_log chain.

#![cfg(feature = "postgres")]

mod common;

use axum::http::StatusCode;
use boss_testing::{TestDb, TestRequest};
use common::InventoryTestApp;

#[tokio::test(flavor = "multi_thread")]
async fn create_vendor_lands_in_audit_log() {
    let db = TestDb::new().await;
    let app = InventoryTestApp::with_audit_pool(db.pool.clone());

    let body = serde_json::json!({
        "name": "Audit Test Vendor",
        "contact_name": "Jane Tester",
        "contact_email": "jane@audit.example",
        "city": "Austin",
        "state": "TX",
        "lead_time_days": 14,
        "payment_terms": "Net 30",
        "category": "parts",
    });
    TestRequest::post("/api/inventory/vendors")
        .json(&body)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let row: (String, String) = sqlx::query_as(
        "SELECT source, kind FROM audit_log \
         WHERE kind = 'inventory.vendor.created' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST");

    assert_eq!(row.0, "inventory");
    assert_eq!(row.1, "inventory.vendor.created");
}
