//! Idempotency guards on the relative FG stock mutations.
//!
//! `produce` (`on_hand += qty`) and `consume` (`on_hand -= qty`) are
//! relative, so a redelivered step-effect event (at-least-once JetStream
//! delivery) must not double-apply. The guard keys on the deterministic
//! `source_id`: replaying the same call is a no-op on `on_hand`.

#![cfg(feature = "postgres")]

use boss_products::ProductsRepository;
use boss_products::postgres::PgProducts;
use boss_products::types::Product;
use boss_testing::TestDb;
use chrono::Utc;

fn product(sku: &str) -> Product {
    Product {
        sku: sku.into(),
        name: "Test Keg".into(),
        product_kind: "beer".into(),
        package_unit: "1/2-bbl-keg".into(),
        description: None,
        metadata: serde_json::json!({}),
        active: true,
    }
}

async fn on_hand(repo: &PgProducts, sku: &str, loc: &str) -> i32 {
    repo.inventory_for(sku)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.location_id == loc)
        .map(|r| r.on_hand)
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
async fn produce_is_idempotent_on_source_id() {
    let db = TestDb::new().await;
    let repo = PgProducts::new(db.pool.clone());
    repo.upsert_product(&product("FP-IPA-1-2-BBL"))
        .await
        .unwrap();

    let key = "produce:step-1:FP-IPA-1-2-BBL".to_string();
    // First produce: 100 units @ 500¢ → on_hand = 100, a WIP→FG fact lands.
    repo.produce(
        "FP-IPA-1-2-BBL",
        "loc-bh",
        100,
        Some(500),
        Utc::now(),
        key.clone(),
    )
    .await
    .unwrap();
    assert_eq!(on_hand(&repo, "FP-IPA-1-2-BBL", "loc-bh").await, 100);

    // Replay with the SAME source_id (a redelivered produce): the guard
    // sees the fact and returns the current row WITHOUT re-incrementing.
    let replay = repo
        .produce("FP-IPA-1-2-BBL", "loc-bh", 100, Some(500), Utc::now(), key)
        .await
        .unwrap();
    assert_eq!(
        replay.inventory.on_hand, 100,
        "replay must not double-count"
    );
    assert!(
        replay.gl_move.is_none(),
        "replay must not post a second GL move"
    );
    assert_eq!(on_hand(&repo, "FP-IPA-1-2-BBL", "loc-bh").await, 100);

    // A genuinely distinct source_id DOES apply (sanity: the guard is keyed,
    // not a blanket block).
    repo.produce(
        "FP-IPA-1-2-BBL",
        "loc-bh",
        100,
        Some(500),
        Utc::now(),
        "produce:step-2:FP-IPA-1-2-BBL".to_string(),
    )
    .await
    .unwrap();
    assert_eq!(on_hand(&repo, "FP-IPA-1-2-BBL", "loc-bh").await, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn consume_is_idempotent_on_source_id() {
    let db = TestDb::new().await;
    let repo = PgProducts::new(db.pool.clone());
    repo.upsert_product(&product("FP-IPA-1-2-BBL"))
        .await
        .unwrap();
    // Stock it with a real cost basis so consume writes a COGS fact.
    repo.produce(
        "FP-IPA-1-2-BBL",
        "loc-bh",
        100,
        Some(500),
        Utc::now(),
        "produce:seed:FP-IPA-1-2-BBL".to_string(),
    )
    .await
    .unwrap();

    let key = "consume:step-9:FP-IPA-1-2-BBL".to_string();
    repo.consume(
        "FP-IPA-1-2-BBL",
        "loc-bh",
        30,
        None,
        Utc::now(),
        key.clone(),
    )
    .await
    .unwrap();
    assert_eq!(on_hand(&repo, "FP-IPA-1-2-BBL", "loc-bh").await, 70);

    // Replay: guard short-circuits, on_hand stays 70 (no double-decrement,
    // and no spurious InsufficientStock).
    let replay = repo
        .consume("FP-IPA-1-2-BBL", "loc-bh", 30, None, Utc::now(), key)
        .await
        .unwrap();
    assert_eq!(
        replay.inventory.on_hand, 70,
        "replay must not double-decrement"
    );
    assert!(replay.gl_move.is_none());
    assert_eq!(on_hand(&repo, "FP-IPA-1-2-BBL", "loc-bh").await, 70);
}
