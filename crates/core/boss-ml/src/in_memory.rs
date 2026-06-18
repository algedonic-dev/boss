//! In-memory adapter for tests + feature-off dev runs.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{Duration, Utc};

use crate::port::{MlError, MlRepository};
use crate::types::{CreatePredictionInput, MlModel, MlModelSummary, MlPrediction, ModelStatus};

#[derive(Default)]
struct Inner {
    models: HashMap<String, MlModel>,
    predictions: Vec<MlPrediction>,
}

pub struct InMemoryMlRepo {
    inner: RwLock<Inner>,
}

impl Default for InMemoryMlRepo {
    fn default() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }
}

impl InMemoryMlRepo {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MlRepository for InMemoryMlRepo {
    async fn all_model_summaries(
        &self,
        status: Option<ModelStatus>,
    ) -> Result<Vec<MlModelSummary>, MlError> {
        let inner = self.inner.read().unwrap();
        let cutoff = Utc::now() - Duration::hours(24);
        let mut out: Vec<MlModelSummary> = inner
            .models
            .values()
            .filter(|m| status.map(|s| m.status == s).unwrap_or(true))
            .cloned()
            .map(|model| {
                let mut count = 0i64;
                let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;
                for p in &inner.predictions {
                    if p.model_id == model.id {
                        if p.created_at >= cutoff {
                            count += 1;
                        }
                        match latest {
                            Some(t) if t >= p.created_at => {}
                            _ => latest = Some(p.created_at),
                        }
                    }
                }
                MlModelSummary {
                    model,
                    predictions_24h: count,
                    latest_prediction_at: latest,
                }
            })
            .collect();
        out.sort_by(|a, b| a.model.name.cmp(&b.model.name));
        Ok(out)
    }

    async fn model_summary_by_id(&self, id: &str) -> Result<Option<MlModelSummary>, MlError> {
        let inner = self.inner.read().unwrap();
        let Some(model) = inner.models.get(id).cloned() else {
            return Ok(None);
        };
        let cutoff = Utc::now() - Duration::hours(24);
        let mut count = 0i64;
        let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;
        for p in &inner.predictions {
            if p.model_id == id {
                if p.created_at >= cutoff {
                    count += 1;
                }
                match latest {
                    Some(t) if t >= p.created_at => {}
                    _ => latest = Some(p.created_at),
                }
            }
        }
        Ok(Some(MlModelSummary {
            model,
            predictions_24h: count,
            latest_prediction_at: latest,
        }))
    }

    async fn upsert_model(&self, model: &MlModel) -> Result<(), MlError> {
        let mut inner = self.inner.write().unwrap();
        inner.models.insert(model.id.clone(), model.clone());
        Ok(())
    }

    async fn create_prediction(
        &self,
        input: &CreatePredictionInput,
    ) -> Result<MlPrediction, MlError> {
        let mut inner = self.inner.write().unwrap();
        if !inner.models.contains_key(&input.model_id) {
            return Err(MlError::NotFound(format!("model {}", input.model_id)));
        }
        // Idempotent: if id already exists, return the existing row.
        if let Some(existing) = inner.predictions.iter().find(|p| p.id == input.id) {
            return Ok(existing.clone());
        }
        let row = MlPrediction {
            id: input.id.clone(),
            model_id: input.model_id.clone(),
            entity_type: input.entity_type.clone(),
            entity_id: input.entity_id.clone(),
            score: input.score,
            payload: input.payload.clone(),
            created_at: Utc::now(),
        };
        inner.predictions.push(row.clone());
        Ok(row)
    }

    async fn predictions_for_entity(
        &self,
        entity_type: &str,
        entity_id: &str,
        limit: i64,
    ) -> Result<Vec<MlPrediction>, MlError> {
        let inner = self.inner.read().unwrap();
        let mut rows: Vec<_> = inner
            .predictions
            .iter()
            .filter(|p| p.entity_type == entity_type && p.entity_id == entity_id)
            .cloned()
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn recent_predictions_for_model(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<MlPrediction>, MlError> {
        let inner = self.inner.read().unwrap();
        let mut rows: Vec<_> = inner
            .predictions
            .iter()
            .filter(|p| p.model_id == model_id)
            .cloned()
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        rows.truncate(limit as usize);
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelKind, ModelStatus};

    fn sample_model(id: &str, name: &str) -> MlModel {
        MlModel {
            id: id.to_string(),
            name: name.to_string(),
            kind: ModelKind::RiskScore,
            version: "v1".to_string(),
            status: ModelStatus::Draft,
            accuracy: None,
            accuracy_metric: None,
            training_data_ref: None,
            description: None,
            inference_spec: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn upsert_and_list() {
        let repo = InMemoryMlRepo::new();
        repo.upsert_model(&sample_model("m1", "churn"))
            .await
            .unwrap();
        repo.upsert_model(&sample_model("m2", "mtbf"))
            .await
            .unwrap();
        let out = repo.all_model_summaries(None).await.unwrap();
        assert_eq!(out.len(), 2);
        // Name-ordered.
        assert_eq!(out[0].model.name, "churn");
        assert_eq!(out[1].model.name, "mtbf");
    }

    #[tokio::test]
    async fn status_filter() {
        let repo = InMemoryMlRepo::new();
        let mut a = sample_model("m1", "churn");
        a.status = ModelStatus::Active;
        let b = sample_model("m2", "mtbf"); // draft
        repo.upsert_model(&a).await.unwrap();
        repo.upsert_model(&b).await.unwrap();
        let active = repo
            .all_model_summaries(Some(ModelStatus::Active))
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].model.name, "churn");
    }

    #[tokio::test]
    async fn upsert_replaces_existing() {
        let repo = InMemoryMlRepo::new();
        repo.upsert_model(&sample_model("m1", "churn"))
            .await
            .unwrap();
        let mut updated = sample_model("m1", "churn");
        updated.accuracy = Some(0.82);
        repo.upsert_model(&updated).await.unwrap();
        let summary = repo.model_summary_by_id("m1").await.unwrap().unwrap();
        assert_eq!(summary.model.accuracy, Some(0.82));
    }

    #[tokio::test]
    async fn prediction_requires_existing_model() {
        let repo = InMemoryMlRepo::new();
        let result = repo
            .create_prediction(&CreatePredictionInput {
                id: "p1".into(),
                model_id: "missing".into(),
                entity_type: "account".into(),
                entity_id: "account-1".into(),
                score: 0.5,
                payload: None,
            })
            .await;
        assert!(matches!(result, Err(MlError::NotFound(_))));
    }

    #[tokio::test]
    async fn prediction_is_idempotent_by_id() {
        let repo = InMemoryMlRepo::new();
        repo.upsert_model(&sample_model("m1", "churn"))
            .await
            .unwrap();
        let input = CreatePredictionInput {
            id: "p1".into(),
            model_id: "m1".into(),
            entity_type: "account".into(),
            entity_id: "account-1".into(),
            score: 0.5,
            payload: None,
        };
        let first = repo.create_prediction(&input).await.unwrap();
        // Re-POST same id — should return the existing row, not a dup.
        let second = repo.create_prediction(&input).await.unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.created_at, second.created_at);
        let rows = repo.recent_predictions_for_model("m1", 10).await.unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn predictions_for_entity_filters() {
        let repo = InMemoryMlRepo::new();
        repo.upsert_model(&sample_model("m1", "churn"))
            .await
            .unwrap();
        for (id, ctype, cid) in [
            ("p1", "account", "account-1"),
            ("p2", "account", "account-1"),
            ("p3", "account", "account-2"),
            ("p4", "device", "dev-1"),
        ] {
            repo.create_prediction(&CreatePredictionInput {
                id: id.into(),
                model_id: "m1".into(),
                entity_type: ctype.into(),
                entity_id: cid.into(),
                score: 0.5,
                payload: None,
            })
            .await
            .unwrap();
        }
        let rows = repo
            .predictions_for_entity("account", "account-1", 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        let rows = repo
            .predictions_for_entity("device", "dev-1", 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn predictions_24h_counts_recent_only() {
        let repo = InMemoryMlRepo::new();
        repo.upsert_model(&sample_model("m1", "churn"))
            .await
            .unwrap();
        repo.create_prediction(&CreatePredictionInput {
            id: "p1".into(),
            model_id: "m1".into(),
            entity_type: "account".into(),
            entity_id: "account-1".into(),
            score: 0.5,
            payload: None,
        })
        .await
        .unwrap();
        let summary = repo.model_summary_by_id("m1").await.unwrap().unwrap();
        assert_eq!(summary.predictions_24h, 1);
        assert!(summary.latest_prediction_at.is_some());
    }
}
