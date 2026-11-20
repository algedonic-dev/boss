//! End-to-end test for the assets → audit_log chain.
//!
//! Since 2026-11-10 the chain runs through the transactional outbox: a
//! door stages the fact in the asset write's own transaction and the
//! relay writes `audit_log` and publishes (backlog 2e45840a, design
//! ad9519bc part d). Until then the doors' publisher wrote `audit_log`
//! directly, after the asset write had committed.

mod common;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_assets::PgAssets;
use boss_assets::http::{AssetsApiState, router};
use boss_assets::sse::SseHub;
use boss_assets::types::{AssetEvent, AssetEventKind, IntakeSource};
use boss_core::port::EventBus;
use boss_people_client::AlwaysExistsPeople;
use boss_policy_client::PermissivePolicyClient;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use common::system_event_fixture;

/// A `Received` naming no catalog model, so the store needs none seeded.
fn received(id: &str, serial: &str) -> AssetEvent {
    system_event_fixture(
        id,
        serial,
        AssetEventKind::Received {
            sku: None,
            source: IntakeSource::new("oem-new"),
            oem_serial: None,
        },
    )
}

fn app(pool: sqlx::PgPool, bus: Arc<RecordingEventBus>) -> Router {
    router(AssetsApiState {
        assets: Arc::new(PgAssets::new(pool)),
        bus,
        people_client: Arc::new(AlwaysExistsPeople),
        classes_client: None,
        hub: SseHub::new(),
        policy: Arc::new(PermissivePolicyClient),
        insights_clients: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    })
}

/// One relay pass, as `boss-event-relay` runs it.
async fn relay(pool: &sqlx::PgPool, bus: &Arc<RecordingEventBus>) {
    let bus = bus.clone() as Arc<dyn EventBus>;
    boss_events::outbox::drain_outbox_once(pool, &bus, 500)
        .await
        .expect("a relay pass");
}

#[tokio::test(flavor = "multi_thread")]
async fn post_event_lands_in_audit_log() {
    let db = TestDb::new().await;
    let bus = RecordingEventBus::new();
    let app = app(db.pool.clone(), bus.clone());

    TestRequest::post("/api/assets/events")
        .json(&received("evt-audit-1", "SN-AUDIT-1"))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);
    relay(&db.pool, &bus).await;

    let row: (String, String) = sqlx::query_as(
        "SELECT source, kind FROM audit_log \
         WHERE kind = 'asset.received' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST");

    assert_eq!(row.0, "assets");
    assert_eq!(row.1, "asset.received");
    bus.assert_event_emitted("asset.received");
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_events_each_land_in_audit_log() {
    // The bulk path stages one fact per stored event, so audit_log
    // captures every event in the batch.
    let db = TestDb::new().await;
    let bus = RecordingEventBus::new();
    let app = app(db.pool.clone(), bus.clone());

    let events = vec![
        received("evt-batch-a-1", "SN-BATCH-A-1"),
        received("evt-batch-a-2", "SN-BATCH-A-2"),
        received("evt-batch-a-3", "SN-BATCH-A-3"),
    ];

    TestRequest::post("/api/assets/events/batch")
        .json(&events)
        .send(&app)
        .await
        .assert_status(StatusCode::OK);
    relay(&db.pool, &bus).await;

    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM audit_log WHERE kind = 'asset.received'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 3, "every event in the batch must land in audit_log");
}
