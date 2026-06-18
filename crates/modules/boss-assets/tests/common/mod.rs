//! Shared test scaffolding for the assets crate.
//!
//! Uses `RecordingEventBus` in place of NATS so tests are hermetic and
//! can assert on published events directly.
//!
//! `#[allow(dead_code)]` on the module: Rust sees each test binary as
//! its own compilation, and some helpers are only used by a subset of
//! the binaries — the unused-in-this-binary ones trigger `dead_code`
//! even though every helper is used somewhere. Crate-wide allow is
//! the cleanest fix without per-item tagging.
#![allow(dead_code)]

use std::sync::Arc;

use axum::Router;
use boss_assets::http::{AssetsApiState, router};
use boss_assets::in_memory::InMemoryAssets;
use boss_assets::sse::SseHub;
use boss_assets::types::{AssetEvent, AssetEventId, AssetEventKind, AssetId, IntakeSource};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
#[cfg(feature = "postgres")]
use boss_events::PgAuditWriter;
use boss_people_client::{AlwaysExistsPeople, PeopleClient};
use boss_policy_client::{PermissivePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
#[cfg(feature = "postgres")]
use sqlx::PgPool;

/// Fully wired assets service for tests.
pub struct AssetsTestApp {
    pub router: Router,
    pub assets: Arc<InMemoryAssets>,
    #[allow(dead_code)]
    pub bus: Arc<RecordingEventBus>,
}

impl AssetsTestApp {
    /// Build a fresh test app with an empty assets repository. Uses a
    /// permissive fake people client that says every actor exists, so
    /// existing tests don't need to know about the guard.
    pub async fn new() -> Self {
        Self::with_events(vec![]).await
    }

    /// Build a test app pre-populated with the given events.
    pub async fn with_events(events: Vec<AssetEvent>) -> Self {
        let people: Arc<dyn PeopleClient> = Arc::new(AlwaysExistsPeople);
        Self::with_events_and_people(events, people).await
    }

    /// Build a test app with an explicit `PeopleClient`. Lets a test
    /// inject a fake that knows specific employees, rejects unknowns,
    /// or fails closed.
    pub async fn with_events_and_people(
        events: Vec<AssetEvent>,
        people_client: Arc<dyn PeopleClient>,
    ) -> Self {
        let assets = Arc::new(
            InMemoryAssets::with_events(events).expect("seed events should be non-duplicate"),
        );
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone() as Arc<dyn EventBus>, "assets");
        let hub = SseHub::new();
        let state = AssetsApiState {
            assets: assets.clone(),
            bus: bus.clone(),
            publisher,
            people_client,
            classes_client: None,
            hub,
            policy: Some(Arc::new(PermissivePolicyClient) as Arc<dyn PolicyClient>),
            insights_clients: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self {
            router,
            assets,
            bus,
        }
    }

    /// Build a test app whose publisher persists every emitted event
    /// to the given Postgres pool's `audit_log` table. Used by the
    /// audit_log E2E integration test.
    #[cfg(feature = "postgres")]
    pub async fn with_audit_pool(pool: PgPool) -> Self {
        let assets = Arc::new(InMemoryAssets::new());
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone() as Arc<dyn EventBus>, "assets")
            .with_audit(Arc::new(PgAuditWriter::new(pool)));
        let people_client: Arc<dyn PeopleClient> = Arc::new(AlwaysExistsPeople);
        let hub = SseHub::new();
        let state = AssetsApiState {
            assets: assets.clone(),
            bus: bus.clone(),
            publisher,
            people_client,
            classes_client: None,
            hub,
            policy: Some(Arc::new(PermissivePolicyClient) as Arc<dyn PolicyClient>),
            insights_clients: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self {
            router,
            assets,
            bus,
        }
    }
}

/// Build a valid `AssetEvent` with the given id, serial, and kind.
pub fn system_event_fixture(id: &str, serial: &str, kind: AssetEventKind) -> AssetEvent {
    AssetEvent {
        id: AssetEventId::new(id),
        asset_id: AssetId::new(serial),
        ts: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        actor_id: boss_core::actor::ActorId::human("emp-test"),
        kind,
    }
}

/// Convenience: a `Received` custody event.
pub fn received_event(id: &str, serial: &str) -> AssetEvent {
    received_event_for_sku(id, serial, "Boss-TEST-2024")
}

/// `Received` event with an explicit SKU. Use this when the test
/// cares which catalog model the device is an instance of (e.g.,
/// `active_asset_count_for_sku` tests).
pub fn received_event_for_sku(id: &str, serial: &str, sku: &str) -> AssetEvent {
    system_event_fixture(
        id,
        serial,
        AssetEventKind::Received {
            sku: Some(sku.to_string()),
            source: IntakeSource::new("oem-new"),
            oem_serial: None,
        },
    )
}

/// Convenience: an `Installed` commerce event.
#[allow(dead_code)]
pub fn installed_event(id: &str, serial: &str, account_id: &str) -> AssetEvent {
    system_event_fixture(
        id,
        serial,
        AssetEventKind::Installed {
            account_id: account_id.to_string(),
        },
    )
}

/// Convenience: a `PutAway` custody event.
pub fn put_away_event(id: &str, serial: &str, bin: &str) -> AssetEvent {
    system_event_fixture(
        id,
        serial,
        AssetEventKind::PutAway {
            bin: bin.to_string(),
        },
    )
}
