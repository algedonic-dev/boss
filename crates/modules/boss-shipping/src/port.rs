//! Hexagonal port: `ShippingRepository` defines what the domain needs from
//! persistence.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::types::{Shipment, ShipmentDirection};

#[derive(Debug, thiserror::Error)]
pub enum ShippingError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

/// Persistence port for shipments.
///
/// Mutation methods come in two flavors: a convenience overload
/// that stamps `Utc::now()` server-side, and an `_at` variant that
/// takes an explicit timestamp. Handlers that emit a domain event
/// for the same mutation use `_at` so the projection write and the
/// audit_log event share one timestamp — required for the
/// audit_log → projection rebuild path. See
/// `docs/design/projection-rebuilders.md`.
#[async_trait]
pub trait ShippingRepository: Send + Sync {
    /// Return every shipment.
    async fn all_shipments(&self) -> Result<Vec<Shipment>, ShippingError>;

    /// Return a page of shipments with total count.
    /// `account_id` filters to a single account when `Some`. The account
    /// detail view uses this to scope the shipments section.
    async fn list_shipments(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<Shipment>, i64), ShippingError>;

    /// Return a single shipment by ID, or `None` if not found.
    async fn shipment_by_id(&self, id: &str) -> Result<Option<Shipment>, ShippingError>;

    /// Create a new shipment. Returns the ID. Errors if ID already exists.
    async fn create_shipment(&self, shipment: &Shipment) -> Result<String, ShippingError> {
        self.create_shipment_at(shipment, Utc::now()).await
    }
    async fn create_shipment_at(
        &self,
        shipment: &Shipment,
        now: DateTime<Utc>,
    ) -> Result<String, ShippingError>;

    /// Replace a shipment by ID. Errors if ID doesn't exist.
    async fn update_shipment(&self, id: &str, shipment: &Shipment) -> Result<(), ShippingError> {
        self.update_shipment_at(id, shipment, Utc::now()).await
    }
    async fn update_shipment_at(
        &self,
        id: &str,
        shipment: &Shipment,
        now: DateTime<Utc>,
    ) -> Result<(), ShippingError>;

    /// Delete a shipment and satellite data. Errors if ID doesn't exist.
    async fn delete_shipment(&self, id: &str) -> Result<(), ShippingError>;

    /// Record one carrier scan for a shipment + roll up the
    /// shipment's `status` column when the scan moves it to a
    /// row-state-changing value (in-transit, delivered).
    /// Idempotent on (shipment_id, status, occurred_on).
    /// Errors with `NotFound` when the shipment doesn't exist
    /// (allows the HTTP layer to skip cleanly on out-of-order
    /// scan delivery).
    async fn record_tracking_scan(
        &self,
        shipment_id: &str,
        status: &str,
        occurred_on: chrono::NaiveDate,
        stage_index: Option<i16>,
    ) -> Result<(), ShippingError>;

    /// Aggregate status summary for one direction — counts per status
    /// (in-flight only) + count of deliveries in the trailing 7 days +
    /// a top-N preview of recent rows (in-flight first, then recently
    /// delivered). Postgres backends should implement this with a
    /// GROUP BY + bounded LIMIT rather than fetching the full table
    /// and aggregating in Rust — at scale the shipments table reaches
    /// tens of thousands of rows and full-table scans trip the 5s
    /// client timeout. See examples/used-device-shop/design/operations-needs.md E1 perf
    /// note.
    async fn status_summary(
        &self,
        direction: ShipmentDirection,
        today: chrono::NaiveDate,
        recent_limit: i64,
    ) -> Result<boss_shipping_client::OutboundShipmentSummary, ShippingError>;
}
