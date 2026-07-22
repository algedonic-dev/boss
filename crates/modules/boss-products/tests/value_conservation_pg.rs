//! Value conservation on finished goods (costing PR 6a, Q1:
//! value-primary) — the FG mirror of boss-inventory's test. produce
//! adds exact line totals; consume drains the proportional share
//! (final unit takes the remainder) and posts COGS at exactly the
//! drain; zero on_hand forces zero value. The 2026-07-06 365d regen
//! measured the old averaged scheme leaking +$6,597.77/yr here.

#![cfg(feature = "postgres")]

use boss_products::PgProducts;
use boss_products::port::ProductsRepository;
use boss_testing::TestDb;
use chrono::Utc;

fn stamp() -> boss_core::publisher::EventStamp {
    boss_core::publisher::EventStamp::new(
        "products",
        boss_core::actor::ActorId::Automation("test".into()),
        chrono::Utc::now(),
    )
}

async fn seed_product(db: &TestDb, sku: &str) {
    sqlx::query(
        "INSERT INTO products (sku, name, product_kind, package_unit, active) VALUES ($1, $1, 'beer', '1/2-bbl-keg', true) \
         ON CONFLICT (sku) DO NOTHING",
    )
    .bind(sku)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn cogs_total(db: &TestDb, sku: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM((payload->>'total_cost_cents')::bigint), 0)::bigint \
         FROM financial_facts \
         WHERE kind = 'finance.cogs.recognized' \
           AND source_table = 'products_consume' \
           AND payload->>'sku' = $1",
    )
    .bind(sku)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn produce_line_totals_then_full_drain_conserves_value_exactly() {
    let db = TestDb::new().await;
    let repo = PgProducts::new(db.pool.clone());
    let sku = "FP-TEST-1-2-BBL";
    let loc = "loc-test";
    seed_product(&db, sku).await;

    // Two produces with prime totals no per-unit cost can represent.
    repo.produce(
        sku,
        loc,
        210,
        Some(1_000_003),
        Utc::now(),
        "p1".into(),
        &stamp(),
    )
    .await
    .unwrap();
    repo.produce(
        sku,
        loc,
        315,
        Some(499_999),
        Utc::now(),
        "p2".into(),
        &stamp(),
    )
    .await
    .unwrap();
    let rows = repo.inventory_for(sku).await.unwrap();
    assert_eq!(rows[0].on_hand, 525);
    assert_eq!(rows[0].value_cents, 1_500_002);

    // Drain to zero across ugly quantities; COGS facts must sum to the
    // exact produced value and the empty row must hold zero.
    for (i, qty) in [7_i32, 517, 1].into_iter().enumerate() {
        repo.consume(
            sku,
            loc,
            qty,
            Some("wholesale"),
            Utc::now(),
            format!("c{i}"),
            &stamp(),
        )
        .await
        .unwrap();
    }
    let rows = repo.inventory_for(sku).await.unwrap();
    assert_eq!(rows[0].on_hand, 0);
    assert_eq!(
        rows[0].value_cents, 0,
        "zero on_hand must force zero value — {} cents stranded",
        rows[0].value_cents
    );
    assert_eq!(cogs_total(&db, sku).await, 1_500_002);
}
