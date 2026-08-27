//! End-to-end test for the shipping → audit_log chain, on the REAL
//! pipeline (outbox phase 2): the POST records the event on the
//! transactional outbox inside the adapter's tx; the relay drain
//! moves it to audit_log. Deliberately NO direct audit writer — this
//! test only passes through the real outbox → relay → audit_log path.

mod common;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_shipping::PgShipping;
use boss_shipping::http::{ShippingApiState, router};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use common::shipment_fixture;
use sqlx::PgPool;

fn build_app(pool: PgPool) -> Router {
    // No publisher: the handler stamp falls back to source="shipping"
    // and the event records on the outbox.
    router(ShippingApiState {
        shipping: Arc::new(PgShipping::new(pool)),
        publisher: None,
        classes_client: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn create_shipment_lands_in_audit_log() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    let ship = shipment_fixture("ship-audit-test");
    TestRequest::post("/api/shipping/shipments")
        .json(&ship)
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

    let row: (String, String) = sqlx::query_as(
        "SELECT source, kind FROM audit_log \
         WHERE kind = 'shipping.shipment.created' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST + drain");

    assert_eq!(row.0, "shipping");
    assert_eq!(row.1, "shipping.shipment.created");
}
