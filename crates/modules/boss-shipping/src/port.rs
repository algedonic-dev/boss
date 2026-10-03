//! Hexagonal port: `ShippingRepository` defines what the domain needs from
//! persistence.

use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
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
    /// The caller's request can never be stored as sent — a negative
    /// limit, a line item of no units, a NUL byte, an update whose body
    /// names another id. Named by the adapters-agree suite (backlog
    /// be459ab9): until 2026-10-01 each reached Postgres and came back
    /// as `Storage` (a 500) while the in-memory double accepted it.
    #[error("invalid: {0}")]
    Invalid(String),
}

/// Refuse a shipment no adapter can store as sent: a NUL byte in any
/// text field (Postgres TEXT cannot hold one) or a line item of no
/// units (the `qty > 0` CHECK). Both adapters call this one function so
/// they refuse the same things with the same words.
pub fn validate_shipment(s: &Shipment) -> Result<(), ShippingError> {
    let opt = |v: &Option<String>| v.clone().unwrap_or_default();
    let mut texts: Vec<(String, String)> = vec![
        ("id".into(), s.id.clone()),
        ("status".into(), s.status.as_str().into()),
        (
            "carrier".into(),
            s.carrier
                .as_ref()
                .map(|c| c.as_str().to_string())
                .unwrap_or_default(),
        ),
        ("tracking_number".into(), opt(&s.tracking_number)),
        ("origin".into(), s.origin.clone()),
        ("destination".into(), s.destination.clone()),
        ("po_id".into(), opt(&s.po_id)),
        ("order_id".into(), opt(&s.order_id)),
        ("account_id".into(), opt(&s.account_id)),
    ];
    texts.extend(
        s.asset_ids
            .iter()
            .enumerate()
            .map(|(i, a)| (format!("asset_ids[{i}]"), a.clone())),
    );
    for (i, line) in s.line_items.iter().enumerate() {
        texts.push((format!("line_items[{i}].sku"), line.sku.clone()));
        texts.push((
            format!("line_items[{i}].description"),
            opt(&line.description),
        ));
        if line.qty <= 0 {
            return Err(ShippingError::Invalid(format!(
                "line_items[{i}].qty must be positive, got {}",
                line.qty
            )));
        }
    }
    match texts.into_iter().find(|(_, v)| v.contains('\0')) {
        Some((field, _)) => Err(ShippingError::Invalid(format!(
            "{field} carries a NUL byte, which cannot be stored"
        ))),
        None => Ok(()),
    }
}

/// Refuse an update whose body names another id than the one it
/// updates: it would rename the row in one adapter and write a second
/// row in the other.
pub fn validate_update(id: &str, s: &Shipment) -> Result<(), ShippingError> {
    if s.id != id {
        return Err(ShippingError::Invalid(format!(
            "update of {id} carries a body naming {}",
            s.id
        )));
    }
    validate_shipment(s)
}

/// Refuse a negative page bound by name.
pub fn validate_bound(field: &str, n: i64) -> Result<(), ShippingError> {
    if n < 0 {
        return Err(ShippingError::Invalid(format!(
            "{field} must not be negative, got {n}"
        )));
    }
    Ok(())
}

/// Persistence port for shipments.
///
/// Mutation methods come in two flavors: a convenience overload
/// that stamps `Utc::now()` server-side (with a platform-automation
/// event stamp — test-path ergonomics), and an `_at` variant that
/// takes an explicit timestamp plus the caller's [`EventStamp`] so
/// the projection write and the audit_log event share one timestamp —
/// required for the audit_log → projection rebuild path. See
/// `docs/design/projection-rebuilders.md`.
///
/// OUTBOX (phase 2): every mutation records its domain event on the
/// transactional outbox INSIDE the adapter transaction via the
/// stamp (`boss_events::outbox::record_event_in_tx`);
/// boss-event-relay delivers to audit_log + NATS post-commit.
/// Nothing publishes post-commit.
#[async_trait]
pub trait ShippingRepository: Send + Sync {
    /// Return every shipment, newest `created_on` first, then by id in
    /// byte order. A shipment's asset ids answer in byte order, each
    /// once; its line items in authoring order. Held to both adapters by
    /// `tests/the_adapters_agree_on_the_shipment_store_pg.rs`.
    async fn all_shipments(&self) -> Result<Vec<Shipment>, ShippingError>;

    /// Return a page of shipments with total count, cut from the order
    /// of [`Self::all_shipments`]; a negative `limit` or `offset` is
    /// refused `Invalid`.
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

    /// Create a new shipment. Returns the ID. Errors `Conflict` if ID
    /// already exists, writing and recording nothing; a body
    /// [`validate_shipment`] refuses is `Invalid`.
    /// Records `shipping.shipment.created` (full row state) in-tx.
    async fn create_shipment(&self, shipment: &Shipment) -> Result<String, ShippingError> {
        let stamp = EventStamp::new("shipping", ActorId::Automation("platform".into()));
        self.create_shipment_at(shipment, stamp.timestamp, &stamp)
            .await
    }
    async fn create_shipment_at(
        &self,
        shipment: &Shipment,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<String, ShippingError>;

    /// Replace a shipment by ID. Errors if ID doesn't exist.
    /// Records `shipping.shipment.updated` (full row state) in-tx.
    async fn update_shipment(&self, id: &str, shipment: &Shipment) -> Result<(), ShippingError> {
        let stamp = EventStamp::new("shipping", ActorId::Automation("platform".into()));
        self.update_shipment_at(id, shipment, stamp.timestamp, &stamp)
            .await
    }
    async fn update_shipment_at(
        &self,
        id: &str,
        shipment: &Shipment,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), ShippingError>;

    /// Delete a shipment and satellite data. Errors if ID doesn't exist.
    /// Records `shipping.shipment.deleted` (`{id, deleted_at}`) in-tx.
    async fn delete_shipment(&self, id: &str) -> Result<(), ShippingError> {
        let stamp = EventStamp::new("shipping", ActorId::Automation("platform".into()));
        self.delete_shipment_at(id, stamp.timestamp, &stamp).await
    }
    async fn delete_shipment_at(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), ShippingError>;

    /// Record one carrier scan for a shipment + roll up the
    /// shipment's `status` column when the scan moves it to a
    /// row-state-changing value (in-transit, delivered).
    /// Idempotent on (shipment_id, status, occurred_on).
    /// Errors with `NotFound` when the shipment doesn't exist
    /// (allows the HTTP layer to skip cleanly on out-of-order
    /// scan delivery).
    /// Records `shipping.tracking.recorded` in-tx — and ONLY when
    /// the scan row actually inserted, so an idempotent replay
    /// records nothing (the guard sits ahead of the recording).
    async fn record_tracking_scan(
        &self,
        shipment_id: &str,
        status: &str,
        occurred_on: chrono::NaiveDate,
        stage_index: Option<i16>,
        stamp: &EventStamp,
    ) -> Result<(), ShippingError>;

    /// Aggregate status summary for one direction — counts per status
    /// (in-flight only) + count of deliveries in the trailing 7 days +
    /// a top-N preview of recent rows (in-flight first, then recently
    /// delivered). Postgres backends should implement this with a
    /// GROUP BY + bounded LIMIT rather than fetching the full table
    /// and aggregating in Rust — at scale the shipments table reaches
    /// tens of thousands of rows and full-table scans trip the 5s
    /// client timeout.
    async fn status_summary(
        &self,
        direction: ShipmentDirection,
        today: chrono::NaiveDate,
        recent_limit: i64,
    ) -> Result<boss_shipping_client::OutboundShipmentSummary, ShippingError>;
}
