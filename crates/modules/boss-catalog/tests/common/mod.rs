//! Shared test scaffolding for the knowledge-base crate.
//!
//! Provides:
//! - `KbTestApp` builder that wires InMemoryKb + HTTP router
//!   + RecordingEventBus + DomainPublisher + a stub AssetsClient
//! - `model_fixture()` and helpers to build valid AssetModel instances
//!
//! Delete-guard tests use the shipped `boss_assets_client::FakeAssetsClient`.

#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use boss_assets_client::{AssetsClient, AssetsClientError};
use boss_catalog::InMemoryKb;
use boss_catalog::http::{KbApiState, router};
use boss_catalog::types::*;
use boss_core::publisher::DomainPublisher;
#[cfg(feature = "postgres")]
use boss_events::PgAuditWriter;
use boss_testing::RecordingEventBus;
#[cfg(feature = "postgres")]
use sqlx::PgPool;

/// Stub AssetsClient that reports zero for every call. Used by
/// kb tests that don't exercise the delete guard path — they
/// just need a client that satisfies the trait bound. Tests that
/// exercise the guard use `boss_assets_client::FakeAssetsClient` instead.
struct StubAssetsClient;

#[async_trait]
impl AssetsClient for StubAssetsClient {
    async fn open_ticket_count_for_account(
        &self,
        _account_id: &str,
    ) -> Result<u64, AssetsClientError> {
        Ok(0)
    }
    async fn active_asset_count_for_sku(&self, _sku: &str) -> Result<u64, AssetsClientError> {
        Ok(0)
    }
    async fn ready_for_sale_count(&self) -> Result<u64, AssetsClientError> {
        Ok(0)
    }
}

/// A fully wired knowledge-base service for tests:
/// - InMemoryKb repository
/// - DomainPublisher backed by RecordingEventBus
/// - Axum Router ready to accept requests
pub struct KbTestApp {
    pub router: Router,
    pub bus: Arc<RecordingEventBus>,
}

impl KbTestApp {
    /// Build a fresh test app with an empty kb.
    pub fn new() -> Self {
        Self::with_models(vec![])
    }

    /// Build a test app pre-populated with the given models.
    pub fn with_models(models: Vec<AssetModel>) -> Self {
        let catalog = Arc::new(InMemoryKb::new(models));
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone(), "kb");
        let state = KbApiState {
            catalog,
            publisher: Some(publisher),
            assets_client: Arc::new(StubAssetsClient),
            classes_client: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self { router, bus }
    }

    /// Build a test app whose publisher persists every emitted event
    /// to the given Postgres pool's `audit_log` table. Use this from
    /// integration tests that exercise the full HTTP → publisher →
    /// audit-log chain.
    #[cfg(feature = "postgres")]
    pub fn with_audit_pool(pool: PgPool) -> Self {
        let catalog = Arc::new(InMemoryKb::new(vec![]));
        let bus = RecordingEventBus::new();
        let publisher =
            DomainPublisher::new(bus.clone(), "kb").with_audit(Arc::new(PgAuditWriter::new(pool)));
        let state = KbApiState {
            catalog,
            publisher: Some(publisher),
            assets_client: Arc::new(StubAssetsClient),
            classes_client: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self { router, bus }
    }
}

/// Build a valid AssetModel suitable for create/update tests.
/// Provides sensible defaults for every required field.
pub fn model_fixture(sku: &str) -> AssetModel {
    AssetModel {
        sku: sku.to_string(),
        name: format!("Test Model {sku}"),
        manufacturer: "TestCo".to_string(),
        model_year: 2024,
        category: DeviceCategory::new("router"),
        extras: serde_json::json!({"port_count": 24}),
        physical: Physical {
            width_cm: 50.0,
            depth_cm: 50.0,
            height_cm: 100.0,
            weight_kg: 80.0,
            power_requirements: "120V".to_string(),
        },
        regulatory: Regulatory {
            clearance_id: None,
            clearance_date: None,
            regulator_device_class: 2,
        },
        commerce: Commerce {
            list_price_new_cents: 5_000_000,
            typical_refurb_price_cents: None,
            currency: "USD".to_string(),
            lead_time_days: None,
            tagline: "Fixture model for tests".to_string(),
            description: "A model used by automated tests.".to_string(),
            use_cases: vec![],
            hero_image: None,
        },
        service: ServiceProfile {
            preventive_maintenance_hours: 2.0,
            preventive_maintenance_interval_months: 6,
            calibration_interval_months: 12,
            required_skill_level: 3,
            depot_required: false,
            common_failure_modes: vec![],
            pm_checklist: vec![],
        },
        spare_parts: vec![],
        consumables: vec![],
        documents: vec![],
        end_of_support: None,
        current_firmware: None,
    }
}
