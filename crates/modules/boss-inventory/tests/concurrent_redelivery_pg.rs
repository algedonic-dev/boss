//! Two deliveries of ONE receipt or consume, at the same instant
//! (backlog 33af9e59).
//!
//! The redelivery guard on `consume_part_at` / `receive_part_at` is a
//! READ of the proof fact the mutation writes. Until this pin it ran
//! BEFORE the row lock: two concurrent deliveries of one `source_id`
//! (JetStream redelivers while the first is still in flight) could both
//! read "no fact yet", both queue on the row lock, and both apply —
//! the fact insert is `ON CONFLICT DO NOTHING`, so the unique index
//! refused only the second fact, never the second `on_hand` move. The
//! stock moved twice against one ledger entry, and `balance(1300) ==
//! Σ value_cents` stopped holding.
//!
//! The shape is forced, not hoped for: a third connection holds the
//! row lock, both deliveries are released together off a barrier, and
//! the lock is let go only once Postgres reports both waiting on it.
//! On the old code both had passed the guard by then; on the fixed
//! code the guard runs under the lock, so the second sees the first's
//! committed fact.

use std::sync::Arc;
use std::time::{Duration, Instant};

use boss_inventory::PgInventory;
use boss_inventory::port::InventoryRepository;
use boss_inventory::types::InventoryItem;
use boss_testing::TestDb;
use chrono::Utc;
use sqlx::Connection;
use tokio::sync::Barrier;

fn stamp() -> boss_core::publisher::EventStamp {
    boss_core::publisher::EventStamp::new(
        "inventory-test",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .with_timestamp(Utc::now())
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

/// Take the item's row lock on a connection of its own and keep it
/// until the returned transaction ends.
async fn hold_row_lock(db: &TestDb, sku: &str) -> sqlx::PgConnection {
    let mut conn = sqlx::PgConnection::connect(&db.url()).await.unwrap();
    sqlx::query("BEGIN").execute(&mut conn).await.unwrap();
    sqlx::query("SELECT 1 FROM inventory_items WHERE part_sku = $1 FOR UPDATE")
        .bind(sku)
        .execute(&mut conn)
        .await
        .unwrap();
    conn
}

/// Wait until `n` backends of this database are blocked on a lock —
/// the deliveries have each gone as far as they can before the row
/// lock. Bounded, and a timeout names what it saw.
async fn wait_for_lock_waiters(pool: &sqlx::PgPool, n: i64) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting >= n {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "only {waiting} of {n} deliveries reached the row lock in 20s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_deliveries_of_one_consume_apply_it_once() {
    let db = TestDb::new().await;
    let inv = Arc::new(PgInventory::new(db.pool.clone()));
    let sku = "ING-RACE-CONSUME";
    // 1000 units @ 50¢: the consume drains value, so it writes the
    // GL-driving transfer fact the guard keys on.
    inv.upsert_item_at(&item(sku, 1000, 50), Utc::now(), &stamp())
        .await
        .unwrap();

    let mut blocker = hold_row_lock(&db, sku).await;
    let barrier = Arc::new(Barrier::new(2));
    let deliveries: Vec<_> = (0..2)
        .map(|_| {
            let inv = Arc::clone(&inv);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                inv.consume_part_at(sku, 200, Utc::now(), "step-9:race", &stamp())
                    .await
            })
        })
        .collect();
    wait_for_lock_waiters(&db.pool, 2).await;
    sqlx::query("ROLLBACK").execute(&mut blocker).await.unwrap();

    let mut applied = 0;
    for d in deliveries {
        let result = d.await.unwrap().expect("both deliveries answer Ok");
        applied += usize::from(result.fact_payload.is_some());
    }
    assert_eq!(applied, 1, "exactly one delivery applies the consume");

    let after = inv.item_by_sku(sku).await.unwrap().unwrap();
    assert_eq!(after.on_hand, 800, "the stock moved once, not twice");
    let posted: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM((payload->>'total_cost_cents')::bigint), 0)::bigint \
         FROM financial_facts \
         WHERE kind = 'finance.inventory.transferred' AND source_id = 'step-9:race'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        50_000 - after.value_cents,
        posted,
        "the value the row lost is the value the ledger posted"
    );
    assert_eq!(
        outbox_count(&db.pool, boss_inventory::events::ITEM_CONSUMED).await,
        1,
        "one consume fact on the record"
    );
    assert_eq!(
        outbox_count(&db.pool, boss_inventory::events::INVENTORY_TRANSFERRED).await,
        1,
        "one transfer on the record"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_deliveries_of_one_receipt_apply_it_once() {
    let db = TestDb::new().await;
    let inv = Arc::new(PgInventory::new(db.pool.clone()));
    let sku = "ING-RACE-RECEIVE";
    inv.upsert_item_at(&item(sku, 100, 40), Utc::now(), &stamp())
        .await
        .unwrap();

    let mut blocker = hold_row_lock(&db, sku).await;
    let barrier = Arc::new(Barrier::new(2));
    let deliveries: Vec<_> = (0..2)
        .map(|_| {
            let inv = Arc::clone(&inv);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                inv.receive_part_at(sku, 50, Some(40), Utc::now(), "recv:race", &stamp())
                    .await
            })
        })
        .collect();
    wait_for_lock_waiters(&db.pool, 2).await;
    sqlx::query("ROLLBACK").execute(&mut blocker).await.unwrap();

    let mut applied = 0;
    for d in deliveries {
        let result = d.await.unwrap().expect("both deliveries answer Ok");
        applied += usize::from(result.receipt_payload.is_some());
    }
    assert_eq!(applied, 1, "exactly one delivery applies the receipt");

    let after = inv.item_by_sku(sku).await.unwrap().unwrap();
    assert_eq!(after.on_hand, 150, "the stock moved once, not twice");
    assert_eq!(
        after.value_cents,
        4_000 + 50 * 40,
        "the line total landed once — the bill approval posts DR-1300 once"
    );
    assert_eq!(
        outbox_count(&db.pool, boss_inventory::events::ITEM_RECEIVED).await,
        1,
        "one receipt fact on the record"
    );
}
