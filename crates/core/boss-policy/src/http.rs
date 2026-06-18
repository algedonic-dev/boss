//! HTTP surface for boss-policy-api.
//!
//! Three groups of endpoints (per the design doc):
//!   - hot path: POST /check, POST /check-batch
//!   - frontend read: GET /my-scope (takes ?user_id=X)
//!   - admin: list/upsert/deactivate rules + user-overrides
//!
//! Session 1 ships the hot path + minimal admin. The full admin matrix
//! UI lands in session 2 on top of the same endpoints.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use boss_policy_client::engine::PolicyEngine;
use boss_policy_client::port::{PolicyError, PolicyRepository};
use boss_policy_client::types::{
    Action, Decision, PolicyRule, Resource, Scope, User, UserOverride,
};

pub struct PolicyApiState<R: PolicyRepository> {
    pub repo: Arc<R>,
    pub engine: Arc<PolicyEngine<R>>,
}

pub fn router<R: PolicyRepository + 'static>(state: PolicyApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/policy/health", get(health))
        .route("/api/policy/check", post(check::<R>))
        .route("/api/policy/check-batch", post(check_batch::<R>))
        .route("/api/policy/my-scope", post(my_scope::<R>))
        .route(
            "/api/policy/rules",
            get(list_rules::<R>).post(upsert_rule::<R>),
        )
        .route(
            "/api/policy/rules/{id}",
            get(get_rule::<R>)
                .put(upsert_rule::<R>)
                .delete(deactivate_rule::<R>),
        )
        .route(
            "/api/policy/user-overrides",
            post(upsert_user_override::<R>),
        )
        .route(
            "/api/policy/user-overrides/{user_id}",
            get(list_user_overrides::<R>).delete(deactivate_user_override::<R>),
        )
        .with_state(shared)
}

fn err_response(e: PolicyError) -> Response {
    match e {
        PolicyError::NotFound(m) => (StatusCode::NOT_FOUND, m).into_response(),
        PolicyError::Conflict(m) => (StatusCode::CONFLICT, m).into_response(),
        PolicyError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-policy-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

// ----- check ---------------------------------------------------------------

#[derive(Deserialize)]
struct CheckBody {
    user: User,
    action: Action,
    resource: Resource,
}

async fn check<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Json(body): Json<CheckBody>,
) -> Response {
    match state
        .engine
        .check(&body.user, body.action, body.resource)
        .await
    {
        Ok(d) => Json(d).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Deserialize)]
struct CheckBatchBody {
    user: User,
    checks: Vec<CheckPair>,
}

#[derive(Deserialize, Serialize)]
struct CheckPair {
    action: Action,
    resource: Resource,
}

#[derive(Serialize)]
struct CheckBatchResult {
    action: Action,
    resource: Resource,
    decision: Decision,
}

async fn check_batch<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Json(body): Json<CheckBatchBody>,
) -> Response {
    let mut out = Vec::with_capacity(body.checks.len());
    for c in body.checks {
        let resource = c.resource.clone();
        match state.engine.check(&body.user, c.action, resource).await {
            Ok(d) => out.push(CheckBatchResult {
                action: c.action,
                resource: c.resource,
                decision: d,
            }),
            Err(e) => return err_response(e),
        }
    }
    Json(out).into_response()
}

// ----- my-scope ------------------------------------------------------------

#[derive(Deserialize)]
struct MyScopeBody {
    user: User,
}

#[derive(Serialize)]
struct MyScopeResult {
    user_id: String,
    role: String,
    rules: Vec<ScopeEntry>,
}

#[derive(Serialize)]
struct ScopeEntry {
    resource: Resource,
    action: Action,
    scope: Scope,
}

async fn my_scope<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Json(body): Json<MyScopeBody>,
) -> Response {
    let mut entries = Vec::new();
    // Iterate the platform's shipped resources (defaults::shipped_resources)
    // so the discovery endpoint covers everything `default_rules` enumerates
    // Read access against — including the registry resources (job_kind,
    // step_plugin) the SPA needs for /workflows + /admin/step-plugins
    // nav-gating.
    for resource in boss_policy_client::defaults::shipped_resources() {
        for action in [
            Action::Read,
            Action::Create,
            Action::Update,
            Action::Close,
            Action::SignOff,
            Action::Delete,
        ] {
            match state
                .engine
                .check(&body.user, action, resource.clone())
                .await
            {
                Ok(Decision::Allow { scope }) => entries.push(ScopeEntry {
                    resource: resource.clone(),
                    action,
                    scope,
                }),
                Ok(Decision::Deny { .. }) => {}
                Err(e) => return err_response(e),
            }
        }
    }

    Json(MyScopeResult {
        user_id: body.user.id.clone(),
        role: body.user.role.clone(),
        rules: entries,
    })
    .into_response()
}

// ----- rules admin ---------------------------------------------------------

async fn list_rules<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
) -> Response {
    match state.repo.list_rules().await {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => err_response(e),
    }
}

async fn get_rule<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.repo.rule_for(&id).await {
        Ok(Some(r)) => Json(r).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("rule {id} not found")).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Deserialize)]
struct UpsertRuleBody {
    rule: PolicyRule,
    changed_by: String,
}

async fn upsert_rule<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Json(body): Json<UpsertRuleBody>,
) -> Response {
    match state.repo.upsert_rule(&body.rule, &body.changed_by).await {
        Ok(()) => (StatusCode::OK, Json(body.rule)).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Deserialize)]
struct DeactivateQuery {
    changed_by: String,
}

async fn deactivate_rule<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Path(id): Path<String>,
    Query(q): Query<DeactivateQuery>,
) -> Response {
    match state.repo.deactivate_rule(&id, &q.changed_by).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}

// ----- user-overrides admin -----------------------------------------------

async fn list_user_overrides<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Path(user_id): Path<String>,
) -> Response {
    match state.repo.list_user_overrides(&user_id).await {
        Ok(ovs) => Json(ovs).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Deserialize)]
struct UpsertOverrideBody {
    #[serde(rename = "override")]
    ov: UserOverride,
    changed_by: String,
}

async fn upsert_user_override<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Json(body): Json<UpsertOverrideBody>,
) -> Response {
    match state
        .repo
        .upsert_user_override(&body.ov, &body.changed_by)
        .await
    {
        Ok(()) => (StatusCode::CREATED, Json(body.ov)).into_response(),
        Err(e) => err_response(e),
    }
}

async fn deactivate_user_override<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Path(id): Path<String>,
    Query(q): Query<DeactivateQuery>,
) -> Response {
    match state
        .repo
        .deactivate_user_override(&id, &q.changed_by)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}
