//! End-to-end: drive catalog writes through PgKb + PgAuditWriter,
//! snapshot asset_models + key satellites, drop, rebuild from
//! `audit_log`, assert match.

#![cfg(feature = "postgres")]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_assets_client::FakeAssetsClient;
use boss_catalog::PgKb;
use boss_catalog::http::{KbApiState, router};
use boss_catalog::rebuild_catalog;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{DateTime, Utc};
use common::model_fixture;
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
struct ModelRow {
    sku: String,
    name: String,
    manufacturer: String,
    model_year: i16,
    category: String,
    list_price_new_cents: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct UseCaseRow {
    sku: String,
    use_case: String,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
struct ExtrasRow {
    sku: String,
    extras: serde_json::Value,
}

async fn snapshot_models(pool: &PgPool) -> Vec<ModelRow> {
    sqlx::query_as("SELECT sku, name, manufacturer, model_year, category, list_price_new_cents, created_at, updated_at FROM asset_models ORDER BY sku")
        .fetch_all(pool).await.unwrap()
}
async fn snapshot_extras(pool: &PgPool) -> Vec<ExtrasRow> {
    sqlx::query_as("SELECT sku, extras FROM asset_models ORDER BY sku")
        .fetch_all(pool)
        .await
        .unwrap()
}
async fn snapshot_use_cases(pool: &PgPool) -> Vec<UseCaseRow> {
    sqlx::query_as("SELECT sku, use_case FROM asset_use_cases ORDER BY sku, use_case")
        .fetch_all(pool)
        .await
        .unwrap()
}

fn build_app(pool: PgPool) -> Router {
    let catalog = Arc::new(PgKb::new(pool.clone()));
    let publisher = DomainPublisher::new(RecordingEventBus::new(), "kb")
        .with_audit(Arc::new(PgAuditWriter::new(pool)));
    let state = KbApiState {
        catalog,
        publisher: Some(publisher),
        assets_client: Arc::new(FakeAssetsClient::with_count(0)),
        classes_client: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_models_and_satellites() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    // 1. Two models — one with multiple use_cases + non-trivial extras.
    let mut m1 = model_fixture("Boss-RB-001");
    m1.extras = serde_json::json!({"wavelengths_nm": [10600, 532]});
    m1.commerce.use_cases = vec!["resurfacing".into(), "scar-revision".into()];
    let m2 = model_fixture("Boss-RB-002");

    for m in [&m1, &m2] {
        TestRequest::post("/api/catalog/models")
            .json(m)
            .send(&app)
            .await
            .assert_status(StatusCode::CREATED);
    }

    // 2. Update m1 — change name, drop one wavelength in extras, swap use_cases.
    let mut m1_updated = m1.clone();
    m1_updated.name = "Renamed".into();
    m1_updated.extras = serde_json::json!({"wavelengths_nm": [10600]});
    m1_updated.commerce.use_cases = vec!["resurfacing".into(), "tightening".into()];
    TestRequest::put(format!("/api/catalog/models/{}", m1.sku))
        .json(&m1_updated)
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // 3. Snapshot.
    let models_before = snapshot_models(&db.pool).await;
    let extras_before = snapshot_extras(&db.pool).await;
    let ucs_before = snapshot_use_cases(&db.pool).await;
    assert_eq!(models_before.len(), 2);
    assert_eq!(extras_before.len(), 2);
    assert_eq!(ucs_before.len(), 2, "m1 has 2 use_cases after update");

    // 4. Verify audit_log has 3 events.
    let event_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE kind LIKE 'kb.model.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(event_count.0, 3, "got {} events", event_count.0);

    // 5. Wipe + rebuild.
    let report = rebuild_catalog(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.models_upserted, 3, "2 created + 1 updated");

    // 6. Reconstructed projections must match originals exactly.
    let models_after = snapshot_models(&db.pool).await;
    let extras_after = snapshot_extras(&db.pool).await;
    let ucs_after = snapshot_use_cases(&db.pool).await;
    assert_eq!(models_before, models_after, "asset_models mismatch");
    assert_eq!(extras_before, extras_after, "asset_models.extras mismatch");
    assert_eq!(ucs_before, ucs_after, "asset_use_cases mismatch");
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_handles_model_delete() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    let m = model_fixture("Boss-DOOMED");
    TestRequest::post("/api/catalog/models")
        .json(&m)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::delete(format!("/api/catalog/models/{}", m.sku))
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let report = rebuild_catalog(&db.pool).await.unwrap();
    assert!(report.models_upserted >= 1);
    assert!(report.models_deleted >= 1);

    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM asset_models WHERE sku = 'Boss-DOOMED'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count.0, 0, "rebuild should reproduce post-delete state");
}

/// Regression: pre-2026-05-07 catalog rebuilder ran
/// `DELETE FROM asset_models` first, which collided with the
/// non-cascading `assets(sku) → asset_models(sku)` FK whenever a
/// assets row referenced any model. The restart-epoch endpoint
/// hit this every time and silently left the sim wedged in
/// `paused=true`. The fix made the rebuilder
/// UPSERT-only — no DELETE, no FK collision. This test pins the
/// exact failure shape so the wipe pattern can't sneak back.
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_succeeds_with_referencing_systems_row() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    // 1. Create a model via the catalog API (lands in asset_models
    //    + emits kb.model.created).
    let m = model_fixture("Boss-FLEET-FK");
    TestRequest::post("/api/catalog/models")
        .json(&m)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // 2. Insert a assets row that FK-references the model. Direct
    //    INSERT (not via boss-assets HTTP) so the test stays
    //    self-contained — we only need the assets(sku) FK edge to
    //    exist; the rebuild path doesn't read assets rows.
    sqlx::query(
        "INSERT INTO assets \
         (asset_id, sku, phase, first_seen, last_event_at) \
         VALUES ($1, $2, 'installed', '2024-01-01', '2024-01-01')",
    )
    .bind("SYS-FK-TEST-001")
    .bind(&m.sku)
    .execute(&db.pool)
    .await
    .expect("insert assets row");

    // 3. Rebuild — pre-fix this failed with `update or delete on
    //    table "asset_models" violates foreign key constraint`.
    let report = rebuild_catalog(&db.pool)
        .await
        .expect("rebuild must not collide with systems(sku) FK");
    assert_eq!(report.models_upserted, 1);

    // 4. Both rows must still exist — UPSERT-only rebuild leaves
    //    assets untouched and re-applies the model.
    let model_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM asset_models WHERE sku = $1")
        .bind(&m.sku)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(model_count.0, 1, "asset_models row preserved");

    let asset_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM assets WHERE asset_id = 'SYS-FK-TEST-001'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(asset_count.0, 1, "assets row preserved through rebuild");
}
