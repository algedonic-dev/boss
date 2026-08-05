//! HTTP surface — `GET /api/search`.
//!
//! One endpoint over core identity, per Q1: `subjects`, `jobs` and
//! `audit_log` live in core and can be joined in one round trip, which
//! a gateway-level fan-out to domain APIs could not do without
//! re-joining in the wrong place. Domain detail (a vendor's category, an
//! invoice's revenue category) stays with the app that owns it — the
//! global box answers "what and where", the app answers "which one".

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get};
use boss_policy_client::{CurrentUser, PolicyClient};
use serde::Deserialize;
use sqlx::PgPool;

use crate::error::SearchError;

#[derive(Clone)]
pub struct SearchApiState {
    pub pool: PgPool,
    /// Results are policy-scoped to the caller. Search reads `jobs`
    /// and `audit_log`, both scoped wherever else they are read;
    /// without this the box is a way to see records a role cannot
    /// open — which is what it was.
    pub policy: Arc<dyn PolicyClient>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    /// Subject kinds belonging to the calling app, comma-separated.
    /// Floats those to the top without filtering anything out — a
    /// global box that hides results because you are in the wrong app
    /// is not a global box.
    #[serde(default)]
    pub app_kinds: Option<String>,
}

pub fn router(state: SearchApiState) -> Router {
    Router::new()
        .route("/api/search/health", get(health))
        .route("/api/search", get(search_handler))
        .with_state(Arc::new(state))
}

async fn health() -> Response {
    Json(serde_json::json!({ "status": "ok", "service": "search" })).into_response()
}

async fn search_handler(
    State(state): State<Arc<SearchApiState>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<SearchQuery>,
) -> Response {
    let app_kinds: Vec<String> = q
        .app_kinds
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let scope = match crate::query::SearchScope::for_user(state.policy.as_ref(), &user).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "search policy scope failed");
            // Fail closed. A search box that answers unscoped when
            // the policy engine is unreachable is worse than one that
            // says it cannot answer.
            return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response();
        }
    };

    match crate::query::search(&state.pool, &q.q, &app_kinds, &scope).await {
        Ok(results) => Json(results).into_response(),
        Err(SearchError::BadRequest(m)) => (StatusCode::BAD_REQUEST, m).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "search failed");
            (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response()
        }
    }
}
