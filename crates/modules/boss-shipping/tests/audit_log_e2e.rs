//! End-to-end test for the shipping → audit_log chain.

#![cfg(feature = "postgres")]

mod common;

use axum::http::StatusCode;
use boss_testing::{TestDb, TestRequest};
use common::{ShippingTestApp, shipment_fixture};

#[tokio::test(flavor = "multi_thread")]
async fn create_shipment_lands_in_audit_log() {
    let db = TestDb::new().await;
    let app = ShippingTestApp::with_audit_pool(db.pool.clone());

    let ship = shipment_fixture("ship-audit-test");
    TestRequest::post("/api/shipping/shipments")
        .json(&ship)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let row: (String, String) = sqlx::query_as(
        "SELECT source, kind FROM audit_log \
         WHERE kind = 'shipping.shipment.created' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST");

    assert_eq!(row.0, "shipping");
    assert_eq!(row.1, "shipping.shipment.created");
}
