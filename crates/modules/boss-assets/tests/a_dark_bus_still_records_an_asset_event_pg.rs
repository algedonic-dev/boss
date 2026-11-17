//! A dark message bus loses no asset event (backlog 2e45840a, design
//! ad9519bc part d, second half).
//!
//! Outage 05fd165a, 2026-11-09 23:01Z, darkened NATS while Postgres, the
//! system of record, stayed up. Car 1 of the design made a publish on a
//! dark bus fail fast, so the two asset event doors — which appended to
//! `asset_events` and then published the fact straight to the bus —
//! logged ERROR and DROPPED the fact: the asset moved and nothing
//! downstream ever heard. They were the last write paths whose facts a
//! dark bus lost.
//!
//! This builds the assets API the way the binary does — `PgAssets` over
//! a bus from `NatsEventBus::connect` — against an address nothing
//! listens on, and holds: both doors' writes commit, each stored event's
//! fact is held in `event_outbox` (a redelivered one is not staged
//! twice), and a relay pass over the dark bus leaves the facts owed
//! rather than stamping a delivery that never left the process.

use std::sync::Arc;

use axum::http::StatusCode;
use boss_assets::http::{AssetsApiState, router};
use boss_assets::sse::SseHub;
use boss_assets::types::{AssetEvent, AssetEventId, AssetEventKind, AssetId, IntakeSource};
use boss_assets::{AssetsRepository, PgAssets};
use boss_core::port::EventBus;
use boss_nats::NatsEventBus;
use boss_people_client::AlwaysExistsPeople;
use boss_policy_client::PermissivePolicyClient;
use boss_testing::{TestDb, TestRequest};
use chrono::NaiveDate;

fn received(id: &str, serial: &str) -> AssetEvent {
    AssetEvent {
        id: AssetEventId::new(id),
        asset_id: AssetId::new(serial),
        ts: NaiveDate::from_ymd_opt(2026, 11, 10).unwrap(),
        actor_id: boss_core::actor::ActorId::human("emp-dark-bus"),
        kind: AssetEventKind::Received {
            sku: None,
            source: IntakeSource::new("oem-new"),
            oem_serial: None,
        },
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dark_bus_holds_both_doors_asset_facts_in_the_outbox() {
    let db = TestDb::new().await;
    let dark = boss_testing::dark_port();
    let bus = Arc::new(
        NatsEventBus::connect(&format!("nats://{}", dark.addr))
            .await
            .expect("a dark bus is not a boot failure"),
    );
    let assets = Arc::new(PgAssets::new(db.pool.clone()));
    let app = router(AssetsApiState {
        assets: assets.clone(),
        bus: bus.clone(),
        people_client: Arc::new(AlwaysExistsPeople),
        classes_client: None,
        hub: SseHub::new(),
        policy: Arc::new(PermissivePolicyClient),
        insights_clients: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    });

    TestRequest::post("/api/assets/events")
        .json(&received("evt-dark-1", "SN-DARK-1"))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);
    // The batch door: two new events and a redelivery of the first,
    // which is counted a duplicate and owes no second fact.
    TestRequest::post("/api/assets/events/batch")
        .json(&vec![
            received("evt-dark-2", "SN-DARK-2"),
            received("evt-dark-3", "SN-DARK-2"),
            received("evt-dark-1", "SN-DARK-1"),
        ])
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    for (serial, n) in [("SN-DARK-1", 1), ("SN-DARK-2", 2)] {
        let stored = assets.events_for(&AssetId::new(serial)).await.unwrap();
        assert_eq!(
            stored.len(),
            n,
            "the writes commit on a dark bus: {stored:?}"
        );
    }

    boss_events::outbox::drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 500)
        .await
        .expect("a relay pass over a dark bus is not an error");

    let held: Vec<(String, serde_json::Value, bool)> = sqlx::query_as(
        "SELECT kind, payload, delivered_at IS NULL FROM event_outbox \
         WHERE source = 'assets' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let ids: Vec<&str> = held
        .iter()
        .map(|(_, payload, _)| payload["id"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        ids,
        ["evt-dark-1", "evt-dark-2", "evt-dark-3"],
        "one fact held per stored event, none dropped and none staged twice: {held:?}"
    );
    for (kind, payload, undelivered) in &held {
        assert_eq!(kind, "asset.received");
        assert_eq!(payload["_actor"], "emp-dark-bus", "{payload}");
        assert!(
            *undelivered,
            "the bus is dark, so the fact is owed, not stamped delivered: {payload}"
        );
    }
}
