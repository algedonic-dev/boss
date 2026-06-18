//! Postgres adapter for the ML platform.

#![cfg(feature = "postgres")]

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::{PgPool, Row, postgres::PgRow};

use crate::port::{MlError, MlRepository};
use crate::types::{
    CreatePredictionInput, MlModel, MlModelSummary, MlPrediction, ModelKind, ModelStatus,
};

pub struct PgMlRepo {
    pool: PgPool,
}

impl PgMlRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage<E: std::fmt::Display>(e: E) -> MlError {
    MlError::Storage(e.to_string())
}

fn model_from_row(row: &PgRow) -> Result<MlModel, MlError> {
    let kind_str: String = row.try_get("kind").map_err(storage)?;
    let kind = ModelKind::parse(&kind_str)
        .ok_or_else(|| MlError::Storage(format!("unknown kind {kind_str}")))?;
    let status_str: String = row.try_get("status").map_err(storage)?;
    let status = ModelStatus::parse(&status_str)
        .ok_or_else(|| MlError::Storage(format!("unknown status {status_str}")))?;
    let inference_spec = row
        .try_get::<Option<JsonValue>, _>("inference_spec")
        .map_err(storage)?
        .map(serde_json::from_value::<crate::types::InferenceSpec>)
        .transpose()
        .map_err(|e| MlError::Storage(format!("inference_spec parse: {e}")))?;
    Ok(MlModel {
        id: row.try_get("id").map_err(storage)?,
        name: row.try_get("name").map_err(storage)?,
        kind,
        version: row.try_get("version").map_err(storage)?,
        status,
        accuracy: row.try_get("accuracy").map_err(storage)?,
        accuracy_metric: row.try_get("accuracy_metric").map_err(storage)?,
        training_data_ref: row.try_get("training_data_ref").map_err(storage)?,
        description: row.try_get("description").map_err(storage)?,
        inference_spec,
        created_at: row.try_get("created_at").map_err(storage)?,
        updated_at: row.try_get("updated_at").map_err(storage)?,
    })
}

fn summary_from_row(row: &PgRow) -> Result<MlModelSummary, MlError> {
    let model = model_from_row(row)?;
    Ok(MlModelSummary {
        model,
        predictions_24h: row.try_get("predictions_24h").map_err(storage)?,
        latest_prediction_at: row
            .try_get::<Option<DateTime<Utc>>, _>("latest_prediction_at")
            .map_err(storage)?,
    })
}

fn prediction_from_row(row: &PgRow) -> Result<MlPrediction, MlError> {
    Ok(MlPrediction {
        id: row.try_get("id").map_err(storage)?,
        model_id: row.try_get("model_id").map_err(storage)?,
        entity_type: row.try_get("entity_type").map_err(storage)?,
        entity_id: row.try_get("entity_id").map_err(storage)?,
        score: row.try_get("score").map_err(storage)?,
        payload: row
            .try_get::<Option<JsonValue>, _>("payload")
            .map_err(storage)?,
        created_at: row.try_get("created_at").map_err(storage)?,
    })
}

const SUMMARY_SELECT: &str = "SELECT m.id, m.name, m.kind, m.version, m.status, \
     m.accuracy, m.accuracy_metric, m.training_data_ref, m.description, \
     m.inference_spec, m.created_at, m.updated_at, \
     COALESCE(( \
         SELECT COUNT(*) FROM ml_predictions p \
         WHERE p.model_id = m.id \
           AND p.created_at >= NOW() - INTERVAL '24 hours' \
     ), 0) AS predictions_24h, \
     ( \
         SELECT MAX(p.created_at) FROM ml_predictions p WHERE p.model_id = m.id \
     ) AS latest_prediction_at \
     FROM ml_models m";

#[async_trait]
impl MlRepository for PgMlRepo {
    async fn all_model_summaries(
        &self,
        status: Option<ModelStatus>,
    ) -> Result<Vec<MlModelSummary>, MlError> {
        let rows = match status {
            Some(s) => {
                let sql = format!("{SUMMARY_SELECT} WHERE m.status = $1 ORDER BY m.name");
                sqlx::query(&sql)
                    .bind(s.as_str())
                    .fetch_all(&self.pool)
                    .await
            }
            None => {
                let sql = format!("{SUMMARY_SELECT} ORDER BY m.name");
                sqlx::query(&sql).fetch_all(&self.pool).await
            }
        }
        .map_err(storage)?;
        rows.iter().map(summary_from_row).collect()
    }

    async fn model_summary_by_id(&self, id: &str) -> Result<Option<MlModelSummary>, MlError> {
        let sql = format!("{SUMMARY_SELECT} WHERE m.id = $1");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        row.as_ref().map(summary_from_row).transpose()
    }

    async fn upsert_model(&self, model: &MlModel) -> Result<(), MlError> {
        let spec_json = model
            .inference_spec
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| MlError::Storage(format!("inference_spec serialize: {e}")))?;
        sqlx::query(
            "INSERT INTO ml_models (
                id, name, kind, version, status, accuracy, accuracy_metric,
                training_data_ref, description, inference_spec,
                created_at, updated_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
             ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                kind = EXCLUDED.kind,
                version = EXCLUDED.version,
                status = EXCLUDED.status,
                accuracy = EXCLUDED.accuracy,
                accuracy_metric = EXCLUDED.accuracy_metric,
                training_data_ref = EXCLUDED.training_data_ref,
                description = EXCLUDED.description,
                inference_spec = EXCLUDED.inference_spec,
                updated_at = EXCLUDED.updated_at",
        )
        .bind(&model.id)
        .bind(&model.name)
        .bind(model.kind.as_str())
        .bind(&model.version)
        .bind(model.status.as_str())
        .bind(model.accuracy)
        .bind(&model.accuracy_metric)
        .bind(&model.training_data_ref)
        .bind(&model.description)
        .bind(spec_json)
        .bind(model.created_at)
        .bind(model.updated_at)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn create_prediction(
        &self,
        input: &CreatePredictionInput,
    ) -> Result<MlPrediction, MlError> {
        // Defensive FK check so we return NotFound instead of a
        // raw integrity constraint error.
        let model_exists: Option<String> =
            sqlx::query_scalar("SELECT id FROM ml_models WHERE id = $1")
                .bind(&input.model_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage)?;
        if model_exists.is_none() {
            return Err(MlError::NotFound(format!("model {}", input.model_id)));
        }

        sqlx::query(
            "INSERT INTO ml_predictions (
                id, model_id, entity_type, entity_id, score, payload
             ) VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&input.id)
        .bind(&input.model_id)
        .bind(&input.entity_type)
        .bind(&input.entity_id)
        .bind(input.score)
        .bind(&input.payload)
        .execute(&self.pool)
        .await
        .map_err(storage)?;

        let row = sqlx::query("SELECT * FROM ml_predictions WHERE id = $1")
            .bind(&input.id)
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
        prediction_from_row(&row)
    }

    async fn predictions_for_entity(
        &self,
        entity_type: &str,
        entity_id: &str,
        limit: i64,
    ) -> Result<Vec<MlPrediction>, MlError> {
        let rows = sqlx::query(
            "SELECT * FROM ml_predictions
             WHERE entity_type = $1 AND entity_id = $2
             ORDER BY created_at DESC LIMIT $3",
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(prediction_from_row).collect()
    }

    async fn recent_predictions_for_model(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<MlPrediction>, MlError> {
        let rows = sqlx::query(
            "SELECT * FROM ml_predictions
             WHERE model_id = $1
             ORDER BY created_at DESC LIMIT $2",
        )
        .bind(model_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(prediction_from_row).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_testing::TestDb;
    use chrono::Utc;

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

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_and_list_models() {
        let db = TestDb::new().await;
        let repo = PgMlRepo::new(db.pool.clone());
        repo.upsert_model(&sample_model("m1", "churn"))
            .await
            .unwrap();
        repo.upsert_model(&sample_model("m2", "mtbf"))
            .await
            .unwrap();
        let out = repo.all_model_summaries(None).await.unwrap();
        assert_eq!(out.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn status_filter() {
        let db = TestDb::new().await;
        let repo = PgMlRepo::new(db.pool.clone());
        let mut active = sample_model("m1", "churn");
        active.status = ModelStatus::Active;
        repo.upsert_model(&active).await.unwrap();
        repo.upsert_model(&sample_model("m2", "mtbf"))
            .await
            .unwrap();
        let out = repo
            .all_model_summaries(Some(ModelStatus::Active))
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].model.name, "churn");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prediction_idempotent_by_id() {
        let db = TestDb::new().await;
        let repo = PgMlRepo::new(db.pool.clone());
        repo.upsert_model(&sample_model("m1", "churn"))
            .await
            .unwrap();
        let input = CreatePredictionInput {
            id: "p1".into(),
            model_id: "m1".into(),
            entity_type: "account".into(),
            entity_id: "account-1".into(),
            score: 0.42,
            payload: Some(serde_json::json!({"signal": "high-churn"})),
        };
        let first = repo.create_prediction(&input).await.unwrap();
        let second = repo.create_prediction(&input).await.unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.created_at, second.created_at);
        let rows = repo.recent_predictions_for_model("m1", 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].score, 0.42);
        assert!(rows[0].payload.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prediction_missing_model_is_not_found() {
        let db = TestDb::new().await;
        let repo = PgMlRepo::new(db.pool.clone());
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

    #[tokio::test(flavor = "multi_thread")]
    async fn predictions_24h_count_is_derived() {
        let db = TestDb::new().await;
        let repo = PgMlRepo::new(db.pool.clone());
        repo.upsert_model(&sample_model("m1", "churn"))
            .await
            .unwrap();
        for i in 0..3 {
            repo.create_prediction(&CreatePredictionInput {
                id: format!("p{i}"),
                model_id: "m1".into(),
                entity_type: "account".into(),
                entity_id: format!("account-{i}"),
                score: 0.5,
                payload: None,
            })
            .await
            .unwrap();
        }
        let summary = repo.model_summary_by_id("m1").await.unwrap().unwrap();
        assert_eq!(summary.predictions_24h, 3);
        assert!(summary.latest_prediction_at.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn predictions_for_entity_filters() {
        let db = TestDb::new().await;
        let repo = PgMlRepo::new(db.pool.clone());
        repo.upsert_model(&sample_model("m1", "churn"))
            .await
            .unwrap();
        for (id, ent_type, ent_id) in [
            ("p1", "account", "account-1"),
            ("p2", "account", "account-1"),
            ("p3", "device", "dev-1"),
        ] {
            repo.create_prediction(&CreatePredictionInput {
                id: id.into(),
                model_id: "m1".into(),
                entity_type: ent_type.into(),
                entity_id: ent_id.into(),
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
    }
}
