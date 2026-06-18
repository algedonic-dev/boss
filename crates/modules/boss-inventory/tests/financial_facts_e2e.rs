//! End-to-end tests for inventory → financial_facts — v1a of the GL track.
//!
//! Verifies that AP domain state transitions emit the right fact, in the
//! same transaction as the domain write, idempotently.

#![cfg(feature = "postgres")]

use boss_inventory::PgInventory;
use boss_inventory::port::InventoryRepository;
use boss_inventory::types::*;
use boss_ledger::{FactRef, post_fact_in_tx};
use boss_testing::TestDb;
use chrono::NaiveDate;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

/// Seed opening cash so cash-crediting posts (here `finance.bill.paid`,
/// which is DR 2100 AP / CR 1000 Cash) don't trip the "1000 Cash must
/// not go negative" guard in `post_fact_in_tx`. Posts a balanced manual
/// entry DR 1000 Cash / CR 3000 Retained Earnings — the books'
/// equivalent of an opening capital injection on a fresh ledger.
async fn seed_opening_cash(db: &TestDb, cents: i64) {
    let id = Uuid::new_v4();
    let kind = "finance.manual.entry";
    let happened_on = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let payload = json!({
        "lines": [
            {"account_code": "1000", "debit_cents": cents, "memo": "opening cash"},
            {"account_code": "3000", "credit_cents": cents, "memo": "opening capital"},
        ]
    });
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, 'manual_entries', 'opening-cash', 'test')",
    )
    .bind(id)
    .bind(kind)
    .bind(happened_on)
    .bind(&payload)
    .execute(&mut *tx)
    .await
    .unwrap();
    post_fact_in_tx(
        &mut tx,
        &FactRef {
            id,
            kind,
            happened_on,
            payload: &payload,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn vendor_invoice(
    id: &str,
    status: VendorInvoiceStatus,
    matched_on: Option<NaiveDate>,
    approved_on: Option<NaiveDate>,
    paid_on: Option<NaiveDate>,
) -> VendorInvoice {
    VendorInvoice {
        id: id.to_string(),
        po_id: "po-ff-1".to_string(),
        vendor: "Widgetco".to_string(),
        vendor_invoice_no: format!("{id}-V1"),
        amount_cents: 450_000,
        currency: "USD".to_string(),
        received_on: NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        matched_on,
        approved_on,
        paid_on,
        status,
        discrepancy_cents: None,
        discrepancy_kind: None,
        lines: Vec::new(),
    }
}

async fn seed_po(db: &TestDb) {
    sqlx::query(
        "INSERT INTO purchase_orders (id, vendor, status, placed_on, expected_on) \
         VALUES ('po-ff-1', 'Widgetco', 'received', '2026-02-20', '2026-03-01') \
         ON CONFLICT DO NOTHING",
    )
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn entry_count_for(db: &TestDb, source_id: &str, kind: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM gl_journal_entries e \
         JOIN financial_facts f ON f.id = e.fact_id \
         WHERE f.source_id = $1 AND f.kind = $2",
    )
    .bind(source_id)
    .bind(kind)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

async fn fact_count(db: &TestDb, kind: &str, source_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM financial_facts \
         WHERE kind = $1 AND source_table = 'vendor_invoices' AND source_id = $2",
    )
    .bind(kind)
    .bind(source_id)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn received_only_emits_no_fact() {
    let db = TestDb::new().await;
    seed_po(&db).await;
    let inv = PgInventory::new(db.pool.clone());

    inv.upsert_vendor_invoice(&vendor_invoice(
        "vi-ff-1",
        VendorInvoiceStatus::Received,
        None,
        None,
        None,
    ))
    .await
    .unwrap();

    assert_eq!(fact_count(&db, "finance.bill.approved", "vi-ff-1").await, 0);
    assert_eq!(fact_count(&db, "finance.bill.paid", "vi-ff-1").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn approved_emits_approved_fact() {
    let db = TestDb::new().await;
    seed_po(&db).await;
    let inv = PgInventory::new(db.pool.clone());

    let approved_on = NaiveDate::from_ymd_opt(2026, 3, 5).unwrap();
    inv.upsert_vendor_invoice(&vendor_invoice(
        "vi-ff-2",
        VendorInvoiceStatus::Approved,
        Some(approved_on),
        Some(approved_on),
        None,
    ))
    .await
    .unwrap();

    assert_eq!(fact_count(&db, "finance.bill.approved", "vi-ff-2").await, 1);
    assert_eq!(fact_count(&db, "finance.bill.paid", "vi-ff-2").await, 0);

    let payload: Value = sqlx::query_scalar(
        "SELECT payload FROM financial_facts \
         WHERE source_id = 'vi-ff-2' AND kind = 'finance.bill.approved'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload["vendor_invoice_id"], "vi-ff-2");
    assert_eq!(payload["po_id"], "po-ff-1");
    assert_eq!(payload["vendor"], "Widgetco");
    assert_eq!(payload["amount_cents"], 450_000);
    assert_eq!(payload["currency"], "USD");
}

#[tokio::test(flavor = "multi_thread")]
async fn paid_emits_both_facts() {
    let db = TestDb::new().await;
    seed_po(&db).await;
    seed_opening_cash(&db, 1_000_000).await;
    let inv = PgInventory::new(db.pool.clone());

    let approved_on = NaiveDate::from_ymd_opt(2026, 3, 5).unwrap();
    let paid_on = NaiveDate::from_ymd_opt(2026, 4, 4).unwrap();
    inv.upsert_vendor_invoice(&vendor_invoice(
        "vi-ff-3",
        VendorInvoiceStatus::Paid,
        Some(approved_on),
        Some(approved_on),
        Some(paid_on),
    ))
    .await
    .unwrap();

    assert_eq!(fact_count(&db, "finance.bill.approved", "vi-ff-3").await, 1);
    assert_eq!(fact_count(&db, "finance.bill.paid", "vi-ff-3").await, 1);
    // v1b: both facts produced balanced journal entries.
    assert_eq!(
        entry_count_for(&db, "vi-ff-3", "finance.bill.approved").await,
        1
    );
    assert_eq!(
        entry_count_for(&db, "vi-ff-3", "finance.bill.paid").await,
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stepwise_lifecycle_emits_facts_exactly_once() {
    // Simulates the generator path: received → approved → paid, as three
    // upserts. Each upsert emits whichever facts match the current state;
    // the unique index prevents double-counts.
    let db = TestDb::new().await;
    seed_po(&db).await;
    seed_opening_cash(&db, 1_000_000).await;
    let inv = PgInventory::new(db.pool.clone());

    inv.upsert_vendor_invoice(&vendor_invoice(
        "vi-ff-4",
        VendorInvoiceStatus::Received,
        None,
        None,
        None,
    ))
    .await
    .unwrap();

    let approved_on = NaiveDate::from_ymd_opt(2026, 3, 5).unwrap();
    inv.upsert_vendor_invoice(&vendor_invoice(
        "vi-ff-4",
        VendorInvoiceStatus::Approved,
        Some(approved_on),
        Some(approved_on),
        None,
    ))
    .await
    .unwrap();

    let paid_on = NaiveDate::from_ymd_opt(2026, 4, 4).unwrap();
    inv.upsert_vendor_invoice(&vendor_invoice(
        "vi-ff-4",
        VendorInvoiceStatus::Paid,
        Some(approved_on),
        Some(approved_on),
        Some(paid_on),
    ))
    .await
    .unwrap();

    assert_eq!(fact_count(&db, "finance.bill.approved", "vi-ff-4").await, 1);
    assert_eq!(fact_count(&db, "finance.bill.paid", "vi-ff-4").await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_is_idempotent() {
    let db = TestDb::new().await;
    seed_po(&db).await;
    seed_opening_cash(&db, 1_000_000).await;
    let inv = PgInventory::new(db.pool.clone());

    let approved_on = NaiveDate::from_ymd_opt(2026, 3, 5).unwrap();
    let paid_on = NaiveDate::from_ymd_opt(2026, 4, 4).unwrap();
    let fixture = vendor_invoice(
        "vi-ff-5",
        VendorInvoiceStatus::Paid,
        Some(approved_on),
        Some(approved_on),
        Some(paid_on),
    );

    inv.upsert_vendor_invoice(&fixture).await.unwrap();
    inv.upsert_vendor_invoice(&fixture).await.unwrap();
    inv.upsert_vendor_invoice(&fixture).await.unwrap();

    assert_eq!(fact_count(&db, "finance.bill.approved", "vi-ff-5").await, 1);
    assert_eq!(fact_count(&db, "finance.bill.paid", "vi-ff-5").await, 1);
    // 2 invoice facts (approved + paid) + 1 seeded opening-cash manual
    // entry = 3.
    let total: i64 = sqlx::query("SELECT COUNT(*) FROM financial_facts")
        .fetch_one(&db.pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(total, 3);
}
