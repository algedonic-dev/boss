#![allow(dead_code)] // tests/common/ helpers used selectively across test files

//! Shared test scaffolding for the shipping crate.
//!
//! Provides:
//! - `ShippingTestApp` builder that wires InMemoryShipping + HTTP router
//!   + RecordingEventBus + DomainPublisher
//! - `shipment_fixture()` helper to build a valid Shipment
//!
//! Usage in a test:
//! ```ignore
//! let app = ShippingTestApp::new();
//! let ship = shipment_fixture("ship-test-1");
//! let resp = TestRequest::post("/api/shipping/shipments")
//!     .json(&ship).send(&app.router).await;
//! resp.assert_status(StatusCode::CREATED);
//! app.bus.assert_event_emitted("shipping.shipment.created");
//! ```

use std::sync::Arc;

use axum::Router;
use boss_core::publisher::DomainPublisher;
#[cfg(feature = "postgres")]
use boss_events::PgAuditWriter;
use boss_shipping::http::{ShippingApiState, router};
use boss_shipping::in_memory::InMemoryShipping;
use boss_shipping::types::*;
use boss_testing::RecordingEventBus;
#[cfg(feature = "postgres")]
use sqlx::PgPool;

/// A fully wired shipping service for tests:
/// - InMemoryShipping repository
/// - DomainPublisher backed by RecordingEventBus
/// - Axum Router ready to accept requests
pub struct ShippingTestApp {
    pub router: Router,
    #[allow(dead_code)]
    pub bus: Arc<RecordingEventBus>,
}

impl ShippingTestApp {
    /// Build a fresh test app with an empty shipment list.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::with_shipments(vec![])
    }

    /// Build a test app pre-populated with the given shipments.
    #[allow(dead_code)]
    pub fn with_shipments(shipments: Vec<Shipment>) -> Self {
        let shipping = Arc::new(InMemoryShipping::new(shipments));
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone(), "shipping");
        let state = ShippingApiState {
            shipping,
            publisher: Some(publisher),
            classes_client: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self { router, bus }
    }

    /// Build a test app whose publisher persists every emitted event
    /// to the given Postgres pool's `audit_log` table. Used by the
    /// audit_log E2E integration test.
    #[cfg(feature = "postgres")]
    pub fn with_audit_pool(pool: PgPool) -> Self {
        let shipping = Arc::new(InMemoryShipping::new(vec![]));
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone(), "shipping")
            .with_audit(Arc::new(PgAuditWriter::new(pool)));
        let state = ShippingApiState {
            shipping,
            publisher: Some(publisher),
            classes_client: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self { router, bus }
    }
}

/// Build a valid Shipment suitable for create/update tests.
#[allow(dead_code)]
pub fn shipment_fixture(id: &str) -> Shipment {
    Shipment {
        id: id.to_string(),
        direction: ShipmentDirection::Outbound,
        status: ShipmentStatus::IN_TRANSIT.into(),
        carrier: Some(Carrier::new("fedex")),
        tracking_number: Some("1Z999AA10123456784".to_string()),
        origin: "HQ Warehouse".to_string(),
        destination: "Account Alpha".to_string(),
        asset_ids: vec!["SN-001".to_string()],
        line_items: Vec::new(),
        po_id: None,
        order_id: Some("ORD-200".to_string()),
        account_id: Some("account-001".to_string()),
        created_on: chrono::NaiveDate::from_ymd_opt(2025, 6, 1).unwrap(),
        shipped_on: Some(chrono::NaiveDate::from_ymd_opt(2025, 6, 2).unwrap()),
        estimated_delivery: Some(chrono::NaiveDate::from_ymd_opt(2025, 6, 5).unwrap()),
        delivered_on: None,
    }
}
