//! HTTP surface for `boss-campaigns`.
//!
//! - `GET  /api/campaigns/health` — liveness
//! - `GET  /api/campaigns`        — list, newest first
//! - `GET  /api/campaigns/{id}`   — one campaign
//! - `POST /api/campaigns`        — create (idempotent on id; the
//!   birth path: domain row + identity row + outbox event in one tx)
//!
//! The sim's campaign births route here
//! (`campaigns.campaign.created` → POST), as does the daemon's
//! boot-time pool sync. Payload tolerance mirrors the kind-scoped
//! subjects mint: `id` is contractual; `name` falls back to the id.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::port::{CampaignsError, CampaignsRepository};
use crate::types::Campaign;

#[derive(Clone)]
pub struct CampaignsApiState<R: CampaignsRepository> {
    pub campaigns: Arc<R>,
    /// Authoritative clock — every stamp routes through clock-api.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router<R: CampaignsRepository + 'static>(state: CampaignsApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/campaigns/health", get(health))
        .route(
            "/api/campaigns",
            get(list_campaigns::<R>).post(create_campaign::<R>),
        )
        .route("/api/campaigns/{id}", get(get_campaign::<R>))
        .with_state(shared)
}

async fn health() -> Response {
    Json(serde_json::json!({"status": "ok"})).into_response()
}

/// Tolerant birth body: sim-synthesized payloads carry `id` + `name`
/// (+ `born_on`, which becomes `starts_on` unless given explicitly).
#[derive(Deserialize)]
struct CreateBody {
    id: String,
    #[serde(default)]
    #[serde(alias = "label", alias = "title")]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    #[serde(alias = "born_on")]
    starts_on: Option<chrono::NaiveDate>,
    #[serde(default)]
    ends_on: Option<chrono::NaiveDate>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

async fn create_campaign<R: CampaignsRepository + 'static>(
    State(state): State<Arc<CampaignsApiState<R>>>,
    Json(body): Json<CreateBody>,
) -> Response {
    if body.id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "id is required").into_response();
    }
    let now = state.clock.now().await.now;
    let campaign = Campaign {
        name: body.name.unwrap_or_else(|| body.id.clone()),
        id: body.id,
        status: body.status.unwrap_or_else(|| "active".to_string()),
        starts_on: body.starts_on.or_else(|| Some(now.date_naive())),
        ends_on: body.ends_on,
        metadata: body.metadata.unwrap_or_else(|| serde_json::json!({})),
        created_at: None,
    };
    match state.campaigns.create_campaign_at(&campaign, now).await {
        // Idempotent re-POST reports 200 (nothing written, nothing
        // emitted); a fresh birth is 201.
        Ok(true) => (StatusCode::CREATED, Json(campaign)).into_response(),
        Ok(false) => (StatusCode::OK, Json(campaign)).into_response(),
        Err(e) => error_response(e),
    }
}

async fn get_campaign<R: CampaignsRepository + 'static>(
    State(state): State<Arc<CampaignsApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.campaigns.get_campaign(&id).await {
        Ok(Some(c)) => Json(c).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => error_response(e),
    }
}

async fn list_campaigns<R: CampaignsRepository + 'static>(
    State(state): State<Arc<CampaignsApiState<R>>>,
) -> Response {
    match state.campaigns.list_campaigns().await {
        Ok(all) => Json(all).into_response(),
        Err(e) => error_response(e),
    }
}

fn error_response(e: CampaignsError) -> Response {
    match e {
        CampaignsError::NotFound(m) => (StatusCode::NOT_FOUND, m).into_response(),
        CampaignsError::Invalid(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        CampaignsError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}
