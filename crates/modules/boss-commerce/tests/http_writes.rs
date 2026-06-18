//! HTTP-level write path tests for the commerce service.

mod common;

use axum::http::StatusCode;
use boss_testing::TestRequest;
use common::{CommerceTestApp, invoice_fixture};

// ---------------------------------------------------------------------------
// POST /api/commerce/invoices/create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_invoice_returns_201_on_valid_input() {
    let app = CommerceTestApp::new();
    let inv = invoice_fixture("inv-create-1");

    let resp = TestRequest::post("/api/commerce/invoices/create")
        .json(&inv)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_invoice_emits_commerce_invoice_created_event() {
    let app = CommerceTestApp::new();
    let inv = invoice_fixture("inv-event-1");

    TestRequest::post("/api/commerce/invoices/create")
        .json(&inv)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let event = app.bus.assert_event_emitted("commerce.invoice.created");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("inv-event-1"),
    );
}

#[tokio::test]
async fn post_invoice_with_invalid_json_returns_4xx() {
    let app = CommerceTestApp::new();

    let resp = TestRequest::post("/api/commerce/invoices/create")
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
async fn post_invoice_with_multiple_line_items_accepts_and_sum_matches() {
    use boss_commerce::types::{Invoice, InvoiceLineItem, InvoiceStatus, RevenueCategory};

    let app = CommerceTestApp::new();
    let inv = Invoice {
        id: "inv-multi-1".to_string(),
        account_id: "account-001".to_string(),
        issued_on: chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        due_on: chrono::NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
        paid_on: None,
        status: InvoiceStatus::OUTSTANDING.into(),
        amount_cents: 6_000_000,
        currency: "USD".to_string(),
        tax_cents: 0,
        tax_jurisdiction: None,
        payment_method: None,
        line_items: vec![
            InvoiceLineItem {
                id: "inv-multi-1-L1".to_string(),
                invoice_id: "inv-multi-1".to_string(),
                revenue_category: RevenueCategory::from("new-sales"),
                amount_cents: 4_500_000,
                currency: "USD".to_string(),
                description: "New device sale".to_string(),
                ref_id: Some("opp-001".to_string()),
                sku: None,
                qty: None,
                cost_basis_cents: None,
            },
            InvoiceLineItem {
                id: "inv-multi-1-L2".to_string(),
                invoice_id: "inv-multi-1".to_string(),
                revenue_category: RevenueCategory::from("contracts"),
                amount_cents: 1_200_000,
                currency: "USD".to_string(),
                description: "1-year service agreement".to_string(),
                ref_id: Some("sa-001".to_string()),
                sku: None,
                qty: None,
                cost_basis_cents: None,
            },
            InvoiceLineItem {
                id: "inv-multi-1-L3".to_string(),
                invoice_id: "inv-multi-1".to_string(),
                revenue_category: RevenueCategory::from("service"),
                amount_cents: 300_000,
                currency: "USD".to_string(),
                description: "Installation + training".to_string(),
                ref_id: Some("wo-001".to_string()),
                sku: None,
                qty: None,
                cost_basis_cents: None,
            },
        ],
    };

    let resp = TestRequest::post("/api/commerce/invoices/create")
        .json(&inv)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_invoice_with_mismatched_sum_rejected() {
    use boss_commerce::types::{Invoice, InvoiceLineItem, InvoiceStatus, RevenueCategory};

    let app = CommerceTestApp::new();
    let inv = Invoice {
        id: "inv-bad-sum".to_string(),
        account_id: "account-001".to_string(),
        issued_on: chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        due_on: chrono::NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
        paid_on: None,
        status: InvoiceStatus::OUTSTANDING.into(),
        amount_cents: 1_000_000, // header claims 10k
        currency: "USD".to_string(),
        tax_cents: 0,
        tax_jurisdiction: None,
        payment_method: None,
        line_items: vec![InvoiceLineItem {
            id: "inv-bad-sum-L1".to_string(),
            invoice_id: "inv-bad-sum".to_string(),
            revenue_category: RevenueCategory::from("new-sales"),
            amount_cents: 500_000, // line item only has 5k
            currency: "USD".to_string(),
            description: "mismatch".to_string(),
            ref_id: None,
            sku: None,
            qty: None,
            cost_basis_cents: None,
        }],
    };

    let resp = TestRequest::post("/api/commerce/invoices/create")
        .json(&inv)
        .send(&app.router)
        .await;

    assert!(
        resp.status.is_server_error() || resp.status.is_client_error(),
        "expected failure status for mismatched sum, got {}",
        resp.status,
    );
}

// ---------------------------------------------------------------------------
// PUT /api/commerce/invoices/{id}/paid
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_invoice_paid_returns_204() {
    let inv = invoice_fixture("inv-paid-1");
    let app = CommerceTestApp::with_invoices(vec![inv]);

    let resp = TestRequest::put("/api/commerce/invoices/inv-paid-1/paid")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn put_invoice_paid_emits_commerce_invoice_paid_event() {
    let inv = invoice_fixture("inv-paid-2");
    let app = CommerceTestApp::with_invoices(vec![inv]);

    TestRequest::put("/api/commerce/invoices/inv-paid-2/paid")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.bus.assert_event_emitted("commerce.invoice.paid");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("inv-paid-2"),
    );
}

#[tokio::test]
async fn put_invoice_paid_returns_404_for_unknown_id() {
    let app = CommerceTestApp::new();

    let resp = TestRequest::put("/api/commerce/invoices/inv-nope/paid")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_invoice_after_seeded_returns_same_data() {
    let inv = invoice_fixture("inv-roundtrip-1");
    let app = CommerceTestApp::with_invoices(vec![inv.clone()]);

    let resp = TestRequest::get("/api/commerce/invoices/inv-roundtrip-1")
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let fetched: boss_commerce::types::Invoice = resp.assert_json();
    assert_eq!(fetched.id, inv.id);
    assert_eq!(fetched.amount_cents, inv.amount_cents);
}
