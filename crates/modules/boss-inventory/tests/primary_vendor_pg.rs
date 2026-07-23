//! `primary_vendor_for_part` — category-based resolution.
//!
//! When a part has no PO history (the first auto-restock), the resolver
//! falls back to a vendor whose `category` matches the part's data-seeded
//! `vendor_category`. The match is fully generic — no SKU knowledge in
//! code; the mapping is data on the item (from the tenant's parts.toml).

#![cfg(feature = "postgres")]

use boss_inventory::PgInventory;
use boss_inventory::port::InventoryRepository;
use boss_inventory::types::InventoryItem;
use boss_testing::TestDb;
use chrono::Utc;

fn stamp() -> boss_core::publisher::EventStamp {
    boss_core::publisher::EventStamp::new(
        "inventory-test",
        boss_core::actor::ActorId::Automation("test".into()),
        chrono::Utc::now(),
    )
}

async fn insert_vendor(db: &TestDb, id: &str, category: &str) {
    sqlx::query(
        "INSERT INTO vendors \
            (id, name, contact_name, contact_email, phone, city, state, \
             lead_time_days, payment_terms, category) \
         VALUES ($1, $2, 'Contact', 'c@example.com', NULL, 'Town', 'CA', 7, 'net-30', $3)",
    )
    .bind(id)
    .bind(format!("Vendor {id}"))
    .bind(category)
    .execute(&db.pool)
    .await
    .unwrap();
}

fn item(sku: &str, vendor_category: Option<&str>) -> InventoryItem {
    InventoryItem {
        part_sku: sku.to_string(),
        bin: "A-01".to_string(),
        on_hand: 100,
        allocated: 0,
        reorder_point: 10,
        reorder_qty: 50,
        trailing_90d_usage: 30,
        value_cents: 0,
        avg_cost_cents: 0,
        vendor_price_cents: None,
        vendor_category: vendor_category.map(str::to_string),
    }
}

#[tokio::test]
async fn category_fallback_resolves_a_matching_vendor() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    // Two of the hops category — the lowest id wins, deterministically.
    insert_vendor(&db, "vnd-h1", "hops-supplier").await;
    insert_vendor(&db, "vnd-h0", "hops-supplier").await;
    insert_vendor(&db, "vnd-g0", "grain-supplier").await;
    inv.upsert_item_at(
        &item("ING-HOPS-X", Some("hops-supplier")),
        Utc::now(),
        &stamp(),
    )
    .await
    .unwrap();

    // No PO history → category match → the lowest-id hops vendor, not the
    // grain vendor.
    let v = inv.primary_vendor_for_part("ING-HOPS-X").await.unwrap();
    assert_eq!(v.as_deref(), Some("vnd-h0"));
}

#[tokio::test]
async fn category_with_no_vendor_resolves_to_none() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    insert_vendor(&db, "vnd-h0", "hops-supplier").await;
    inv.upsert_item_at(&item("PKG-X", Some("packaging")), Utc::now(), &stamp())
        .await
        .unwrap();

    // 'packaging' has no seeded vendor → None. The caller's rule must then
    // not fire (surfacing the gap), rather than inventing a bad vendor.
    let v = inv.primary_vendor_for_part("PKG-X").await.unwrap();
    assert_eq!(v, None);
}

#[tokio::test]
async fn part_without_a_category_resolves_to_none() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    insert_vendor(&db, "vnd-h0", "hops-supplier").await;
    inv.upsert_item_at(&item("ING-X", None), Utc::now(), &stamp())
        .await
        .unwrap();

    let v = inv.primary_vendor_for_part("ING-X").await.unwrap();
    assert_eq!(v, None);
}
