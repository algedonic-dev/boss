//! Invariant: deleting a device model is blocked when any active
//! (non-decommissioned) devices in the assets still reference the
//! SKU.
//!
//! The guard is implemented as a cross-service call from boss-catalog
//! to boss-assets via the `AssetsClient` port. These tests exercise the
//! full HTTP path against a real Postgres database through TestDb,
//! swapping `FakeAssetsClient` in for the real assets service so the
//! tests are hermetic.
//!
//! Contract:
//!   - DELETE /api/catalog/models/{sku} → 204 when assets reports 0 active devices
//!   - DELETE /api/catalog/models/{sku} → 409 when assets reports any
//!   - DELETE /api/catalog/models/{sku} → 503 when assets is unreachable (fails closed)
//!   - DELETE /api/catalog/models/{sku} → 404 for unknown SKU (guard returns 0, then repo reports NotFound)
//!   - The guard fires BEFORE the SQL delete: verify the row is still present on 409.

#![cfg(feature = "postgres")]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_assets_client::{AssetsClient, FakeAssetsClient};
use boss_catalog::http::{KbApiState, router};
use boss_catalog::{InMemoryKb, PgKb};
use boss_testing::{TestDb, TestRequest};
use sqlx::PgPool;

async fn seed_model(pool: &PgPool, sku: &str) {
    // Insert a minimal valid asset_models row. Uses the schema's
    // CHECK constraints so anything that fails here is a schema
    // drift test, not a bug in the guard.
    sqlx::query(
        "INSERT INTO asset_models ( \
            sku, name, manufacturer, model_year, category, \
            regulator_device_class, \
            preventive_maintenance_interval_months, preventive_maintenance_hours, calibration_interval_months, \
            required_skill_level, depot_required, \
            list_price_new_cents, tagline, description, \
            width_cm, depth_cm, height_cm, weight_kg, power_requirements \
         ) VALUES ( \
            $1, $2, $3, $4, $5, \
            $6, \
            $7, $8, $9, \
            $10, $11, \
            $12, $13, $14, \
            $15, $16, $17, $18, $19 \
         )",
    )
    .bind(sku)
    .bind(format!("{sku} Test Device"))
    .bind("TestCo")
    .bind(2024_i32)
    .bind("fractional-co2")
    .bind(2_i32)
    .bind(6_i32)
    .bind(2.0_f32)
    .bind(12_i32)
    .bind(3_i32)
    .bind(false)
    .bind(5_000_000_i64)
    .bind("A test device")
    .bind("For delete guard tests")
    .bind(50.0_f32)
    .bind(50.0_f32)
    .bind(100.0_f32)
    .bind(80.0_f32)
    .bind("120V")
    .execute(pool)
    .await
    .expect("insert asset_model");
}

async fn model_exists(pool: &PgPool, sku: &str) -> bool {
    let (exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS(SELECT 1 FROM asset_models WHERE sku = $1)")
            .bind(sku)
            .fetch_one(pool)
            .await
            .expect("exists query");
    exists
}

fn build_router(pool: PgPool, client: Arc<dyn AssetsClient>) -> Router {
    // Build the kb HTTP router wired against PgKb so the
    // delete path exercises the real Postgres adapter.
    let state = KbApiState {
        catalog: Arc::new(PgKb::new(pool)),
        publisher: None,
        assets_client: client,
        classes_client: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_model_with_no_active_devices_returns_204_and_removes_row() {
    let db = TestDb::new().await;
    seed_model(&db.pool, "Boss-GUARD-HAPPY").await;

    let assets: Arc<dyn AssetsClient> = Arc::new(FakeAssetsClient::with_count(0));
    let router = build_router(db.pool.clone(), assets);

    let resp = TestRequest::delete("/api/catalog/models/Boss-GUARD-HAPPY")
        .send(&router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
    assert!(
        !model_exists(&db.pool, "Boss-GUARD-HAPPY").await,
        "kb model row should be removed after successful delete"
    );
}

#[tokio::test]
async fn delete_model_with_active_devices_returns_409_and_preserves_row() {
    let db = TestDb::new().await;
    seed_model(&db.pool, "Boss-GUARD-BUSY").await;

    let fake = Arc::new(FakeAssetsClient::with_count(7));
    let router = build_router(db.pool.clone(), fake.clone());

    let resp = TestRequest::delete("/api/catalog/models/Boss-GUARD-BUSY")
        .send(&router)
        .await;

    resp.assert_status(StatusCode::CONFLICT);
    assert!(
        model_exists(&db.pool, "Boss-GUARD-BUSY").await,
        "kb model row must be preserved when guard rejects the delete"
    );

    let body = resp.body_text();
    assert!(
        body.contains("active device"),
        "conflict message should explain why; got: {body}"
    );
    assert!(
        body.contains("7"),
        "conflict message should include the count; got: {body}"
    );

    assert_eq!(
        fake.calls(),
        vec!["Boss-GUARD-BUSY".to_string()],
        "guard should have queried assets exactly once with the right sku"
    );
}

#[tokio::test]
async fn delete_model_when_assets_unreachable_returns_503_and_preserves_row() {
    let db = TestDb::new().await;
    seed_model(&db.pool, "Boss-GUARD-ISOLATED").await;

    let assets: Arc<dyn AssetsClient> =
        Arc::new(FakeAssetsClient::unreachable("connection refused"));
    let router = build_router(db.pool.clone(), assets);

    let resp = TestRequest::delete("/api/catalog/models/Boss-GUARD-ISOLATED")
        .send(&router)
        .await;

    resp.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        model_exists(&db.pool, "Boss-GUARD-ISOLATED").await,
        "kb model row must be preserved when guard can't verify safety"
    );

    let body = resp.body_text();
    assert!(
        body.contains("assets unreachable"),
        "503 response should say why; got: {body}"
    );
}

#[tokio::test]
async fn delete_model_that_does_not_exist_returns_404() {
    let db = TestDb::new().await;
    let assets: Arc<dyn AssetsClient> = Arc::new(FakeAssetsClient::with_count(0));
    let router = build_router(db.pool.clone(), assets);

    // Guard passes (zero active devices for an unknown sku is
    // correct), then repo reports NotFound.
    let resp = TestRequest::delete("/api/catalog/models/Boss-DOES-NOT-EXIST")
        .send(&router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guard_queries_assets_before_touching_database() {
    // Ordering check: even though the kb delete *could*
    // succeed (the row exists), the guard must run first. We
    // verify this by having assets report active devices — the
    // row should stay put because the guard rejected before any
    // DELETE SQL ran.
    let db = TestDb::new().await;
    seed_model(&db.pool, "Boss-ORDER-CHECK").await;

    let fake = Arc::new(FakeAssetsClient::with_count(1));
    let router = build_router(db.pool.clone(), fake.clone());

    TestRequest::delete("/api/catalog/models/Boss-ORDER-CHECK")
        .send(&router)
        .await
        .assert_status(StatusCode::CONFLICT);

    assert!(
        model_exists(&db.pool, "Boss-ORDER-CHECK").await,
        "model must still exist after guard-blocked delete"
    );
    assert_eq!(fake.calls().len(), 1, "guard should have been called once");
}

// Silence unused import warnings when the test file compiles alone.
#[allow(dead_code)]
fn _import_marker() -> InMemoryKb {
    InMemoryKb::new(vec![])
}
