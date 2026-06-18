//! Cross-domain full-text search backed by Postgres `search_all()` function.
//!
//! Mounted alongside the people API since it already has a Postgres pool.
//! Searches employees, device_models, devices, and shipments.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct SearchState {
    pub pool: Arc<PgPool>,
}

pub fn search_router(pool: PgPool) -> Router {
    let state = SearchState {
        pool: Arc::new(pool),
    };
    Router::new()
        .route("/api/people/search", get(search))
        .with_state(state)
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    #[serde(default = "default_limit")]
    limit: i32,
}

fn default_limit() -> i32 {
    5
}

#[derive(Serialize, sqlx::FromRow)]
struct SearchResult {
    entity_type: String,
    entity_id: String,
    label: String,
    detail: String,
    path: String,
    rank: f32,
}

async fn search(State(state): State<SearchState>, Query(params): Query<SearchParams>) -> Response {
    let q = params.q.trim();
    if q.is_empty() || q.len() < 2 {
        return Json(Vec::<SearchResult>::new()).into_response();
    }

    let results: Result<Vec<SearchResult>, _> = sqlx::query_as(
        "SELECT entity_type, entity_id, label, detail, path, rank FROM search_all($1, $2)",
    )
    .bind(q)
    .bind(params.limit)
    .fetch_all(state.pool.as_ref())
    .await;

    match results {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
