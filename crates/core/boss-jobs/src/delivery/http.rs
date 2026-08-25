//! Axum routes for the delivery-policy registry.
//!
//! Same door, same reason as `cadence::http`: the train conductor runs
//! OUTSIDE the cluster without a database connection, and reaches the
//! `boss-jobs-internal` address it already uses for every other call.
//! One address does its whole job.
//!
//! Both routes answer `null` rather than 404 for "no such policy". The
//! conductor's retry policy treats 4xx as fatal and 5xx as a blip, so a
//! missing row must not arrive as either: it is an ANSWER, and the
//! answer means "fall back to the compiled values and say so".

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use boss_policy_client::{AccessTier, CurrentUser, User};

use super::port::{DeliveryPolicyError, DeliveryPolicyRepository};

pub struct DeliveryPolicyApiState {
    pub repo: Arc<dyn DeliveryPolicyRepository>,
}

/// Delivery policy is operator machinery — it decides how the pipeline
/// treats a car. Same two categories as cadence: operator-tier callers
/// (the conductor stamps `access_tier: operator`), and trusted internal
/// callers, which the extractor defaults to `role=guest` when no
/// `x-boss-user` header arrived. The gateway always injects the header
/// for external requests, so a browser session never lands here.
fn is_trusted(user: &User) -> bool {
    user.role == "guest" || user.access_tier == AccessTier::Operator
}

pub fn router(state: DeliveryPolicyApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/delivery/policy/{name}", get(active_policy))
        .route(
            "/api/delivery/policy/{name}/versions/{version}",
            get(policy_version),
        )
        .with_state(shared)
}

fn err_response(e: DeliveryPolicyError) -> Response {
    match e {
        DeliveryPolicyError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        DeliveryPolicyError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn active_policy(
    State(state): State<Arc<DeliveryPolicyApiState>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.active_policy(&name).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => err_response(e),
    }
}

async fn policy_version(
    State(state): State<Arc<DeliveryPolicyApiState>>,
    CurrentUser(user): CurrentUser,
    Path((name, version)): Path<(String, i32)>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.policy_version(&name, version).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => err_response(e),
    }
}
