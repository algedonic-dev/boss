//! HTTP surface: health + readiness probes, the read-only cascade-viz
//! `rules` feed, and the rule-authoring write endpoints (create-draft /
//! validate / publish / retire) that back the SPA authoring UI. The
//! authoring writes go through `crate::rules::authoring`; the running
//! RulesRunner picks up a published change on its next restart (live
//! hot-reload is a planned follow-up).
//!
//! `/api/dispatcher/health` answers 200 while the PROCESS is up — necessary
//! but NOT sufficient: the consumer loops run detached and can die while the
//! process keeps serving 200 (a NATS blip, JetStream not ready at cold start
//! under a no-restart launcher). `/api/dispatcher/readyz` reports the actual
//! consumer liveness (see [`crate::liveness`]) so operators — and the brewery
//! sim's pre-Go readiness gate — can tell "up" from "actually working."

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::cascade;
use crate::liveness::DispatcherLiveness;
use crate::rules::authoring::{self, AuthoringError};
use crate::rules::registry::{RawRule, load_active_rules};

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
        // GET serves the cascade-viz feed; POST creates a new rule draft.
        .route("/api/dispatcher/rules", get(rules).post(create_rule_draft))
        .route("/api/dispatcher/rules/_validate", post(validate_rule))
        .route(
            "/api/dispatcher/rules/{name}/versions",
            get(list_rule_versions),
        )
        .route(
            "/api/dispatcher/rules/{name}/versions/{version}",
            get(get_rule_version),
        )
        .route("/api/dispatcher/rules/{name}/publish", post(publish_rule))
        .route("/api/dispatcher/rules/{name}/retire", post(retire_rule))
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

// ---------------------------------------------------------------------------
// Rule authoring (control-plane writes) — see crate::rules::authoring.
// ---------------------------------------------------------------------------

fn authoring_err(e: AuthoringError) -> Response {
    let code = match &e {
        AuthoringError::NotFound(_) => StatusCode::NOT_FOUND,
        AuthoringError::Invalid(_) => StatusCode::BAD_REQUEST,
        AuthoringError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (code, e.to_string()).into_response()
}

/// `POST /api/dispatcher/rules` — append a new draft version of a rule.
/// Body is the rule spec (name, on_event, when?, do[], delay?). The draft is
/// validated (must load via `Rule::from_raw`) before it persists; `201` on
/// success returns the stored draft.
async fn create_rule_draft(State(state): State<HttpState>, Json(raw): Json<RawRule>) -> Response {
    match authoring::create_draft(&state.pool, &raw).await {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `POST /api/dispatcher/rules/_validate` — dry-run a draft without
/// persisting. Returns `{ ok, error }` so the authoring UI can surface
/// topic/predicate/arg parse errors live, before publish.
async fn validate_rule(Json(raw): Json<RawRule>) -> Json<serde_json::Value> {
    match authoring::validate(&raw) {
        Ok(()) => Json(serde_json::json!({ "ok": true, "error": null })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// `GET /api/dispatcher/rules/{name}/versions` — all versions, oldest first
/// (draft + active + retired).
async fn list_rule_versions(State(state): State<HttpState>, Path(name): Path<String>) -> Response {
    match authoring::list_versions(&state.pool, &name).await {
        Ok(vs) => Json(vs).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `GET /api/dispatcher/rules/{name}/versions/{version}` — one version.
async fn get_rule_version(
    State(state): State<HttpState>,
    Path((name, version)): Path<(String, i32)>,
) -> Response {
    match authoring::get_version(&state.pool, &name, version).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `POST /api/dispatcher/rules/{name}/publish` — activate the latest draft,
/// retiring the prior active version.
async fn publish_rule(State(state): State<HttpState>, Path(name): Path<String>) -> Response {
    match authoring::publish(&state.pool, &name).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `POST /api/dispatcher/rules/{name}/retire` — retire the active version.
async fn retire_rule(State(state): State<HttpState>, Path(name): Path<String>) -> Response {
    match authoring::retire(&state.pool, &name).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => authoring_err(e),
    }
}
