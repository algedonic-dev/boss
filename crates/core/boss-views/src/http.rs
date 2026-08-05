//! HTTP surface for Views.
//!
//! `GET/POST /api/views`, `GET/PUT/DELETE /api/views/{id}`, and
//! `GET /api/views/{id}/results` — the definition CRUD plus the one
//! endpoint that runs it.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get};
use serde::Deserialize;

use crate::error::ViewsError;
use crate::port::{ViewResolver, ViewsRepo};
use crate::types::ViewInput;

/// Rows returned by `/results` when the caller does not say.
const DEFAULT_LIMIT: usize = 100;
/// Ceiling on the caller-supplied limit. The scan itself is bounded
/// separately (`query::SCAN_CEILING`); this bounds the response.
const MAX_LIMIT: usize = 500;

#[derive(Clone)]
pub struct ViewsApiState {
    pub repo: Arc<dyn ViewsRepo>,
    pub resolver: Arc<dyn ViewResolver>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    /// Whose Views to list. Everything they own plus everything
    /// shared.
    pub viewer_id: String,
}

#[derive(Deserialize)]
pub struct ResultsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

pub fn router(state: ViewsApiState) -> Router {
    Router::new()
        .route("/api/views/health", get(health))
        .route("/api/views", get(list_views).post(create_view))
        .route(
            "/api/views/{id}",
            get(get_view).put(replace_view).delete(delete_view),
        )
        .route("/api/views/{id}/results", get(view_results))
        .with_state(Arc::new(state))
}

async fn health() -> Response {
    Json(serde_json::json!({ "status": "ok", "service": "views" })).into_response()
}

fn err_to_response(e: ViewsError) -> Response {
    match e {
        ViewsError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        // A filter that does not parse is the caller's text, not a
        // server fault — 422 so the authoring surface can show it
        // against the field.
        ViewsError::InvalidFilter(s) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid filter: {s}"),
        )
            .into_response(),
        ViewsError::Invalid(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        ViewsError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
    }
}

async fn list_views(
    State(state): State<Arc<ViewsApiState>>,
    Query(q): Query<ListQuery>,
) -> Response {
    match state.repo.list_for_viewer(&q.viewer_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn get_view(State(state): State<Arc<ViewsApiState>>, Path(id): Path<String>) -> Response {
    match state.repo.get(&id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn create_view(
    State(state): State<Arc<ViewsApiState>>,
    Json(input): Json<ViewInput>,
) -> Response {
    match state.repo.create(&input).await {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn replace_view(
    State(state): State<Arc<ViewsApiState>>,
    Path(id): Path<String>,
    Json(input): Json<ViewInput>,
) -> Response {
    match state.repo.replace(&id, &input).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn delete_view(State(state): State<Arc<ViewsApiState>>, Path(id): Path<String>) -> Response {
    match state.repo.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn view_results(
    State(state): State<Arc<ViewsApiState>>,
    Path(id): Path<String>,
    Query(q): Query<ResultsQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let view = match state.repo.get(&id).await {
        Ok(v) => v,
        Err(e) => return err_to_response(e),
    };
    match state.resolver.resolve(&view, limit).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => err_to_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemoryViewsRepo;
    use crate::types::{View, ViewLayout, ViewResults, ViewSource, Visibility};
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::DateTime;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Resolver stub — HTTP tests care about wiring and status codes,
    /// not about what the projections contain.
    struct StubResolver;

    #[async_trait]
    impl ViewResolver for StubResolver {
        async fn resolve(&self, view: &View, limit: usize) -> Result<ViewResults, ViewsError> {
            Ok(ViewResults {
                view_id: view.id.clone(),
                source: view.source,
                layout: view.layout,
                rows: vec![serde_json::json!({"limit_seen": limit})],
                matched: 1,
                truncated: false,
            })
        }
    }

    fn app() -> Router {
        router(ViewsApiState {
            repo: Arc::new(InMemoryViewsRepo::new(
                DateTime::from_timestamp(1_700_000_000, 0).expect("valid ts"),
            )),
            resolver: Arc::new(StubResolver),
        })
    }

    fn create_body(title: &str, filter: &str) -> String {
        serde_json::json!({
            "owner_id": "alice",
            "title": title,
            "source": "jobs",
            "filter": filter,
            "columns": [],
            "layout": "table",
            "visibility": "private"
        })
        .to_string()
    }

    async fn post_view(app: &Router, title: &str, filter: &str) -> (StatusCode, String) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/views")
                    .header("content-type", "application/json")
                    .body(Body::from(create_body(title, filter)))
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        let status = resp.status();
        let body = resp.into_body().collect().await.expect("body").to_bytes();
        (status, String::from_utf8_lossy(&body).to_string())
    }

    #[tokio::test]
    async fn create_then_list_and_run() {
        let app = app();
        let (status, body) = post_view(&app, "Open jobs", "status = \"open\"").await;
        assert_eq!(status, StatusCode::CREATED);
        let made: View = serde_json::from_str(&body).expect("a view");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/views?viewer_id=alice")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/views/{}/results", made.id))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_malformed_filter_is_422_not_500() {
        // The authoring surface shows this against the filter field,
        // so it has to be distinguishable from a server fault.
        let (status, body) = post_view(&app(), "Broken", "status =").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("invalid filter"), "body was: {body}");
    }

    #[tokio::test]
    async fn results_clamps_an_absurd_limit_instead_of_honouring_it() {
        let app = app();
        let (_, body) = post_view(&app, "Everything", "").await;
        let made: View = serde_json::from_str(&body).expect("a view");

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/views/{}/results?limit=99999", made.id))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        let body = resp.into_body().collect().await.expect("body").to_bytes();
        let results: ViewResults = serde_json::from_slice(&body).expect("results");
        assert_eq!(results.rows[0]["limit_seen"], serde_json::json!(MAX_LIMIT));
    }

    #[tokio::test]
    async fn running_a_missing_view_is_404() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/api/views/view-nope/results")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_shared_view_reaches_a_viewer_who_does_not_own_it() {
        let app = app();
        let body = serde_json::json!({
            "owner_id": "alice", "title": "Ours", "source": "jobs",
            "filter": "", "columns": [], "layout": "table", "visibility": "shared"
        })
        .to_string();
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/views")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/views?viewer_id=bob")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        let body = resp.into_body().collect().await.expect("body").to_bytes();
        let list: Vec<View> = serde_json::from_slice(&body).expect("a list");
        assert_eq!(list.len(), 1, "bob should see alice's shared View");
        assert_eq!(list[0].visibility, Visibility::Shared);
        assert_eq!(list[0].source, ViewSource::Jobs);
        assert_eq!(list[0].layout, ViewLayout::Table);
    }
}
