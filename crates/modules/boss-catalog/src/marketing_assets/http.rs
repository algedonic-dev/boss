//! HTTP surface for the Marketing Asset KB. Mounted under
//! `/api/catalog/marketing-assets/...`.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use serde::Deserialize;
use sqlx::PgPool;

use super::postgres::PgMarketingAssets;
use super::types::{NewMarketingAsset, UpdateMarketingAsset};
use crate::port::KbError;

#[derive(Clone)]
pub struct MarketingAssetsApiState {
    pub pool: PgPool,
    /// Class-registry handle. When `Some`, incoming asset `kind`
    /// codes are validated against the active Class set under
    /// `subject_kind='marketing-asset'`; `None` is permissive. The
    /// `boss-catalog-api` binary always wires `Some` from the
    /// required `classes_api_url` config (fail-loud, matching
    /// boss-people) — the field stays `Option` only so tests can
    /// pass `None`.
    pub classes_client: Option<Arc<dyn ClassesClient>>,
}

/// Validate a marketing-asset `kind` against the Class registry under
/// `subject_kind='marketing-asset'` (the code is the kind string). Same
/// contract as `check_document_kinds` in the device-catalog router:
/// permissive when no registry is wired, fail-closed (503) when it's
/// unreachable, 400 on an unregistered code. Identity-first: a `None`
/// kind is an unclassified asset, so the gate is skipped entirely —
/// it only fires once a value is supplied.
async fn check_marketing_kind(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    kind: Option<&str>,
) -> Result<(), Response> {
    let Some(kind) = kind else {
        return Ok(());
    };
    let Some(client) = classes_client else {
        return Ok(());
    };
    let class_ref = ClassRef::new("marketing-asset", kind);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown marketing-asset kind `{kind}` — register it as a Class \
                 first (subject_kind='marketing-asset')"
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("classes registry unreachable: {e}"),
        )
            .into_response()),
    }
}

pub fn router(state: MarketingAssetsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route(
            "/api/catalog/marketing-assets",
            get(list_assets).post(upsert_asset),
        )
        .route(
            "/api/catalog/marketing-assets/{id}",
            get(get_asset).put(update_asset),
        )
        .route("/api/catalog/marketing-assets/{id}/history", get(history))
        .route("/api/catalog/marketing-assets/{id}/retire", post(retire))
        .route(
            "/api/catalog/marketing-assets/{id}/supersede",
            post(supersede),
        )
        .with_state(shared)
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    include_retired: bool,
    #[serde(default)]
    limit: Option<i64>,
}

async fn list_assets(
    State(state): State<Arc<MarketingAssetsApiState>>,
    Query(q): Query<ListQuery>,
) -> Response {
    let adapter = PgMarketingAssets::new(state.pool.clone());
    match adapter
        .list(q.kind.as_deref(), q.include_retired, q.limit.unwrap_or(200))
        .await
    {
        Ok(list) => Json(list).into_response(),
        Err(e) => err(e),
    }
}

async fn upsert_asset(
    State(state): State<Arc<MarketingAssetsApiState>>,
    Json(body): Json<NewMarketingAsset>,
) -> Response {
    if let Err(resp) =
        check_marketing_kind(state.classes_client.as_ref(), body.kind.as_deref()).await
    {
        return resp;
    }
    let adapter = PgMarketingAssets::new(state.pool.clone());
    match adapter.create(body).await {
        Ok(a) => Json(a).into_response(),
        Err(e) => err(e),
    }
}

async fn get_asset(
    State(state): State<Arc<MarketingAssetsApiState>>,
    Path(id): Path<String>,
) -> Response {
    let adapter = PgMarketingAssets::new(state.pool.clone());
    match adapter.get(&id).await {
        Ok(Some(a)) => Json(a).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => err(e),
    }
}

async fn update_asset(
    State(state): State<Arc<MarketingAssetsApiState>>,
    Path(id): Path<String>,
    Json(patch): Json<UpdateMarketingAsset>,
) -> Response {
    let adapter = PgMarketingAssets::new(state.pool.clone());
    match adapter.update(&id, patch).await {
        Ok(Some(a)) => Json(a).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => err(e),
    }
}

async fn history(
    State(state): State<Arc<MarketingAssetsApiState>>,
    Path(id): Path<String>,
) -> Response {
    let adapter = PgMarketingAssets::new(state.pool.clone());
    match adapter.history(&id).await {
        Ok(chain) => Json(chain).into_response(),
        Err(e) => err(e),
    }
}

async fn retire(
    State(state): State<Arc<MarketingAssetsApiState>>,
    Path(id): Path<String>,
) -> Response {
    let adapter = PgMarketingAssets::new(state.pool.clone());
    match adapter.retire(&id).await {
        Ok(Some(a)) => Json(a).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found or already retired").into_response(),
        Err(e) => err(e),
    }
}

/// Create a new asset version that supersedes `{id}`. The body is a
/// `NewMarketingAsset` describing the replacement; this endpoint
/// stamps `supersedes_id` to the URL id before creating, so callers
/// don't have to thread the pointer manually.
async fn supersede(
    State(state): State<Arc<MarketingAssetsApiState>>,
    Path(id): Path<String>,
    Json(mut body): Json<NewMarketingAsset>,
) -> Response {
    if let Err(resp) =
        check_marketing_kind(state.classes_client.as_ref(), body.kind.as_deref()).await
    {
        return resp;
    }
    body.supersedes_id = Some(id);
    let adapter = PgMarketingAssets::new(state.pool.clone());
    match adapter.create(body).await {
        Ok(a) => Json(a).into_response(),
        Err(e) => err(e),
    }
}

fn err(e: KbError) -> Response {
    match e {
        KbError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
        KbError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        KbError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        KbError::BadRequest(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg).into_response(),
    }
}
