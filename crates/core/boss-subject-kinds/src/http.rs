//! HTTP API for the SubjectKind registry. Read-only in v1;
//! authoring lands when the admin UI does.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::port::SubjectKindRepository;

#[derive(Clone)]
pub struct SubjectKindsApiState {
    pub subject_kinds: Arc<dyn SubjectKindRepository>,
}

pub fn router(state: SubjectKindsApiState) -> Router {
    Router::new()
        .route("/api/subject-kinds/health", get(health))
        .route("/api/subject-kinds", get(list_active))
        .route("/api/subject-kinds/{kind}", get(get_kind))
        .route("/api/subject-kinds/{kind}/exists", get(kind_exists))
        .route("/api/subject-kinds/{kind}/children", get(children_of))
        .with_state(state)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

/// Standard health probe — see boss-classes/src/http.rs for context.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "boss-subject-kinds-api",
        "storage": STORAGE,
    }))
}

async fn list_active(State(state): State<SubjectKindsApiState>) -> Response {
    match state.subject_kinds.list_active().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_kind(State(state): State<SubjectKindsApiState>, Path(kind): Path<String>) -> Response {
    match state.subject_kinds.get(&kind).await {
        Ok(Some(k)) => Json(k).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such subject kind").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn kind_exists(
    State(state): State<SubjectKindsApiState>,
    Path(kind): Path<String>,
) -> Response {
    match state.subject_kinds.exists_active(&kind).await {
        Ok(b) => Json(serde_json::json!({ "exists": b })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn children_of(
    State(state): State<SubjectKindsApiState>,
    Path(kind): Path<String>,
) -> Response {
    match state.subject_kinds.children_of(&kind).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemorySubjectKinds;
    use crate::port::SubjectKind;
    use axum::body::to_bytes;
    use axum::http::Request;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    fn sk(kind: &str, parent: Option<&str>) -> SubjectKind {
        SubjectKind {
            kind: kind.into(),
            label: kind.to_string(),
            parent_kind: parent.map(String::from),
            description: None,
            owning_team: "platform".into(),
            metadata: json!({}),
            sort_order: 0,
            retired_at: None,
        }
    }

    fn app(rows: Vec<SubjectKind>) -> Router {
        let state = SubjectKindsApiState {
            subject_kinds: Arc::new(InMemorySubjectKinds::new(rows)),
        };
        router(state)
    }

    #[tokio::test]
    async fn list_returns_active_rows() {
        let app = app(vec![sk("asset", None), sk("vendor", None)]);
        let req = Request::builder()
            .uri("/api/subject-kinds")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn get_returns_404_for_unknown_kind() {
        let app = app(vec![sk("asset", None)]);
        let req = Request::builder()
            .uri("/api/subject-kinds/nope")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn exists_endpoint_envelope_shape() {
        let app = app(vec![sk("asset", None)]);
        let req = Request::builder()
            .uri("/api/subject-kinds/asset/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["exists"], json!(true));
    }

    #[tokio::test]
    async fn children_endpoint_walks_parent_kind() {
        let app = app(vec![
            sk("account", None),
            sk("medical-practice", Some("account")),
            sk("wholesale-customer", Some("account")),
        ]);
        let req = Request::builder()
            .uri("/api/subject-kinds/account/children")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let kinds: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, vec!["medical-practice", "wholesale-customer"]);
    }
}
