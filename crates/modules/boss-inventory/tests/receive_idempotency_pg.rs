//! Idempotency guard on the relative goods-receipt.
//!
//! `receive_part_at` does `on_hand += qty` (relative), so a redelivered
//! `step.done.receiving` event must not double-increment. Under JetStream
//! at-least-once redelivery, a re-applied receipt would inflate `on_hand`
//! while the matching DR-1300 (which rides the idempotent bill-approval
//! path) posts only once — drifting GL 1300. The guard keys on the
//! deterministic `source_id`: replaying the same receive is a no-op on
//! `on_hand`, and the `finance.inventory.received` proof-fact exists
//! exactly once. (Direct regression test for the GL-1300 decoupling.)
//!
//! The proof-fact is a DEDUP + AUDIT marker ONLY — it drives no GL
//! journal line. `received_fact_is_gl_inert` asserts it produces no
//! gl_journal_entries row.

#![cfg(feature = "postgres")]

use boss_inventory::PgInventory;
use boss_inventory::port::InventoryRepository;
use boss_inventory::types::InventoryItem;
use boss_testing::TestDb;
use chrono::Utc;

fn item(sku: &str, on_hand: u32, avg_cost_cents: i64) -> InventoryItem {
    InventoryItem {
        part_sku: sku.into(),
        bin: "A-01".into(),
        on_hand,
        allocated: 0,
        reorder_point: 0,
        reorder_qty: 0,
        trailing_90d_usage: 0,
        avg_cost_cents,
        vendor_price_cents: None,
        vendor_category: None,
    }
}

/// Count `finance.inventory.received` proof-facts for a given source_id.
async fn received_fact_count(pool: &sqlx::PgPool, source_id: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM financial_facts \
         WHERE kind = 'finance.inventory.received' \
           AND source_table = 'inventory_receipt' \
           AND source_id = $1",
    )
    .bind(source_id)
    .fetch_one(pool)
    .await
    .unwrap();
    n
}

#[tokio::test(flavor = "multi_thread")]
async fn receive_is_idempotent_on_source_id() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    // Seed 1000 units @ 50¢.
    inv.upsert_item_at(&item("ING-MALT-2ROW-50", 1000, 50), Utc::now())
        .await
        .unwrap();

    let key = "step-7:ING-MALT-2ROW-50";

    // First delivery: on_hand += 200 → 1200, and a proof-fact lands.
    let first = inv
        .receive_part_at("ING-MALT-2ROW-50", 200, Some(50), Utc::now(), key)
        .await
        .unwrap();
    assert_eq!(first.on_hand, 1200);
    assert_eq!(
        inv.item_by_sku("ING-MALT-2ROW-50")
            .await
            .unwrap()
            .unwrap()
            .on_hand,
        1200
    );
    assert_eq!(
        received_fact_count(&db.pool, key).await,
        1,
        "first receive must write exactly one proof-fact"
    );

    // Redelivery with the SAME source_id: guard short-circuits, no second
    // increment, no second fact row. THIS is the bug fix — without the
    // guard on_hand would climb to 1400 while DR-1300 stayed put.
    let replay = inv
        .receive_part_at("ING-MALT-2ROW-50", 200, Some(50), Utc::now(), key)
        .await
        .unwrap();
    assert_eq!(replay.on_hand, 1200, "replay must not double-increment");
    assert_eq!(
        inv.item_by_sku("ING-MALT-2ROW-50")
            .await
            .unwrap()
            .unwrap()
            .on_hand,
        1200
    );
    assert_eq!(
        received_fact_count(&db.pool, key).await,
        1,
        "replay must NOT write a second proof-fact"
    );

    // A distinct source_id DOES apply (a genuinely separate receipt).
    inv.receive_part_at(
        "ING-MALT-2ROW-50",
        200,
        Some(50),
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
        1400
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn received_fact_is_gl_inert() {
    // The proof-fact is dedup + audit ONLY. It must NOT drive a journal
    // line — the receive path's DR-1300 rides the idempotent
    // bill-approval path, so posting here would double-post it. Assert
    // the receive produces a financial_facts row but zero
    // gl_journal_entries rows.
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    inv.upsert_item_at(&item("ING-HOPS-CASCADE-44", 100, 30000), Utc::now())
        .await
        .unwrap();

    let key = "step-3:ING-HOPS-CASCADE-44";
    inv.receive_part_at("ING-HOPS-CASCADE-44", 40, Some(30000), Utc::now(), key)
        .await
        .unwrap();

    // The proof-fact exists ...
    assert_eq!(received_fact_count(&db.pool, key).await, 1);

    // ... but no journal entry was posted from it. Look up the fact id,
    // then assert no gl_journal_entries reference it.
    let (fact_id,): (uuid::Uuid,) = sqlx::query_as(
        "SELECT id FROM financial_facts \
         WHERE kind = 'finance.inventory.received' \
           AND source_table = 'inventory_receipt' AND source_id = $1",
    )
    .bind(key)
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let (je_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM gl_journal_entries WHERE fact_id = $1")
            .bind(fact_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        je_count, 0,
        "finance.inventory.received must drive NO journal line (GL-inert)"
    );
}
