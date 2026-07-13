//! The write-off drive arrives more than once per invoice: the
//! `bad-debt-writeoff` counterparty fires once per
//! `commerce.invoice.past_due` copy it receives, and post-#100 there
//! are two — ar-aging's sim-internal emission and the system's
//! webhook copy (`forward-invoice-past-due-to-webhook`, dead-air
//! until the stream captured `commerce.>`). The terminal flip must
//! converge: one status transition, one
//! `finance.invoice.written_off` fact, however many drives land.
//!
//! Pinned at the repository level because the transition gate lives
//! in the same transaction as the fact write. The HTTP
//! shape-resolution for the two drive payloads is pinned in
//! `http_writes.rs`.

#![cfg(feature = "postgres")]

use boss_commerce::PgCommerce;
use boss_commerce::port::{CommerceError, CommerceRepository};
use boss_commerce::types::*;
use boss_testing::TestDb;
use chrono::NaiveDate;

fn invoice(id: &str, status: &str) -> Invoice {
    Invoice {
        id: id.to_string(),
        account_id: "account-wo-1".to_string(),
        issued_on: NaiveDate::from_ymd_opt(2025, 4, 15).unwrap(),
        due_on: NaiveDate::from_ymd_opt(2025, 5, 15).unwrap(),
        paid_on: None,
        status: status.into(),
        amount_cents: 480_000,
        tax_cents: 0,
        tax_jurisdiction: None,
        currency: "USD".to_string(),
        payment_method: None,
        line_items: vec![InvoiceLineItem {
            id: format!("{id}-l1"),
            invoice_id: id.to_string(),
            revenue_category: RevenueCategory::from("wholesale"),
            amount_cents: 480_000,
            currency: "USD".to_string(),
            description: "Keg order".to_string(),
            ref_id: None,
            sku: None,
            qty: None,
            cost_basis_cents: None,
            cost_total_cents: None,
        }],
    }
}

async fn written_off_fact_count(db: &TestDb, source_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM financial_facts \
         WHERE kind = 'finance.invoice.written_off' \
           AND source_table = 'invoices' AND source_id = $1",
    )
    .bind(source_id)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn write_off_flips_once_and_double_delivery_converges() {
    let db = TestDb::new().await;
    let repo = PgCommerce::new(db.pool.clone());
    repo.create_invoice(&invoice("inv-step-wo-1", InvoiceStatus::PAST_DUE))
        .await
        .unwrap();

    let first = repo
        .mark_invoice_written_off("inv-step-wo-1")
        .await
        .unwrap();
    assert!(first, "first drive performs the flip");
    assert_eq!(written_off_fact_count(&db, "inv-step-wo-1").await, 1);

    let second = repo
        .mark_invoice_written_off("inv-step-wo-1")
        .await
        .unwrap();
    assert!(!second, "second drive converges as a no-op");
    assert_eq!(
        written_off_fact_count(&db, "inv-step-wo-1").await,
        1,
        "no duplicate bad-debt fact on double delivery"
    );

    let inv = repo
        .invoice_by_id("inv-step-wo-1")
        .await
        .unwrap()
        .expect("invoice exists");
    assert_eq!(inv.status.as_str(), InvoiceStatus::WRITTEN_OFF);
}

#[tokio::test]
async fn write_off_from_outstanding_is_allowed() {
    // The sim-internal drive can outrun the /past-due PUT (both fire
    // off ar-aging's emission), so an invoice still `outstanding`
    // writes off cleanly rather than dead-ending the drive.
    let db = TestDb::new().await;
    let repo = PgCommerce::new(db.pool.clone());
    repo.create_invoice(&invoice("inv-step-wo-2", InvoiceStatus::OUTSTANDING))
        .await
        .unwrap();

    assert!(
        repo.mark_invoice_written_off("inv-step-wo-2")
            .await
            .unwrap()
    );
    assert_eq!(written_off_fact_count(&db, "inv-step-wo-2").await, 1);
}

#[tokio::test]
async fn write_off_paid_invoice_conflicts() {
    // The paid and past-due counterparty branches are mutually
    // exclusive, so a write-off drive against a paid invoice is model
    // drift — refuse loudly instead of silently double-counting (cash
    // received AND bad-debt expense).
    let db = TestDb::new().await;
    let repo = PgCommerce::new(db.pool.clone());
    repo.create_invoice(&invoice("inv-step-wo-3", InvoiceStatus::PAID))
        .await
        .unwrap();

    let err = repo
        .mark_invoice_written_off("inv-step-wo-3")
        .await
        .unwrap_err();
    assert!(
        matches!(err, CommerceError::Conflict(_)),
        "expected Conflict, got {err:?}"
    );
    assert_eq!(written_off_fact_count(&db, "inv-step-wo-3").await, 0);
}

#[tokio::test]
async fn write_off_missing_invoice_not_found() {
    let db = TestDb::new().await;
    let repo = PgCommerce::new(db.pool.clone());

    let err = repo
        .mark_invoice_written_off("inv-step-nope")
        .await
        .unwrap_err();
    assert!(
        matches!(err, CommerceError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}
