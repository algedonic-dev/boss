//! Outbox phase 2 for boss-inventory, the contract in four tests:
//!
//! 1. A consume records `inventory.item.consumed` +
//!    `inventory.transferred` on the transactional outbox INSIDE the
//!    domain transaction — with the stamp's `_actor` enrichment — and
//!    a redelivered consume (same source_id) records NOTHING.
//! 2. A receive records `inventory.item.upserted` +
//!    `inventory.item.received` the same way; replay records nothing.
//! 3. A PO status flip records the post-update full-row state event
//!    (read back in-tx) + the status-changed marker atomically.
//! 4. A re-upsert of an already-approved vendor invoice appends no
//!    duplicate `approved` transition event (the transition gates on
//!    its financial fact actually inserting).

#![cfg(feature = "postgres")]

use boss_inventory::PgInventory;
use boss_inventory::port::InventoryRepository;
use boss_inventory::types::{
    InventoryItem, PoStatus, PurchaseOrder, PurchaseOrderLine, Vendor, VendorInvoice,
    VendorInvoiceStatus,
};
use boss_testing::TestDb;
use chrono::Utc;

fn stamp() -> boss_core::publisher::EventStamp {
    boss_core::publisher::EventStamp::new(
        "inventory-test",
        boss_core::actor::ActorId::Automation("test".into()),
        Utc::now(),
    )
}

fn item(sku: &str, on_hand: u32, unit_cost_cents: i64) -> InventoryItem {
    InventoryItem {
        part_sku: sku.into(),
        bin: "A-01".into(),
        on_hand,
        allocated: 0,
        reorder_point: 0,
        reorder_qty: 0,
        trailing_90d_usage: 0,
        value_cents: on_hand as i64 * unit_cost_cents,
        avg_cost_cents: 0, // derived display — ignored on writes
        vendor_price_cents: None,
        vendor_category: None,
    }
}

async fn outbox_count(pool: &sqlx::PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = $1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn consume_records_events_in_tx_and_replay_records_nothing() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    inv.upsert_item_at(&item("ING-OUTBOX-A", 100, 50), Utc::now(), &stamp())
        .await
        .unwrap();
    assert_eq!(outbox_count(&db.pool, "inventory.item.upserted").await, 1);

    inv.consume_part_at("ING-OUTBOX-A", 10, Utc::now(), "consume-key-1", &stamp())
        .await
        .unwrap();
    assert_eq!(outbox_count(&db.pool, "inventory.item.consumed").await, 1);
    assert_eq!(outbox_count(&db.pool, "inventory.transferred").await, 1);
    let actor: Option<String> = sqlx::query_scalar(
        "SELECT payload->>'_actor' FROM event_outbox \
         WHERE kind = 'inventory.item.consumed' LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        actor.as_deref(),
        Some("automation:test"),
        "the stamp's _actor enrichment rides the outbox payload"
    );

    // Redelivery: the idempotency guard sits AHEAD of the recording,
    // so the replay appends neither the state nor the transfer event.
    inv.consume_part_at("ING-OUTBOX-A", 10, Utc::now(), "consume-key-1", &stamp())
        .await
        .unwrap();
    assert_eq!(
        outbox_count(&db.pool, "inventory.item.consumed").await,
        1,
        "replayed consume records no state event"
    );
    assert_eq!(
        outbox_count(&db.pool, "inventory.transferred").await,
        1,
        "replayed consume records no transfer event"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn receive_records_events_in_tx_and_replay_records_nothing() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    inv.upsert_item_at(&item("ING-OUTBOX-B", 10, 50), Utc::now(), &stamp())
        .await
        .unwrap();

    inv.receive_part_at(
        "ING-OUTBOX-B",
        40,
        Some(50),
        Utc::now(),
        "receive-key-1",
        &stamp(),
    )
    .await
    .unwrap();
    // 1 from the seeding upsert + 1 post-receive snapshot.
    assert_eq!(outbox_count(&db.pool, "inventory.item.upserted").await, 2);
    assert_eq!(outbox_count(&db.pool, "inventory.item.received").await, 1);

    inv.receive_part_at(
        "ING-OUTBOX-B",
        40,
        Some(50),
        Utc::now(),
        "receive-key-1",
        &stamp(),
    )
    .await
    .unwrap();
    assert_eq!(
        outbox_count(&db.pool, "inventory.item.upserted").await,
        2,
        "replayed receive records no snapshot"
    );
    assert_eq!(
        outbox_count(&db.pool, "inventory.item.received").await,
        1,
        "replayed receive records no receipt marker"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn po_status_flip_records_post_update_state_plus_marker() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    // The PO's vendor_id FK needs a vendor row — which itself
    // exercises the vendor-create outbox path.
    inv.create_vendor_at(
        &Vendor {
            id: "vnd-outbox".into(),
            name: Some("Outbox Supply Co".into()),
            contact_name: None,
            contact_email: None,
            city: None,
            state: None,
            lead_time_days: 7,
            payment_terms: None,
            category: None,
            behavior: None,
        },
        Utc::now(),
        &stamp(),
    )
    .await
    .unwrap();
    assert_eq!(outbox_count(&db.pool, "inventory.vendor.created").await, 1);

    inv.create_purchase_order_at(
        &PurchaseOrder {
            id: "PO-OUTBOX-1".into(),
            vendor: Some("vnd-outbox".into()),
            status: PoStatus::Draft,
            placed_on: None,
            expected_on: None,
            received_on: None,
            lines: vec![PurchaseOrderLine {
                part_sku: "ING-OUTBOX-C".into(),
                qty: 5,
                unit_cost_cents: 100,
                currency: "USD".into(),
            }],
        },
        Utc::now(),
        &stamp(),
    )
    .await
    .unwrap();
    assert_eq!(
        outbox_count(&db.pool, "inventory.purchase_order.upserted").await,
        1
    );

    inv.update_po_status("PO-OUTBOX-1", "submitted", &stamp())
        .await
        .unwrap();
    assert_eq!(
        outbox_count(&db.pool, "inventory.purchase_order.upserted").await,
        2,
        "the flip records a second full-row state event"
    );
    assert_eq!(
        outbox_count(&db.pool, "inventory.po.status_changed").await,
        1
    );
    // The state event carries the POST-update status — read back
    // inside the same tx, not the caller's stale copy.
    let status: Option<String> = sqlx::query_scalar(
        "SELECT payload->>'status' FROM event_outbox \
         WHERE kind = 'inventory.purchase_order.upserted' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(status.as_deref(), Some("submitted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn vendor_invoice_reupsert_appends_no_duplicate_transition() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    // FK fixtures: the invoice's po_id → purchase_orders → vendors.
    inv.create_vendor_at(
        &Vendor {
            id: "vnd-outbox".into(),
            name: Some("Outbox Supply Co".into()),
            contact_name: None,
            contact_email: None,
            city: None,
            state: None,
            lead_time_days: 7,
            payment_terms: None,
            category: None,
            behavior: None,
        },
        Utc::now(),
        &stamp(),
    )
    .await
    .unwrap();
    inv.create_purchase_order_at(
        &PurchaseOrder {
            id: "PO-OUTBOX-9".into(),
            vendor: Some("vnd-outbox".into()),
            status: PoStatus::Draft,
            placed_on: None,
            expected_on: None,
            received_on: None,
            lines: vec![],
        },
        Utc::now(),
        &stamp(),
    )
    .await
    .unwrap();
    let approved_on: chrono::NaiveDate = "2026-07-01".parse().unwrap();
    let invoice = VendorInvoice {
        id: "vi-outbox-1".into(),
        po_id: "PO-OUTBOX-9".into(),
        vendor: "vnd-outbox".into(),
        vendor_invoice_no: "VI-9".into(),
        amount_cents: 500,
        currency: "USD".into(),
        received_on: approved_on,
        matched_on: None,
        approved_on: Some(approved_on),
        paid_on: None,
        status: VendorInvoiceStatus::Approved,
        discrepancy_cents: None,
        discrepancy_kind: None,
        lines: vec![],
    };

    inv.upsert_vendor_invoice_at(&invoice, Utc::now(), &stamp())
        .await
        .unwrap();
    inv.upsert_vendor_invoice_at(&invoice, Utc::now(), &stamp())
        .await
        .unwrap();

    assert_eq!(
        outbox_count(&db.pool, "inventory.vendor_invoice.upserted").await,
        2,
        "every upsert snapshots the full row (last-write-wins source)"
    );
    assert_eq!(
        outbox_count(&db.pool, "inventory.vendor_invoice.approved").await,
        1,
        "the approved transition records exactly once — gated on its fact inserting"
    );
}
