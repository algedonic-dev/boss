//! `?account_id=...` filter on the shipments list endpoint. Same
//! shape as the commerce filter — required by the unified account
//! detail view's shipments section.

mod common;

use axum::http::StatusCode;
use boss_shipping::types::*;
use boss_testing::TestRequest;
use common::{ShippingTestApp, shipment_fixture};

fn ship_for(id: &str, account: Option<&str>) -> Shipment {
    let mut s = shipment_fixture(id);
    s.account_id = account.map(String::from);
    s
}

#[tokio::test]
async fn list_shipments_without_filter_returns_all() {
    let app = ShippingTestApp::with_shipments(vec![
        ship_for("ship-a", Some("account-001")),
        ship_for("ship-b", Some("account-002")),
        ship_for("ship-c", None),
    ]);

    let resp = TestRequest::get("/api/shipping/shipments")
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(body["total"], 3);
}

#[tokio::test]
async fn list_shipments_with_account_filter_returns_only_that_account() {
    let app = ShippingTestApp::with_shipments(vec![
        ship_for("ship-a", Some("account-001")),
        ship_for("ship-b", Some("account-002")),
        ship_for("ship-c", Some("account-001")),
    ]);

    let resp = TestRequest::get("/api/shipping/shipments?account_id=account-001")
        .send(&app.router)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(body["total"], 2);
    for entry in body["data"].as_array().unwrap() {
        assert_eq!(entry["account_id"], "account-001");
    }
}

#[tokio::test]
async fn list_shipments_with_unknown_account_returns_empty() {
    let app = ShippingTestApp::with_shipments(vec![ship_for("ship-a", Some("account-001"))]);

    let resp = TestRequest::get("/api/shipping/shipments?account_id=account-999")
        .send(&app.router)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(body["total"], 0);
}
