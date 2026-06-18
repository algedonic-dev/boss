//! Account-scoped filters on the existing list endpoints.
//!
//! `?account_id=...` returns only the rows for that account. Used by
//! the unified account detail view to populate the pipeline and
//! finance sections without pulling everything down.
//!
//! Without the filter the existing "list everything" behavior is
//! preserved — old callers don't have to change.

mod common;

use boss_commerce::types::*;
use boss_testing::TestRequest;
use common::{CommerceTestApp, invoice_fixture};

fn inv_for(id: &str, account: &str) -> Invoice {
    let mut i = invoice_fixture(id);
    i.account_id = account.to_string();
    i
}

#[tokio::test]
async fn list_invoices_with_account_id_filters_correctly() {
    let app = CommerceTestApp::with_invoices(vec![
        inv_for("inv-a", "account-001"),
        inv_for("inv-b", "account-002"),
        inv_for("inv-c", "account-001"),
        inv_for("inv-d", "account-003"),
    ]);

    let resp = TestRequest::get("/api/commerce/invoices?account_id=account-001")
        .send(&app.router)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(body["total"], 2);
    for entry in body["data"].as_array().unwrap() {
        assert_eq!(entry["account_id"], "account-001");
    }
}

#[tokio::test]
async fn list_invoices_pagination_respects_account_filter() {
    // Filter first, then paginate. limit=1 + account-001 should return
    // one of the two account-001 invoices, not one of the four total.
    let app = CommerceTestApp::with_invoices(vec![
        inv_for("inv-a", "account-001"),
        inv_for("inv-b", "account-002"),
        inv_for("inv-c", "account-001"),
        inv_for("inv-d", "account-003"),
    ]);

    let resp = TestRequest::get("/api/commerce/invoices?account_id=account-001&limit=1")
        .send(&app.router)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(
        body["total"], 2,
        "total counts the filtered set, not the full set"
    );
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
}
