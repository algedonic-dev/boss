//! Hexagonal port: what boss-ml needs from persistence.

use async_trait::async_trait;

use crate::types::{CreatePredictionInput, MlModel, MlModelSummary, MlPrediction, ModelStatus};

#[derive(Debug, thiserror::Error)]
pub enum MlError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
}

/// A listing's `limit`, refused when negative. Until the adapters-agree
/// suite (backlog be459ab9) Postgres answered `LIMIT -1` with its own
/// error text as a `Storage` failure while the in-memory adapter cast
/// it to `usize` and answered every row; both adapters now ask this one
/// function, so the refusal is one sentence.
pub(crate) fn checked_limit(limit: i64) -> Result<i64, MlError> {
    if limit < 0 {
        return Err(MlError::BadRequest(format!(
            "limit must not be negative, got {limit}"
        )));
    }
    Ok(limit)
}

/// The refusal of a model whose `(name, version)` another id already
/// holds — the table's `UNIQUE (name, version)`. Postgres used to
/// surface the raw constraint text as a `Storage` failure (a 500) and
/// the in-memory adapter wrote the second row (backlog be459ab9).
pub(crate) fn name_version_taken(name: &str, version: &str, holder: &str) -> MlError {
    MlError::BadRequest(format!(
        "model {name} version {version} is already registered as {holder}"
    ))
}

#[async_trait]
pub trait MlRepository: Send + Sync {
    /// List all models, optionally filtered by status, byte-ordered by
    /// name and then by id. Each row
    /// includes the derived `predictions_24h` count and the
    /// timestamp of the most recent prediction.
    async fn all_model_summaries(
        &self,
        status: Option<ModelStatus>,
    ) -> Result<Vec<MlModelSummary>, MlError>;

    /// Fetch a single model summary by id.
    async fn model_summary_by_id(&self, id: &str) -> Result<Option<MlModelSummary>, MlError>;

    /// Upsert a model by id. Used by the bootstrap seed path on
    /// service startup; idempotent across restarts. A re-upsert keeps
    /// the FIRST `created_at`; a `(name, version)` held by another id
    /// is refused as `BadRequest` and writes nothing. Instants are kept
    /// to the microsecond.
    async fn upsert_model(&self, model: &MlModel) -> Result<(), MlError>;

    /// Create a prediction. Idempotent via `id` — re-POSTing the
    /// same id is a no-op (ON CONFLICT DO NOTHING). Returns the
    /// canonical stored row.
    async fn create_prediction(
        &self,
        input: &CreatePredictionInput,
    ) -> Result<MlPrediction, MlError>;

    /// List predictions for a specific `(entity_type, entity_id)`
    /// pair. Ordered by `created_at DESC` then id, capped by `limit`
    /// (a negative limit is `BadRequest`).
    async fn predictions_for_entity(
        &self,
        entity_type: &str,
        entity_id: &str,
        limit: i64,
    ) -> Result<Vec<MlPrediction>, MlError>;

    /// List recent predictions for a model. Ordered by
    /// `created_at DESC` then id, capped by `limit` (a negative limit
    /// is `BadRequest`).
    async fn recent_predictions_for_model(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<MlPrediction>, MlError>;
}
