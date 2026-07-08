//! Idempotency guard on the relative raw-material consume.
//!
//! `consume_part_at` does `on_hand -= qty` (relative), so a redelivered
//! step-effect event must not double-decrement. The guard keys on the
//! deterministic `source_id`: replaying the same consume is a no-op on
//! `on_hand` (and dodges a spurious InsufficientStock once stock falls).

#![cfg(feature = "postgres")]

use boss_inventory::PgInventory;
use boss_inventory::port::InventoryRepository;
use boss_inventory::types::InventoryItem;
use boss_testing::TestDb;
use chrono::Utc;

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

#[tokio::test(flavor = "multi_thread")]
async fn consume_is_idempotent_on_source_id() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    // Seed 1000 units @ 50¢ so the consume writes a transfer fact (the
    // proof-of-application the guard keys on).
    inv.upsert_item_at(&item("ING-MALT-2ROW-50", 1000, 50), Utc::now())
        .await
        .unwrap();

    let key = "step-7:ING-MALT-2ROW-50";
    inv.consume_part_at("ING-MALT-2ROW-50", 200, Utc::now(), key)
        .await
        .unwrap();
    assert_eq!(
        inv.item_by_sku("ING-MALT-2ROW-50")
            .await
            .unwrap()
            .unwrap()
            .on_hand,
        800
    );

    // Replay with the SAME source_id: guard short-circuits, no second
    // decrement.
    let replay = inv
        .consume_part_at("ING-MALT-2ROW-50", 200, Utc::now(), key)
        .await
        .unwrap();
    assert_eq!(replay.item.on_hand, 800, "replay must not double-decrement");
    assert_eq!(
        inv.item_by_sku("ING-MALT-2ROW-50")
            .await
            .unwrap()
            .unwrap()
            .on_hand,
        800
    );

    // A distinct source_id DOES apply.
    inv.consume_part_at(
        "ING-MALT-2ROW-50",
        200,
        Utc::now(),
        "step-8:ING-MALT-2ROW-50",
    )
    .await
    .unwrap();
    assert_eq!(
        inv.item_by_sku("ING-MALT-2ROW-50")
            .await
            .unwrap()
            .unwrap()
            .on_hand,
        600
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_after_stock_fell_below_qty_is_still_a_noop() {
    // The dangerous case the guard also fixes: a consume commits, stock is
    // later drawn down below qty, then the original event redelivers. A
    // naive re-consume would fail the `on_hand >= qty` check → error → NAK
    // → dead-letter. The guard makes it a clean no-op instead.
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    inv.upsert_item_at(&item("ING-HOPS-CASCADE-44", 100, 30000), Utc::now())
        .await
        .unwrap();

    let key = "step-3:ING-HOPS-CASCADE-44";
    inv.consume_part_at("ING-HOPS-CASCADE-44", 80, Utc::now(), key)
        .await
        .unwrap(); // on_hand → 20
    // Draw it down below the original qty via a different consume.
    inv.consume_part_at("ING-HOPS-CASCADE-44", 15, Utc::now(), "other")
        .await
        .unwrap(); // on_hand → 5

    // Redeliver the first consume (qty 80, but only 5 on hand): guard makes
    // it a no-op rather than an InsufficientStock error.
    let replay = inv
        .consume_part_at("ING-HOPS-CASCADE-44", 80, Utc::now(), key)
        .await
        .expect("replay must succeed as a no-op, not InsufficientStock");
    assert_eq!(replay.item.on_hand, 5);
}
