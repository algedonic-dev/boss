//! Axum routes for the cadence registry.
//!
//! This surface exists so the train conductor can run OUTSIDE the
//! cluster without a database connection. It reaches the same
//! `boss-jobs-internal` door it already uses for the dock probe, so
//! the conductor needs exactly one address to do its whole job.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use boss_policy_client::{AccessTier, CurrentUser, User};

use super::port::{CadenceError, CadenceRepository};
use super::types::{ClaimResult, FiringOutcome, NewFiring};

pub struct CadenceApiState {
    pub repo: Arc<dyn CadenceRepository>,
}

/// Cadence is operator machinery — it decides when the train runs.
/// Two categories pass: operator-tier callers (the conductor stamps
/// `access_tier: operator`), and trusted internal callers, which the
/// extractor defaults to `role=guest` when no `x-boss-user` header
/// arrived — i.e. a loopback sibling or a test harness. The gateway
/// always injects the header for external requests, so a real browser
/// session never lands in the trusted-internal path.
fn is_trusted(user: &User) -> bool {
    user.role == "guest" || user.access_tier == AccessTier::Operator
}

pub fn router(state: CadenceApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/cadence/rules", get(list_rules))
        .route("/api/cadence/rules/{name}/last-firing", get(last_firing))
        .route("/api/cadence/firings", post(claim_firing))
        .route("/api/cadence/firings/{id}/outcome", post(record_outcome))
        .with_state(shared)
}

fn err_response(e: CadenceError) -> Response {
    match e {
        CadenceError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        CadenceError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn list_rules(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.active_rules().await {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => err_response(e),
    }
}

async fn last_firing(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.last_firing(&name).await {
        // `null` for "never fired" — the conductor treats it as
        // "every window is a candidate", so a 404 would be wrong.
        Ok(f) => Json(f).into_response(),
        Err(e) => err_response(e),
    }
}

async fn claim_firing(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewFiring>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if body.firing_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "firing_id is required").into_response();
    }
    match state.repo.claim_firing(&body).await {
        // A lost claim is a normal, expected outcome — not an error.
        // It is reported as 200 + `{"claimed": false}` rather than 409
        // so the conductor's retry policy (which retries 5xx and
        // treats 4xx as fatal) never sees a race as a failure.
        Ok(claimed) => Json(ClaimResult { claimed }).into_response(),
        Err(e) => err_response(e),
    }
}

async fn record_outcome(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<FiringOutcome>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state
        .repo
        .record_outcome(&id, body.rc, body.runtime_secs)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}
