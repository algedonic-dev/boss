//! HTTP handlers + router for `boss-ml-api`.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use crate::port::{MlError, MlRepository};
use crate::types::{CreatePredictionInput, ModelStatus};

#[cfg(feature = "postgres")]
use crate::inference::{InferError, InferenceDispatcher};

pub struct MlApiState {
    pub repo: Arc<dyn MlRepository>,
    /// Optional inference dispatcher. When unset, the
    /// `/infer/...` and `/infer-batch` endpoints return
    /// 503 — the boss-ml-api binary wires it on startup,
    /// other test contexts may run the read-only routes
    /// without a Postgres-backed dispatcher.
    #[cfg(feature = "postgres")]
    pub dispatcher: Option<Arc<InferenceDispatcher>>,
}

pub fn router(state: MlApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/ml/health", get(health))
        .route("/api/ml/models", get(list_models))
        .route("/api/ml/models/{id}", get(get_model))
        .route(
            "/api/ml/predictions",
            post(create_prediction).get(list_predictions),
        )
        .route(
            "/api/ml/models/{id}/infer/{entity_id}",
            post(infer_one_handler),
        )
        .route("/api/ml/models/{id}/infer-batch", post(infer_batch_handler))
        .with_state(shared)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-ml-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

fn err_to_response(e: MlError) -> Response {
    match e {
        MlError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        MlError::BadRequest(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        MlError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
    }
}

#[derive(Deserialize)]
struct ListModelsQuery {
    status: Option<String>,
}

async fn list_models(
    State(state): State<Arc<MlApiState>>,
    Query(q): Query<ListModelsQuery>,
) -> Response {
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => match ModelStatus::parse(s) {
            Some(status) => Some(status),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!(
                        "invalid status `{s}` — expected one of draft, active, shadow, retired"
                    ),
                )
                    .into_response();
            }
        },
    };
    match state.repo.all_model_summaries(status).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn get_model(State(state): State<Arc<MlApiState>>, Path(id): Path<String>) -> Response {
    match state.repo.model_summary_by_id(&id).await {
        Ok(Some(row)) => Json(row).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("no model {id}")).into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn create_prediction(
    State(state): State<Arc<MlApiState>>,
    Json(input): Json<CreatePredictionInput>,
) -> Response {
    match state.repo.create_prediction(&input).await {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        Err(e) => err_to_response(e),
    }
}

#[derive(Deserialize)]
struct ListPredictionsQuery {
    entity_type: Option<String>,
    entity_id: Option<String>,
    model_id: Option<String>,
    limit: Option<i64>,
}

async fn list_predictions(
    State(state): State<Arc<MlApiState>>,
    Query(q): Query<ListPredictionsQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    if let (Some(entity_type), Some(entity_id)) = (q.entity_type.as_deref(), q.entity_id.as_deref())
    {
        match state
            .repo
            .predictions_for_entity(entity_type, entity_id, limit)
            .await
        {
            Ok(rows) => Json(rows).into_response(),
            Err(e) => err_to_response(e),
        }
    } else if let Some(model_id) = q.model_id.as_deref() {
        match state
            .repo
            .recent_predictions_for_model(model_id, limit)
            .await
        {
            Ok(rows) => Json(rows).into_response(),
            Err(e) => err_to_response(e),
        }
    } else {
        (
            StatusCode::BAD_REQUEST,
            "provide either (entity_type + entity_id) or model_id",
        )
            .into_response()
    }
}

#[cfg(feature = "postgres")]
fn infer_err_to_response(e: InferError) -> Response {
    match e {
        InferError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        InferError::BadRequest(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        InferError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
        InferError::Plugin(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
        InferError::Unimplemented(s) => (StatusCode::NOT_IMPLEMENTED, s).into_response(),
    }
}

#[cfg(feature = "postgres")]
async fn infer_one_handler(
    State(state): State<Arc<MlApiState>>,
    Path((id, entity_id)): Path<(String, String)>,
) -> Response {
    let Some(dispatcher) = state.dispatcher.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "inference dispatcher not wired",
        )
            .into_response();
    };
    match dispatcher.infer_one(&id, &entity_id).await {
        Ok(p) => (StatusCode::CREATED, Json(p)).into_response(),
        Err(e) => infer_err_to_response(e),
    }
}

#[cfg(feature = "postgres")]
async fn infer_batch_handler(
    State(state): State<Arc<MlApiState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(dispatcher) = state.dispatcher.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "inference dispatcher not wired",
        )
            .into_response();
    };
    match dispatcher.infer_batch(&id).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => infer_err_to_response(e),
    }
}

// When the postgres feature is off the routes still need handlers
// that the router can resolve; they always return 503.
#[cfg(not(feature = "postgres"))]
async fn infer_one_handler(
    State(_state): State<Arc<MlApiState>>,
    Path((_id, _entity_id)): Path<(String, String)>,
) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "inference dispatcher requires the postgres feature",
    )
        .into_response()
}

#[cfg(not(feature = "postgres"))]
async fn infer_batch_handler(
    State(_state): State<Arc<MlApiState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "inference dispatcher requires the postgres feature",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryMlRepo;
    use crate::bootstrap::seed_phase_two_candidates;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn seeded_app() -> axum::Router {
        let repo = Arc::new(InMemoryMlRepo::new());
        seed_phase_two_candidates(repo.as_ref()).await.unwrap();
        let state = MlApiState {
            repo: repo as Arc<dyn MlRepository>,
            #[cfg(feature = "postgres")]
            dispatcher: None,
        };
        router(state)
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string())
        })
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let app = seeded_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/ml/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_models_returns_seeded_set() {
        let app = seeded_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/ml/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let arr = body.as_array().expect("array");
        assert_eq!(arr.len(), 9);
        let names: Vec<&str> = arr.iter().filter_map(|m| m["name"].as_str()).collect();
        assert!(names.contains(&"account-churn-risk"));
        assert!(names.contains(&"device-mtbf"));
        assert!(names.contains(&"opportunity-win-probability"));
        assert!(names.contains(&"next-action-contract-expiring"));
        assert!(names.contains(&"next-action-past-due-invoice"));
        assert!(names.contains(&"next-action-missing-primary-contact"));
        assert!(names.contains(&"next-action-high-churn-risk"));
        assert!(names.contains(&"next-action-stalled-service-ticket"));
        assert!(names.contains(&"next-action-preventive-maintenance-due"));
    }

    #[tokio::test]
    async fn list_models_status_filter() {
        let app = seeded_app().await;
        // Of the 9 Phase-2 seeds, 2 stay draft (device-mtbf,
        // opportunity-win-probability — storage-only kinds awaiting
        // future inference work); 7 flipped to active across steps
        // 7-9 when they became inference-driven (churn-risk
        // heuristic-formula + 6 next-action declarative-rules).
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/ml/models?status=draft")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_models_invalid_status_is_400() {
        let app = seeded_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/ml/models?status=bogus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_model_by_id() {
        let app = seeded_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/ml/models/mdl-account-churn-risk-v1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["name"].as_str(), Some("account-churn-risk"));
    }

    #[tokio::test]
    async fn create_prediction_happy_path() {
        let app = seeded_app().await;
        let body = serde_json::json!({
            "id": "p1",
            "model_id": "mdl-account-churn-risk-v1",
            "entity_type": "account",
            "entity_id": "account-00001",
            "score": 0.75,
            "payload": {"signal": "low-cadence"}
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ml/predictions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_prediction_missing_model_is_404() {
        let app = seeded_app().await;
        let body = serde_json::json!({
            "id": "p1",
            "model_id": "mdl-nope",
            "entity_type": "account",
            "entity_id": "account-1",
            "score": 0.5
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ml/predictions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_predictions_requires_filter() {
        let app = seeded_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/ml/predictions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn infer_one_returns_503_when_dispatcher_unset() {
        let app = seeded_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ml/models/mdl-account-churn-risk-v1/infer/account-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn infer_batch_returns_503_when_dispatcher_unset() {
        let app = seeded_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ml/models/mdl-account-churn-risk-v1/infer-batch")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[cfg(feature = "postgres")]
    #[tokio::test(flavor = "multi_thread")]
    async fn infer_batch_runs_declarative_rule_via_http() {
        use crate::InferenceDispatcher;
        use crate::types::{DeclarativeRuleSpec, InferenceSpec, MlModel, ModelKind, ModelStatus};
        use boss_testing::TestDb;
        use chrono::Utc;

        let db = TestDb::new().await;
        let pool = db.pool.clone();
        sqlx::query("CREATE TABLE fake_accounts (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO fake_accounts VALUES ('a-1'), ('a-2')")
            .execute(&pool)
            .await
            .unwrap();

        let repo: Arc<dyn MlRepository> = Arc::new(InMemoryMlRepo::new());
        let model = MlModel {
            id: "mdl-http".into(),
            name: "mdl-http".into(),
            kind: ModelKind::DeclarativeRule,
            version: "v1".into(),
            status: ModelStatus::Active,
            accuracy: None,
            accuracy_metric: None,
            training_data_ref: None,
            description: None,
            inference_spec: Some(InferenceSpec::DeclarativeRule(DeclarativeRuleSpec {
                query: "SELECT id AS account_id, 0.5::float8 AS score FROM fake_accounts".into(),
                score_expr: "score".into(),
                entity_type: "account".into(),
                entity_id_column: "account_id".into(),
                row_key_column: None,
                payload_template: serde_json::Value::Null,
            })),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        repo.upsert_model(&model).await.unwrap();
        let dispatcher = Arc::new(InferenceDispatcher::new(
            pool.clone(),
            repo.clone(),
            Arc::new(boss_clock_client::WallClockClient),
        ));
        let state = MlApiState {
            repo: repo.clone(),
            dispatcher: Some(dispatcher),
        };
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ml/models/mdl-http/infer-batch")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["written"].as_i64(), Some(2));
    }
}
