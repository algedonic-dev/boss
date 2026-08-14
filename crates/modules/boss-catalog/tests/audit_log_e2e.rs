//! End-to-end test for the kb → audit_log chain, on the REAL
//! pipeline (outbox phase 2): the POST records the event on the
//! transactional outbox inside the adapter's tx; the relay drain
//! moves it to audit_log. Deliberately NO direct audit writer — this
//! test only passes through the real outbox → relay → audit_log path.

mod common;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_assets_client::FakeAssetsClient;
use boss_catalog::PgKb;
use boss_catalog::http::{KbApiState, router};
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use common::model_fixture;
use sqlx::PgPool;

fn build_app(pool: PgPool) -> Router {
    // No publisher: the handler stamp falls back to source="kb" and
    // the event records on the outbox.
    router(KbApiState {
        catalog: Arc::new(PgKb::new(pool)),
        publisher: None,
        assets_client: Arc::new(FakeAssetsClient::with_count(0)),
        classes_client: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn post_model_lands_in_audit_log() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    let model = model_fixture("Boss-AUDIT-CATALOG");
    TestRequest::post("/api/catalog/models")
        .json(&model)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Drain the outbox through the relay pipeline: outbox →
    // audit_log (chained) → bus → delivered.
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
    assert_eq!(stats.delivered, 1, "the create arrives via the outbox");

    // Query the audit log directly via the same TestDb pool. The row
    // must show up under the kb source with the model_created
    // kind and the sku in its payload.
    let row: (String, String, serde_json::Value) = sqlx::query_as(
        "SELECT source, kind, payload FROM audit_log \
         WHERE kind = 'kb.model.created' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST + drain");

    assert_eq!(row.0, "kb");
    assert_eq!(row.1, "kb.model.created");
    assert_eq!(row.2["sku"], "Boss-AUDIT-CATALOG");
}
