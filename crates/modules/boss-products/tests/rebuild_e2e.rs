//! End-to-end: drive products writes through the HTTP API on the
//! real pipeline (outbox phase 2), snapshot `products` +
//! `finished_product_inventory`, rebuild from `audit_log`, and
//! assert byte-identical rows — timestamps included.
//!
//! Before packet d7b8158e both the live writes and the rebuilder
//! left `created_at` / `updated_at` to the column DEFAULT NOW() (or
//! stamped NOW() in the conflict arm), so a replay reproduced the
//! catalog with rebuild-time timestamps instead of event-time.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_products::http::{ProductsApiState, router};
use boss_products::postgres::PgProducts;
use boss_products::rebuild_products;
use boss_products::types::{Product, ProductInventory};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct ProductRow {
    sku: String,
    name: String,
    product_kind: String,
    package_unit: String,
    description: Option<String>,
    metadata: serde_json::Value,
    active: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct InventoryRow {
    product_sku: String,
    location_id: String,
    on_hand: i32,
    reserved: i32,
    value_cents: i64,
    updated_at: DateTime<Utc>,
}

async fn snapshot_products(pool: &PgPool) -> Vec<ProductRow> {
    sqlx::query_as(
        "SELECT sku, name, product_kind, package_unit, description, metadata, active, \
                created_at, updated_at \
         FROM products ORDER BY sku",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn snapshot_inventory(pool: &PgPool) -> Vec<InventoryRow> {
    sqlx::query_as(
        "SELECT product_sku, location_id, on_hand, reserved, value_cents, updated_at \
         FROM finished_product_inventory ORDER BY product_sku, location_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn build_app(pool: PgPool) -> Router {
    router(ProductsApiState {
        products: Arc::new(PgProducts::new(pool)),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    })
}

async fn drain_outbox(pool: &PgPool) -> u64 {
    let bus = RecordingEventBus::new();
    drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain")
        .delivered
}

fn product(sku: &str, name: &str) -> Product {
    Product {
        sku: sku.into(),
        name: name.into(),
        product_kind: "beer".into(),
        package_unit: "1/2-bbl-keg".into(),
        description: Some("Test brew".into()),
        metadata: serde_json::json!({"abv_pct": 5.0}),
        active: true,
    }
}

#[tokio::test]
async fn rebuilding_sku_versions_restores_complete_history_twice() {
    use boss_products::ProductsRepository;
    let db = TestDb::new().await;
    let repo = PgProducts::new(db.pool.clone());
    let first = sku_fixture("fixture-replay", "First name", 1);
    let event_stamp = boss_core::publisher::EventStamp::new(
        "products",
        boss_core::actor::ActorId::Automation("fixture".into()),
    );
    repo.append_sku_version(&first, &event_stamp).await.unwrap();
    let mut second = first.clone();
    second.version = 2;
    second.name = "Retired name".into();
    second.active = false;
    second.price.amount_cents = 25000;
    repo.append_sku_version(&second, &event_stamp)
        .await
        .unwrap();
    let expected = repo.sku_versions().await.unwrap();
    let expected_storage: Vec<(String, i64, serde_json::Value, DateTime<Utc>)> = sqlx::query_as(
        "SELECT sku, version, data, created_at FROM product_sku_versions ORDER BY sku COLLATE \"C\", version",
    ).fetch_all(&db.pool).await.unwrap();
    assert_eq!(expected, vec![first, second]);
    assert_eq!(drain_outbox(&db.pool).await, 2);
    // Corrupt the projection, never its immutable source facts. A
    // rebuilder that merely leaves the new table alone must fail.
    sqlx::query("DELETE FROM product_sku_versions")
        .execute(&db.pool)
        .await
        .unwrap();
    for _ in 0..2 {
        rebuild_products(&db.pool).await.unwrap();
        assert_eq!(repo.sku_versions().await.unwrap(), expected);
        let storage: Vec<(String, i64, serde_json::Value, DateTime<Utc>)> = sqlx::query_as(
            "SELECT sku, version, data, created_at FROM product_sku_versions ORDER BY sku COLLATE \"C\", version",
        ).fetch_all(&db.pool).await.unwrap();
        assert_eq!(storage, expected_storage);
    }
    boss_subject_kinds::subjects::rebuild_subjects(&db.pool)
        .await
        .unwrap();
    let label = sqlx::query_scalar::<_, Option<String>>(
        "SELECT label FROM subjects WHERE kind = 'product' AND id = 'fixture-replay'",
    )
    .fetch_optional(&db.pool)
    .await
    .unwrap()
    .flatten();
    assert_eq!(label.as_deref(), Some("Retired name"));
}

#[tokio::test]
async fn sku_fact_label_must_match_its_complete_row_on_replay() {
    let db = TestDb::new().await;
    let row = sku_fixture("fixture-label-refusal", "The complete row's label", 1);
    // A deliberately malformed fixture fact, not a rewrite of a recorded event.
    let fact = boss_core::publisher::EventStamp::new(
        "products",
        boss_core::actor::ActorId::Automation("fixture".into()),
    )
    .event(
        boss_products::events::SKU_VERSION_APPENDED,
        serde_json::json!({
            "sku": row.sku, "version": row.version,
            "name": "A disagreeing identity label", "data": row,
        }),
    );
    let mut tx = db.pool.begin().await.unwrap();
    boss_events::outbox::record_event_in_tx(&mut tx, &fact)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(drain_outbox(&db.pool).await, 1);
    let error = rebuild_products(&db.pool).await.unwrap_err();
    assert!(error.to_string().contains("label"), "{error}");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM product_sku_versions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

fn sku_fixture(sku: &str, name: &str, version: i64) -> boss_products::sku::SkuVersion {
    serde_json::from_value(serde_json::json!({
        "sku": sku, "name": name, "kind": "fixture-class",
        "description": "First promise", "price": {"amount_cents": 10100, "currency": "USD", "basis": "prepaid-min"},
        "includes": [{"code": "fixture", "text": "First entitlement", "ref": {"subject_kind": "product", "code": "fixture-class"}}],
        "active": true, "version": version
    })).unwrap()
}

async fn alternating_identity_history(last_legacy: bool) -> (Option<String>, Option<String>) {
    use boss_products::ProductsRepository;
    let db = TestDb::new().await;
    let repo = PgProducts::new(db.pool.clone());
    let sku = "fixture-shared-identity";
    let stamp = boss_core::publisher::EventStamp::new(
        "products",
        boss_core::actor::ActorId::Automation("fixture".into()),
    );
    repo.upsert_product(&product(sku, "Legacy one"), &stamp)
        .await
        .unwrap();
    repo.append_sku_version(&sku_fixture(sku, "SKU one", 1), &stamp)
        .await
        .unwrap();
    repo.upsert_product(&product(sku, "Legacy two"), &stamp)
        .await
        .unwrap();
    repo.append_sku_version(&sku_fixture(sku, "SKU two", 2), &stamp)
        .await
        .unwrap();
    if last_legacy {
        repo.upsert_product(&product(sku, "Legacy three"), &stamp)
            .await
            .unwrap();
    }
    let live: Option<String> =
        sqlx::query_scalar("SELECT label FROM subjects WHERE kind = 'product' AND id = $1")
            .bind(sku)
            .fetch_optional(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        live.as_deref(),
        Some(if last_legacy {
            "Legacy three"
        } else {
            "SKU two"
        })
    );
    assert_eq!(
        drain_outbox(&db.pool).await,
        if last_legacy { 5 } else { 4 }
    );
    boss_subject_kinds::subjects::rebuild_subjects(&db.pool)
        .await
        .unwrap();
    let rebuilt: Option<String> =
        sqlx::query_scalar("SELECT label FROM subjects WHERE kind = 'product' AND id = $1")
            .bind(sku)
            .fetch_optional(&db.pool)
            .await
            .unwrap();
    (live, rebuilt)
}

#[tokio::test]
async fn alternating_history_keeps_a_newer_sku_subject_label() {
    let (live, rebuilt) = alternating_identity_history(false).await;
    assert_eq!(rebuilt, live);
}

#[tokio::test]
async fn alternating_history_keeps_a_newer_legacy_goods_subject_label() {
    let (live, rebuilt) = alternating_identity_history(true).await;
    assert_eq!(rebuilt, live);
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_products_and_inventory_byte_identical() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    // 1. Two products; re-upsert one so the ON CONFLICT arm (the
    //    `updated_at` stamp) is exercised, not just the insert arm.
    TestRequest::post("/api/products")
        .json(&product("FP-TEST-PALE", "Test Pale"))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);
    TestRequest::post("/api/products")
        .json(&product("FP-TEST-STOUT", "Test Stout"))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);
    TestRequest::post("/api/products")
        .json(&product("FP-TEST-PALE", "Test Pale (renamed)"))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // 2. An inventory row via PUT, then a produce and a consume so
    //    every write arm that stamps `updated_at` runs.
    TestRequest::put("/api/products/FP-TEST-PALE/inventory")
        .json(&ProductInventory {
            product_sku: "FP-TEST-PALE".into(),
            location_id: "wh-1".into(),
            on_hand: 10,
            reserved: 0,
            value_cents: 10_000,
            production_cost_cents: 0,
            updated_at: None,
        })
        .send(&app)
        .await
        .assert_status(StatusCode::OK);
    TestRequest::post("/api/products/FP-TEST-PALE/inventory/produce")
        .json(&serde_json::json!({
            "location_id": "wh-1",
            "qty": 5,
            "total_cost_cents": 5_000,
            "idempotency_key": "rebuild-e2e-produce-1",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);
    TestRequest::post("/api/products/FP-TEST-PALE/inventory/consume")
        .json(&serde_json::json!({
            "location_id": "wh-1",
            "qty": 3,
            "idempotency_key": "rebuild-e2e-consume-1",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    drain_outbox(&db.pool).await;

    let products_before = snapshot_products(&db.pool).await;
    let inventory_before = snapshot_inventory(&db.pool).await;
    assert_eq!(products_before.len(), 2);
    assert_eq!(inventory_before.len(), 1);
    assert_eq!(inventory_before[0].on_hand, 12, "10 + 5 - 3");

    // 3. Rebuild wipes both tables and replays audit_log.
    let report = rebuild_products(&db.pool).await.expect("rebuild");
    assert_eq!(report.products_upserted, 3, "2 creates + 1 re-upsert");

    let products_after = snapshot_products(&db.pool).await;
    let inventory_after = snapshot_inventory(&db.pool).await;
    assert_eq!(
        products_before, products_after,
        "products must replay byte-identical (created_at/updated_at included)"
    );
    assert_eq!(
        inventory_before, inventory_after,
        "finished_product_inventory must replay byte-identical (updated_at included)"
    );
}
