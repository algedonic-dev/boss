//! Hexagonal port: `KbRepository` defines what the domain needs from
//! persistence. Adapters (in-memory for tests, Postgres for prod) implement
//! this trait.

use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};

use crate::types::{AssetModel, PartCatalogRow};

#[derive(Debug, thiserror::Error)]
pub enum KbError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// Caller-supplied data failed validation (e.g. extras blob
    /// rejected by the per-category JSON schema).
    #[error("bad request: {0}")]
    BadRequest(String),
}

/// Persistence port for the knowledge base.
///
/// The knowledge base is slow-changing reference data (~20 models today).
/// Implementations may cache aggressively for reads.
///
/// Mutation methods come in two flavors: a convenience overload that
/// stamps `Utc::now()` server-side (with a platform-automation event
/// stamp — test-path ergonomics), and an `_at` variant for the
/// audit_log → projection rebuild path. See
/// `docs/design/projection-rebuilders.md`.
///
/// OUTBOX (phase 2): every mutation records its domain event on the
/// transactional outbox INSIDE the adapter transaction via the stamp
/// (`boss_events::outbox::record_event_in_tx`); boss-event-relay
/// delivers to audit_log + NATS post-commit. Nothing publishes
/// post-commit.
#[async_trait]
pub trait KbRepository: Send + Sync {
    /// Return every device model in the knowledge base.
    async fn all_models(&self) -> Result<Vec<AssetModel>, KbError>;

    /// Return a single model by SKU, or `None` if it doesn't exist.
    async fn model_by_sku(&self, sku: &str) -> Result<Option<AssetModel>, KbError>;

    /// Create a new device model. Returns the SKU. Errors if SKU already exists.
    /// Records `kb.model.created` (full row state) in-tx.
    async fn create_model(&self, model: &AssetModel) -> Result<String, KbError> {
        let stamp = EventStamp::new("kb", ActorId::Automation("platform".into()));
        self.create_model_at(model, stamp.timestamp, &stamp).await
    }
    async fn create_model_at(
        &self,
        model: &AssetModel,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<String, KbError>;

    /// Replace a device model by SKU. Errors if SKU doesn't exist.
    /// Records `kb.model.updated` (full row state) in-tx.
    async fn update_model(&self, sku: &str, model: &AssetModel) -> Result<(), KbError> {
        let stamp = EventStamp::new("kb", ActorId::Automation("platform".into()));
        self.update_model_at(sku, model, stamp.timestamp, &stamp)
            .await
    }
    async fn update_model_at(
        &self,
        sku: &str,
        model: &AssetModel,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), KbError>;

    /// Delete a device model and all satellite data. Errors if SKU doesn't exist.
    /// Records `kb.model.deleted` (`{sku, deleted_at}`) in-tx.
    async fn delete_model(&self, sku: &str) -> Result<(), KbError> {
        let stamp = EventStamp::new("kb", ActorId::Automation("platform".into()));
        self.delete_model_at(sku, stamp.timestamp, &stamp).await
    }
    async fn delete_model_at(
        &self,
        sku: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), KbError>;

    /// Return every row in the `parts` table — independent of any
    /// `asset_models.spare_parts` / `asset_models.consumables`
    /// linkage. The brewery tenant carries parts (ingredients +
    /// packaging) without modeling them as device-asset
    /// satellites; this endpoint is the canonical "show me all
    /// the parts" surface for that case.
    async fn all_parts(&self) -> Result<Vec<PartCatalogRow>, KbError>;

    /// Return every row in the generic `documents` table for one
    /// (entity_kind, entity_id) pair. Used by the SPA's
    /// KnowledgeBaseView and the parts-detail page to surface
    /// vendor datasheets, COA scans, allergen tags, etc. — the
    /// "knowledge" layer for any entity that's not a system
    /// model satellite. Empty result is the not-yet-documented
    /// case; missing entity is not an error here.
    async fn documents_for(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<Vec<crate::types::EntityDocument>, KbError>;
}
