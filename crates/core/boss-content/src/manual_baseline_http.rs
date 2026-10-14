//! Privileged raw inventory and present observation use authoritative policy.
use crate::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use boss_policy_client::{CurrentUser, Decision, PolicyClient, Scope, User, controls};
use std::sync::Arc;

pub struct BaselineApiState {
    pub repo: Arc<dyn ManualBaselineRepository>,
    pub policy: Arc<dyn PolicyClient>,
    pub publisher: Option<boss_core::publisher::DomainPublisher>,
}

pub fn router(state: BaselineApiState) -> Router {
    Router::new()
        .route("/api/content/manual-inventory", get(inventory))
        .route("/api/content/manual-baseline", post(observe))
        .with_state(Arc::new(state))
}

async fn authorize(
    state: &BaselineApiState,
    user: &User,
    pair: controls::Pair,
) -> Result<(), Response> {
    match state.policy.ask(user, pair).await {
        Ok(Decision::Allow { scope: Scope::All }) => Ok(()),
        Ok(_) => Err((
            StatusCode::FORBIDDEN,
            "requires explicit manual authority at scope all",
        )
            .into_response()),
        Err(error) => Err(error.into_response()),
    }
}

fn error(error: crate::ContentError) -> Response {
    match error {
        crate::ContentError::Validation(message) => (StatusCode::CONFLICT, message).into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "complete manual evidence unavailable",
        )
            .into_response(),
    }
}

async fn inventory(
    State(state): State<Arc<BaselineApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if let Err(response) = authorize(&state, &user, controls::READ_MANUAL_INVENTORY).await {
        return response;
    }
    match state.repo.manual_inventory().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => error(e),
    }
}

async fn observe(
    State(state): State<Arc<BaselineApiState>>,
    CurrentUser(user): CurrentUser,
    Json(admission): Json<BaselineAdmission>,
) -> Response {
    for pair in [
        controls::READ_MANUAL_INVENTORY,
        controls::CREATE_MANUAL_BASELINE,
    ] {
        if let Err(response) = authorize(&state, &user, pair).await {
            return response;
        }
    }
    let Some(publisher) = &state.publisher else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "observation publisher unavailable",
        )
            .into_response();
    };
    let actor = match user.id.parse() {
        Ok(actor) => actor,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid observer").into_response(),
    };
    let stamp = publisher.stamp_with_actor(actor).await;
    match state.repo.observe_manual_baseline(&admission, &stamp).await {
        Ok(observation) => Json(observation).into_response(),
        Err(e) => error(e),
    }
}
