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

use crate::cascade;
use crate::liveness::DispatcherLiveness;
use crate::rules::registry::load_active_rules;

/// HTTP state: the consumer-liveness handle + the Postgres pool, so the
/// read-only `/api/dispatcher/rules` surface can serve the rule registry
/// (the `dispatcher_rules` table) for the cascade visualization.
#[derive(Clone)]
pub struct HttpState {
    pub live: Arc<DispatcherLiveness>,
    pub pool: sqlx::PgPool,
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/api/dispatcher/health", get(health))
        .route("/api/dispatcher/readyz", get(readyz))
        .route("/api/dispatcher/rules", get(rules))
        .with_state(state)
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
async fn readyz(State(state): State<HttpState>) -> Json<serde_json::Value> {
    Json(state.live.snapshot())
}

/// Read-only rule-registry surface for the cascade visualization. Serves
/// the ACTIVE rows of the `dispatcher_rules` registry table (name,
/// on_event, when, do/args) plus the static cascade metadata: per-handler
/// emitted events + the jobs-api/external "system edges" that close the
/// feedback loops. Queries the table per request — a low-traffic admin
/// view, and reading live reflects any rule edits without a restart.
async fn rules(State(state): State<HttpState>) -> Json<serde_json::Value> {
    let rules_value = match load_active_rules(&state.pool).await {
        Ok(raw) => serde_json::to_value(&raw.rules).unwrap_or_else(|_| serde_json::json!([])),
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("load dispatcher_rules: {e}"),
                "rules": [], "handler_emits": {}, "system_edges": [],
            }));
        }
    };
    let mut out = serde_json::Map::new();
    out.insert("rules".into(), rules_value);
    out.insert(
        "handler_emits".into(),
        serde_json::to_value(cascade::handler_emits()).unwrap_or_default(),
    );
    out.insert(
        "system_edges".into(),
        serde_json::to_value(cascade::system_edges()).unwrap_or_default(),
    );
    Json(serde_json::Value::Object(out))
}
