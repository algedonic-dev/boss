//! HTTP API for the Class registry. Reads are open; the one write —
//! `POST /api/classes/batch` — seeds the registry via the public API
//! (replacing the direct `psql -f classes.sql` end-around) and is
//! gated to operator-tier callers (with the `x-sim-origin` bypass).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use boss_core::primitives::{Class, ClassRef};
use boss_policy_client::{AccessTier, CurrentUser};
use serde::Deserialize;
use serde_json::Value;

use crate::port::ClassRepository;

#[derive(Clone)]
pub struct ClassesApiState {
    pub classes: Arc<dyn ClassRepository>,
}

pub fn router(state: ClassesApiState) -> Router {
    Router::new()
        .route("/api/classes/health", get(health))
        .route("/api/classes", get(list_classes))
        .route("/api/classes/batch", post(batch_upsert))
        .route("/api/classes/{subject_kind}/{code}", get(get_class))
        .route(
            "/api/classes/{subject_kind}/{code}/exists",
            get(class_exists),
        )
        .with_state(state)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

/// Standard health probe — every boss-*-api binary exposes one
/// at `/api/<service>/health`. The SPA's MonitoringPage polls
/// this on every page load; a missing endpoint surfaces as 404
/// console spam.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "boss-classes-api",
        "storage": STORAGE,
    }))
}

#[derive(Deserialize)]
struct ListQuery {
    subject_kind: String,
}

async fn list_classes(
    State(state): State<ClassesApiState>,
    Query(q): Query<ListQuery>,
) -> Response {
    match state.classes.list_for_subject_kind(&q.subject_kind).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_class(
    State(state): State<ClassesApiState>,
    Path((subject_kind, code)): Path<(String, String)>,
) -> Response {
    let class_ref = ClassRef::new(subject_kind, code);
    match state.classes.get(&class_ref).await {
        Ok(Some(c)) => Json(c).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such class").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn class_exists(
    State(state): State<ClassesApiState>,
    Path((subject_kind, code)): Path<(String, String)>,
) -> Response {
    let class_ref = ClassRef::new(subject_kind, code);
    match state.classes.exists_active(&class_ref).await {
        Ok(b) => Json(serde_json::json!({ "exists": b })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// One row in a `POST /api/classes/batch` body. Mirrors the `classes`
/// table's authorable columns; `retired_at` / `created_at` /
/// `updated_at` are owned by the table and not accepted here (seeded
/// rows arrive active). Optional fields default so a minimal seed row
/// is `{"subject_kind","code","display_name"}`.
#[derive(Deserialize)]
struct ClassInput {
    subject_kind: String,
    code: String,
    display_name: String,
    #[serde(default)]
    parent_code: Option<String>,
    #[serde(default)]
    member_attribute: Option<String>,
    #[serde(default = "empty_object")]
    metadata: Value,
    #[serde(default)]
    sort_order: i32,
}

fn empty_object() -> Value {
    serde_json::json!({})
}

impl From<ClassInput> for Class {
    fn from(i: ClassInput) -> Self {
        Class {
            subject_kind: i.subject_kind,
            code: i.code,
            display_name: i.display_name,
            parent_code: i.parent_code,
            member_attribute: i.member_attribute,
            metadata: i.metadata,
            sort_order: i.sort_order,
            retired_at: None,
        }
    }
}

/// Batch-upsert Class rows — the single write surface, used to seed
/// the registry from JSON instead of `psql -f classes.sql`. Each row
/// inserts `ON CONFLICT (subject_kind, code) DO NOTHING`, so the call
/// is idempotent.
///
/// Gated to operator-tier callers, with the `x-sim-origin` bypass that
/// every seed path honors (the trusted simulator/seeder masquerades as
/// operators; its requests carry `x-sim-origin: true`, which the
/// request-context middleware scopes into `is_in_sim_chain`). Reads
/// stay open; only this write is privileged.
async fn batch_upsert(
    State(state): State<ClassesApiState>,
    CurrentUser(user): CurrentUser,
    Json(rows): Json<Vec<ClassInput>>,
) -> Response {
    let sim = boss_core::sim_origin::is_in_sim_chain();
    let tier_ok = matches!(user.access_tier, AccessTier::Operator);
    if !(sim || tier_ok) {
        return (StatusCode::FORBIDDEN, "operator tier required").into_response();
    }

    let classes: Vec<Class> = rows.into_iter().map(Into::into).collect();
    match state.classes.batch_upsert(&classes).await {
        Ok(inserted) => Json(serde_json::json!({
            "received": classes.len(),
            "inserted": inserted,
        }))
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemoryClasses;
    use axum::body::to_bytes;
    use axum::http::Request;
    use boss_core::primitives::Class;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    fn employee(code: &str, sort: i32) -> Class {
        Class {
            subject_kind: "employee".into(),
            code: code.into(),
            display_name: code.to_uppercase(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata: json!({}),
            sort_order: sort,
            retired_at: None,
        }
    }

    fn build_app(rows: Vec<Class>) -> Router {
        let state = ClassesApiState {
            classes: Arc::new(InMemoryClasses::new(rows)),
        };
        router(state)
    }

    #[tokio::test]
    async fn list_returns_classes_for_subject_kind() {
        let app = build_app(vec![employee("ceo", 10), employee("cto", 11)]);
        let req = Request::builder()
            .uri("/api/classes?subject_kind=employee")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn get_returns_404_for_missing_class() {
        let app = build_app(vec![employee("ceo", 10)]);
        let req = Request::builder()
            .uri("/api/classes/employee/no-such-role")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn exists_returns_boolean_envelope() {
        let app = build_app(vec![employee("ceo", 10)]);
        let req = Request::builder()
            .uri("/api/classes/employee/ceo/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["exists"], json!(true));
    }

    /// `x-boss-user` JSON for an operator-tier caller. Mirrors the
    /// header the gateway injects + the seed binaries send.
    fn operator_header() -> String {
        json!({
            "id": "automation:test-seed",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
        })
        .to_string()
    }

    fn batch_request(user_header: Option<&str>, body: Value) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/classes/batch")
            .header("content-type", "application/json");
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn batch_upsert_inserts_rows_for_operator() {
        let repo = Arc::new(InMemoryClasses::new(vec![]));
        let app = router(ClassesApiState {
            classes: repo.clone(),
        });
        let body = json!([
            {"subject_kind": "employee", "code": "head-brewer", "display_name": "Head Brewer", "member_attribute": "role", "sort_order": 30},
            {"subject_kind": "employee", "code": "brewer", "display_name": "Brewer", "member_attribute": "role", "sort_order": 32},
        ]);
        let resp = app
            .oneshot(batch_request(Some(&operator_header()), body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["received"], json!(2));
        assert_eq!(v["inserted"], json!(2));

        let stored = repo.list_for_subject_kind("employee").await.unwrap();
        assert_eq!(stored.len(), 2);
    }

    #[tokio::test]
    async fn batch_upsert_is_idempotent_on_conflict() {
        let repo = Arc::new(InMemoryClasses::new(vec![employee("brewer", 32)]));
        let app = router(ClassesApiState {
            classes: repo.clone(),
        });
        // `brewer` already present → DO NOTHING; only `cellar-tech` is new.
        let body = json!([
            {"subject_kind": "employee", "code": "brewer", "display_name": "Brewer", "member_attribute": "role", "sort_order": 32},
            {"subject_kind": "employee", "code": "cellar-tech", "display_name": "Cellar Tech", "member_attribute": "role", "sort_order": 33},
        ]);
        let resp = app
            .oneshot(batch_request(Some(&operator_header()), body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["received"], json!(2));
        assert_eq!(v["inserted"], json!(1), "conflicting row is left untouched");
        assert_eq!(
            repo.list_for_subject_kind("employee").await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn batch_upsert_forbidden_for_non_operator() {
        // Default (no header) → anonymous user, AccessTier::User.
        let app = build_app(vec![]);
        let body = json!([
            {"subject_kind": "employee", "code": "brewer", "display_name": "Brewer"},
        ]);
        let resp = app.oneshot(batch_request(None, body)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn batch_upsert_bypassed_by_sim_origin() {
        // Sim traffic carries `x-sim-origin: true`, which the request-
        // context middleware scopes into `is_in_sim_chain`. The router
        // under test omits that middleware, so we set the task-local
        // directly to exercise the bypass branch with a non-operator
        // (anonymous) caller.
        let repo = Arc::new(InMemoryClasses::new(vec![]));
        let app = router(ClassesApiState {
            classes: repo.clone(),
        });
        let body = json!([
            {"subject_kind": "employee", "code": "brewer", "display_name": "Brewer", "member_attribute": "role", "sort_order": 32},
        ]);
        let resp =
            boss_core::sim_origin::with_sim_chain(true, app.oneshot(batch_request(None, body)))
                .await
                .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            repo.list_for_subject_kind("employee").await.unwrap().len(),
            1
        );
    }
}
