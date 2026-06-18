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

#[async_trait]
pub trait MlRepository: Send + Sync {
    /// List all models, optionally filtered by status. Each row
    /// includes the derived `predictions_24h` count and the
    /// timestamp of the most recent prediction.
    async fn all_model_summaries(
        &self,
        status: Option<ModelStatus>,
    ) -> Result<Vec<MlModelSummary>, MlError>;

    /// Fetch a single model summary by id.
    async fn model_summary_by_id(&self, id: &str) -> Result<Option<MlModelSummary>, MlError>;

    /// Upsert a model by id. Used by the bootstrap seed path on
    /// service startup; idempotent across restarts.
    async fn upsert_model(&self, model: &MlModel) -> Result<(), MlError>;

    /// Create a prediction. Idempotent via `id` — re-POSTing the
    /// same id is a no-op (ON CONFLICT DO NOTHING). Returns the
    /// canonical stored row.
    async fn create_prediction(
        &self,
        input: &CreatePredictionInput,
    ) -> Result<MlPrediction, MlError>;

    /// List predictions for a specific `(entity_type, entity_id)`
    /// pair. Ordered by `created_at DESC`, capped by `limit`.
    async fn predictions_for_entity(
        &self,
        entity_type: &str,
        entity_id: &str,
        limit: i64,
    ) -> Result<Vec<MlPrediction>, MlError>;

    /// List recent predictions for a model. Ordered by
    /// `created_at DESC`, capped by `limit`.
    async fn recent_predictions_for_model(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<MlPrediction>, MlError>;
}
