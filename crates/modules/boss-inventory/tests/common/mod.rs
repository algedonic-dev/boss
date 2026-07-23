#![allow(dead_code)] // tests/common/ helpers used selectively across test files

//! Shared test scaffolding for the inventory crate.
//!
//! Provides:
//! - `InventoryTestApp` builder that wires InMemoryInventory + HTTP router
//!   + RecordingEventBus + DomainPublisher
//! - `vendor_fixture()` and `purchase_order_fixture()` for test data
//!
//! Usage in a test:
//! ```ignore
//! let app = InventoryTestApp::new();
//! let resp = TestRequest::post("/api/inventory/vendors")
//!     .json(&serde_json::json!({ "name": "...", ... }))
//!     .send(&app.router).await;
//! resp.assert_status(StatusCode::CREATED);
//! app.bus.assert_event_emitted("inventory.vendor.created");
//! ```

use std::sync::Arc;

use axum::Router;
use boss_core::publisher::DomainPublisher;
use boss_inventory::http::{InventoryApiState, router};
use boss_inventory::in_memory::InMemoryInventory;
use boss_inventory::types::*;
use boss_testing::RecordingEventBus;

/// A fully wired inventory service for tests:
/// - InMemoryInventory repository
/// - DomainPublisher backed by RecordingEventBus
/// - Axum Router ready to accept requests
pub struct InventoryTestApp {
    pub router: Router,
    #[allow(dead_code)]
    pub bus: Arc<RecordingEventBus>,
    /// The in-memory repository — write paths RECORD their events
    /// here (outbox phase 2), so HTTP-tier tests assert against
    /// `recorded_events()` instead of the bus (handlers no longer
    /// publish post-commit).
    pub inventory: Arc<InMemoryInventory>,
}

impl InventoryTestApp {
    /// Assert a write path recorded an event of `kind`; returns it.
    #[allow(dead_code)]
    pub fn assert_event_recorded(&self, kind: &str) -> boss_core::event::Event {
        let events = self.inventory.recorded_events();
        match events.iter().find(|e| e.kind == kind) {
            Some(e) => e.clone(),
            None => {
                let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
                panic!(
                    "\n  expected recorded event kind: {}\n  events actually recorded: {:?}\n  total events: {}\n",
                    kind,
                    kinds,
                    events.len(),
                );
            }
        }
    }

    /// Assert no event of `kind` was recorded by any write path.
    #[allow(dead_code)]
    pub fn assert_event_not_recorded(&self, kind: &str) {
        if let Some(e) = self
            .inventory
            .recorded_events()
            .into_iter()
            .find(|e| e.kind == kind)
        {
            panic!(
                "\n  expected event kind {} NOT to be recorded\n  but it was recorded with payload: {}\n",
                kind, e.payload,
            );
        }
    }
}

impl InventoryTestApp {
    /// Build a fresh test app with no items, orders, or vendors.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::build(vec![default_item("PART-001")], vec![], vec![])
    }

    /// Build a test app pre-populated with the given vendors.
    #[allow(dead_code)]
    pub fn with_vendors(vendors: Vec<Vendor>) -> Self {
        Self::build(vec![default_item("PART-001")], vec![], vendors)
    }

    /// Build a test app pre-populated with the given purchase orders.
    #[allow(dead_code)]
    pub fn with_orders(orders: Vec<PurchaseOrder>) -> Self {
        Self::build(vec![default_item("PART-001")], orders, vec![])
    }

    fn build(items: Vec<InventoryItem>, orders: Vec<PurchaseOrder>, vendors: Vec<Vendor>) -> Self {
        let inventory = Arc::new(InMemoryInventory::with_vendors(items, orders, vendors));
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone(), "inventory");
        let state = InventoryApiState {
            inventory: inventory.clone(),
            publisher: Some(publisher),
            clients: None,
            classes_client: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self {
            router,
            bus,
            inventory,
        }
    }
}

/// Build a valid Vendor suitable for update/delete tests.
#[allow(dead_code)]
pub fn vendor_fixture(id: &str) -> Vendor {
    Vendor {
        id: id.to_string(),
        name: Some(format!("Test Vendor {id}")),
        contact_name: Some("Jane Tester".to_string()),
        contact_email: Some("jane@testvendor.example".to_string()),
        city: Some("Austin".to_string()),
        state: Some("TX".to_string()),
        lead_time_days: 14,
        payment_terms: Some("Net 30".to_string()),
        category: Some("parts".to_string()),
        behavior: None,
    }
}

/// Build a valid PurchaseOrder suitable for tests.
#[allow(dead_code)]
pub fn purchase_order_fixture(id: &str) -> PurchaseOrder {
    PurchaseOrder {
        id: id.to_string(),
        vendor: Some("Acme Parts Co".to_string()),
        status: PoStatus::Submitted,
        placed_on: Some(chrono::NaiveDate::from_ymd_opt(2025, 3, 1).unwrap()),
        expected_on: Some(chrono::NaiveDate::from_ymd_opt(2025, 3, 15).unwrap()),
        received_on: None,
        lines: vec![PurchaseOrderLine {
            part_sku: "PART-001".to_string(),
            qty: 25,
            unit_cost_cents: 15_000,
            currency: "USD".to_string(),
        }],
    }
}

fn default_item(sku: &str) -> InventoryItem {
    InventoryItem {
        part_sku: sku.to_string(),
        bin: "A-01".to_string(),
        on_hand: 50,
        allocated: 10,
        reorder_point: 20,
        reorder_qty: 100,
        trailing_90d_usage: 30,
        value_cents: 0,
        avg_cost_cents: 0,
        vendor_price_cents: None,
        vendor_category: None,
    }
}
