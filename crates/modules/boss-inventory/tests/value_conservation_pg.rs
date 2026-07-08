//! Value conservation on raw inventory (costing PR 6a, Q1: value-primary).
//!
//! The stored, conserved quantity is `value_cents`; `avg_cost_cents` is
//! derived display (`value / on_hand`). Every mutation's GL amount IS
//! the row's value delta, so `balance(1300) == Σ value_cents` cannot
//! drift:
//!   - receive adds the exact line total (qty × unit price, no
//!     re-averaging arithmetic anywhere on the add side);
//!   - consume drains the proportional share
//!     `round(value × qty / on_hand)` with the final unit taking the
//!     remainder — draining to zero on_hand forces value to zero;
//!   - the transfer fact's `total_cost_cents` equals the drain exactly
//!     (what the ledger posts == what the row lost).
//!
//! The old scheme (integer-cent weighted averages) leaked $100–$200
//! per truncation at 10–20K-unit scale; the 2026-07-06 365d regen
//! measured the class at +$6,597.77/yr on FG. Design + decisions:
//! docs/design/inventory-value-conservation.md (Q1–Q3 resolved
//! 2026-07-07).

#![cfg(feature = "postgres")]

use boss_inventory::PgInventory;
use boss_inventory::port::InventoryRepository;
use boss_inventory::types::InventoryItem;
use boss_testing::TestDb;
use chrono::Utc;

fn item(sku: &str, on_hand: u32, value_cents: i64) -> InventoryItem {
    InventoryItem {
        part_sku: sku.into(),
        bin: "A-01".into(),
        on_hand,
        allocated: 0,
        reorder_point: 0,
        reorder_qty: 0,
        trailing_90d_usage: 0,
        value_cents,
        // Derived display — ignored on writes.
        avg_cost_cents: 0,
        vendor_price_cents: None,
        vendor_category: None,
    }
}

/// Sum of `total_cost_cents` across the item's transfer facts — what
/// the ledger actually posted for its consumes.
async fn posted_consume_total(db: &TestDb, sku: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM((payload->>'total_cost_cents')::bigint), 0)::bigint \
         FROM financial_facts \
         WHERE kind = 'finance.inventory.transferred' \
           AND source_table = 'inventory_consume' \
           AND payload->>'part_sku' = $1",
    )
    .bind(sku)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_price_receives_then_full_drain_conserves_value_exactly() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    let sku = "ING-MALT-2ROW-50";

    // Opening: 10,000 units worth exactly $3,333.33 — a value that no
    // integer per-unit cost can represent (33.3333¢/unit), the shape
    // that leaked under the old weighted-average scheme.
    inv.upsert_item_at(&item(sku, 10_000, 333_333), Utc::now())
        .await
        .unwrap();

    // Receive 7,919 more (prime, guarantees ugly division) at 41¢ —
    // the exact line total lands on the row, nothing is re-averaged.
    inv.receive_part_at(sku, 7_919, Some(41), Utc::now(), "recv:t1")
        .await
        .unwrap();
    let after_recv = inv.item_by_sku(sku).await.unwrap().unwrap();
    assert_eq!(after_recv.on_hand, 17_919);
    assert_eq!(after_recv.value_cents, 333_333 + 7_919 * 41);

    // Drain the row to zero across three consumes with remainders at
    // every step. Each fact's total must equal the row's value delta,
    // and zero on_hand must force zero value — nothing strands.
    let total_value = after_recv.value_cents;
    let mut drained = 0_i64;
    for (i, qty) in [5_000_u32, 9_999, 2_920].into_iter().enumerate() {
        let before = inv.item_by_sku(sku).await.unwrap().unwrap();
        inv.consume_part_at(sku, qty, Utc::now(), &format!("cons:t{i}"))
            .await
            .unwrap();
        let after = inv.item_by_sku(sku).await.unwrap().unwrap();
        let delta = before.value_cents - after.value_cents;
        assert!(delta >= 0, "consume must never add value");
        drained += delta;
        // The fact total posted for THIS consume == the value delta.
        assert_eq!(
            posted_consume_total(&db, sku).await,
            drained,
            "posted GL total diverged from the drained value at step {i}"
        );
    }
    let empty = inv.item_by_sku(sku).await.unwrap().unwrap();
    assert_eq!(empty.on_hand, 0);
    assert_eq!(
        empty.value_cents, 0,
        "zero on_hand must force zero value — {} cents stranded",
        empty.value_cents
    );
    assert_eq!(
        drained, total_value,
        "drains must sum to the exact value received"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn derived_avg_cost_is_value_over_on_hand() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    let sku = "ING-HOPS-CASCADE-44";

    inv.upsert_item_at(&item(sku, 0, 0), Utc::now())
        .await
        .unwrap();
    inv.receive_part_at(sku, 3, Some(1_000), Utc::now(), "recv:a")
        .await
        .unwrap();
    let row = inv.item_by_sku(sku).await.unwrap().unwrap();
    assert_eq!(row.value_cents, 3_000);
    // Display average comes from the derived column, never a stored
    // input: 3000 / 3 = 1000.
    let avg: i64 =
        sqlx::query_scalar("SELECT avg_cost_cents FROM inventory_items WHERE part_sku = $1")
            .bind(sku)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(avg, 1_000);
}
