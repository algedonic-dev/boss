//! End-to-end test for the inventory → audit_log chain, on the REAL
//! pipeline (outbox phase 2): the HTTP handler records the event on
//! the transactional outbox inside the domain tx; the relay drain
//! moves it to audit_log (chained) + the bus.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_inventory::PgInventory;
use boss_inventory::http::{InventoryApiState, router};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};

#[tokio::test(flavor = "multi_thread")]
async fn create_vendor_lands_in_audit_log() {
    let db = TestDb::new().await;
    // The REAL storage chain: the Pg repository records the event on
    // the outbox inside the vendor-create transaction. No publisher —
    // the handler's stamp falls back to source="inventory".
    let state = InventoryApiState {
        inventory: Arc::new(PgInventory::new(db.pool.clone())),
        publisher: None,
        clients: None,
        classes_client: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    let app_router = router(state);

    let body = serde_json::json!({
        "name": "Audit Test Vendor",
        "contact_name": "Jane Tester",
        "contact_email": "jane@audit.example",
        "city": "Austin",
        "state": "TX",
        "lead_time_days": 14,
        "payment_terms": "net-30",
        "category": "parts",
    });
    TestRequest::post("/api/inventory/vendors")
        .json(&body)
        .send(&app_router)
        .await
        .assert_status(StatusCode::CREATED);

    // Drain the outbox through the relay pipeline: outbox →
    // audit_log (chained) → bus → delivered.
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
    assert!(stats.delivered >= 1, "the POST queued at least one event");

    let row: (String, String) = sqlx::query_as(
        "SELECT source, kind FROM audit_log \
         WHERE kind = 'inventory.vendor.created' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST + relay drain");

    assert_eq!(row.0, "inventory");
    assert_eq!(row.1, "inventory.vendor.created");
}
