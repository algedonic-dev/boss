//! Minimal HTTP surface: a health + readiness probe pair. No write surface;
//! the dispatcher's only outputs are PUTs to jobs-api.
//!
//! `/api/dispatcher/health` answers 200 while the PROCESS is up — necessary
//! but NOT sufficient: the consumer loops run detached and can die while the
//! process keeps serving 200 (a NATS blip, JetStream not ready at cold start
//! under a no-restart launcher). `/api/dispatcher/readyz` reports the actual
//! consumer liveness (see [`crate::liveness`]) so operators — and the brewery
//! sim's pre-Go readiness gate — can tell "up" from "actually working."

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use crate::liveness::DispatcherLiveness;

pub fn router(live: Arc<DispatcherLiveness>) -> Router {
    Router::new()
        .route("/api/dispatcher/health", get(health))
        .route("/api/dispatcher/readyz", get(readyz))
        .with_state(live)
}

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-dispatcher",
        env!("CARGO_PKG_VERSION"),
        "nats-subscriber",
    ))
}

/// Real readiness: are both durable consumers bound + draining? Returns
/// `{ready, assigning, assignment_events, rules_running, rules_events,
/// last_event_unix}`. `ready=false` while health is 200 is the exact
/// "process up, but assigning nothing, so Jobs never close" failure.
async fn readyz(State(live): State<Arc<DispatcherLiveness>>) -> Json<serde_json::Value> {
    Json(live.snapshot())
}
