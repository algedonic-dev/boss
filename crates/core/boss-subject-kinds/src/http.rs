//! HTTP API for the SubjectKind registry. Reads are open; the one
//! write — `PATCH /api/subject-kinds/{kind}/metadata` — merges keys
//! into one kind's metadata, asks policy first, and records the
//! `subject_kind.updated` fact with the row (backlog abc2e9d5).

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch};
use axum::{Json, Router};
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use boss_policy_client::{CurrentUser, PolicyClient, User, controls};
use serde_json::{Map, Value};

use crate::port::{SubjectKindError, SubjectKindRepository};

#[derive(Clone)]
pub struct SubjectKindsApiState {
    pub subject_kinds: Arc<dyn SubjectKindRepository>,
    /// Asked by the write door, never by the reads: the registry's
    /// vocabulary is what every page and every write validates against,
    /// so reading it stays open.
    pub policy: Arc<dyn PolicyClient>,
}

/// The metadata door's body ceiling, in bytes (review of car abc2e9d5,
/// LOW-2). A kind's metadata is a handful of short descriptive keys;
/// 16 KiB is generous for that and refuses a registry row being used as
/// a document store — axum's default would admit 2 MiB.
pub const METADATA_BODY_LIMIT: usize = 16 * 1024;

pub fn router(state: SubjectKindsApiState) -> Router {
    Router::new()
        .route("/api/subject-kinds/health", get(health))
        .route("/api/subject-kinds", get(list_active))
        .route("/api/subject-kinds/{kind}", get(get_kind))
        .route("/api/subject-kinds/{kind}/exists", get(kind_exists))
        .route("/api/subject-kinds/{kind}/children", get(children_of))
        .route(
            "/api/subject-kinds/{kind}/metadata",
            patch(patch_metadata).layer(DefaultBodyLimit::max(METADATA_BODY_LIMIT)),
        )
        .with_state(state)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

/// Standard health probe — see boss-classes/src/http.rs for context.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "boss-subject-kinds-api",
        "storage": STORAGE,
    }))
}

async fn list_active(State(state): State<SubjectKindsApiState>) -> Response {
    match state.subject_kinds.list_active().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_kind(State(state): State<SubjectKindsApiState>, Path(kind): Path<String>) -> Response {
    match state.subject_kinds.get(&kind).await {
        Ok(Some(k)) => Json(k).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such subject kind").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn kind_exists(
    State(state): State<SubjectKindsApiState>,
    Path(kind): Path<String>,
) -> Response {
    match state.subject_kinds.exists_active(&kind).await {
        Ok(b) => Json(serde_json::json!({ "exists": b })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn children_of(
    State(state): State<SubjectKindsApiState>,
    Path(kind): Path<String>,
) -> Response {
    match state.subject_kinds.children_of(&kind).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `PATCH /api/subject-kinds/{kind}/metadata` — merge keys into one
/// kind's metadata: a key replaces, a key sent as `null` is deleted, a
/// key not named is kept (the jobs API's metadata-PATCH semantics). The
/// answer is the row as it now stands.
///
/// WHY IT EXISTS (backlog abc2e9d5, 2026-09-28). The registry answered
/// only GETs, so setting `metadata.module` on eight kinds for the
/// /it/registry/subjects page reached for a migration UPDATE — a change
/// the audit log never hears of, which `migrations-declare-schema-only`
/// refuses for this table. This is the door a migration is not.
///
/// Answers, in this order: no resolvable caller → 401 (there is no one
/// to ask policy about, and the fact must name its author); policy says
/// no to Update on `subject-kind` → 403 with policy's reason
/// (platform-admin's alone in the core defaults); policy cannot answer
/// → its own 503 (fail closed); a reserved or empty key or a malformed
/// `module` → 422 naming it; no such kind → 404. A body over
/// [`METADATA_BODY_LIMIT`] is 413 before any of these. A patch that
/// changes nothing answers 200 with the row and records nothing.
///
/// Only `metadata` moves, and not all of it: the keys code reads as the
/// kind's semantics (`port::RESERVED_METADATA_KEYS` — `birth`,
/// `calendar_reservable`) are refused. The key, label, parent and
/// ownership have no write door; declaring a new kind has none yet
/// either (ca7bf46c).
async fn patch_metadata(
    State(state): State<SubjectKindsApiState>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
    Json(patch): Json<Map<String, Value>>,
) -> Response {
    let actor = match authorize_update(state.policy.as_ref(), &user).await {
        Ok(actor) => actor,
        Err(refusal) => return refusal,
    };
    match state
        .subject_kinds
        .patch_metadata(&kind, &patch, &EventStamp::new("subject-kinds", actor))
        .await
    {
        Ok(Some(row)) => Json(row).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such subject kind").into_response(),
        Err(e @ SubjectKindError::InvalidPatch(_)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The write door's policy question: Update on `subject-kind`, through
/// the registry-write ladder the Class registry's doors share
/// ([`boss_policy_client::writes::require_registry_write`], lifted from
/// here in backlog 553cf479 so the two cannot answer it two ways). On an
/// allow it returns the caller's actor, which is what the fact is signed
/// with — so no fallback author exists anywhere on this path.
async fn authorize_update(policy: &dyn PolicyClient, user: &User) -> Result<ActorId, Response> {
    boss_policy_client::writes::require_registry_write(policy, user, controls::UPDATE_SUBJECT_KIND)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemorySubjectKinds;
    use crate::port::SubjectKind;
    use axum::body::to_bytes;
    use axum::http::Request;
    use boss_policy_client::{Action, Resource};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    fn sk(kind: &str, parent: Option<&str>) -> SubjectKind {
        SubjectKind {
            kind: kind.into(),
            label: kind.to_string(),
            parent_kind: parent.map(String::from),
            description: None,
            owning_team: "platform".into(),
            metadata: json!({}),
            sort_order: 0,
            retired_at: None,
        }
    }

    fn app(rows: Vec<SubjectKind>) -> Router {
        let state = SubjectKindsApiState {
            subject_kinds: Arc::new(InMemorySubjectKinds::new(rows)),
            policy: Arc::new(boss_policy_client::FakePolicyClient::deny_all()),
        };
        router(state)
    }

    // ---- the metadata door (backlog abc2e9d5) -------------------------

    /// The core default rules, as the live policy service seeds them —
    /// so these tests judge the door against the grant that ships, not
    /// against a grant written for the test.
    fn default_policy() -> Arc<dyn PolicyClient> {
        Arc::new(
            boss_policy_client::defaults::default_rules()
                .into_iter()
                .fold(boss_policy_client::FakePolicyClient::builder(), |b, r| {
                    b.allow(r.role, r.action, r.resource, r.scope)
                })
                .build(),
        )
    }

    /// A policy service that cannot be asked.
    struct DarkPolicy;

    #[async_trait::async_trait]
    impl PolicyClient for DarkPolicy {
        async fn check(
            &self,
            _: &User,
            _: Action,
            _: Resource,
        ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
            Err(boss_policy_client::PolicyClientError::Unreachable(
                "dark".into(),
            ))
        }
        async fn scope_predicate(
            &self,
            _: &User,
            _: Resource,
        ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
            Err(boss_policy_client::PolicyClientError::Unreachable(
                "dark".into(),
            ))
        }
    }

    fn door(repo: Arc<InMemorySubjectKinds>, policy: Arc<dyn PolicyClient>) -> Router {
        router(SubjectKindsApiState {
            subject_kinds: repo,
            policy,
        })
    }

    fn signed(id: &str, role: &str) -> String {
        json!({
            "id": id,
            "role": role,
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
        })
        .to_string()
    }

    fn patch_request(user: Option<&str>, kind: &str, body: Value) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("PATCH")
            .uri(format!("/api/subject-kinds/{kind}/metadata"))
            .header("content-type", "application/json");
        if let Some(h) = user {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::from(body.to_string())).unwrap()
    }

    async fn body_json(resp: Response) -> Value {
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn seeded() -> Arc<InMemorySubjectKinds> {
        let mut asset = sk("asset", None);
        asset.metadata = json!({"module": "old", "icon": "box"});
        Arc::new(InMemorySubjectKinds::new(vec![asset, sk("campaign", None)]))
    }

    /// The door the packet asked for: platform-admin merges keys, a
    /// null deletes, an unnamed key is kept, the answer is the row as it
    /// now stands, and exactly one `subject_kind.updated` names the
    /// change and its signer — none for the restatement that follows.
    #[tokio::test]
    async fn platform_admin_merges_metadata_and_one_fact_names_the_change() {
        let repo = seeded();
        let admin = signed("claude@algedonic.dev", "platform-admin");
        let body = json!({"module": "equipment", "icon": null});
        let resp = door(repo.clone(), default_policy())
            .oneshot(patch_request(Some(&admin), "asset", body.clone()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let row = body_json(resp).await;
        assert_eq!(row["kind"], json!("asset"));
        assert_eq!(row["metadata"], json!({"module": "equipment"}));

        let again = door(repo.clone(), default_policy())
            .oneshot(patch_request(Some(&admin), "asset", body))
            .await
            .unwrap();
        assert_eq!(
            again.status(),
            StatusCode::OK,
            "a restatement still answers"
        );

        let events = repo.recorded_events();
        assert_eq!(events.len(), 1, "one fact, none for the repeat: {events:?}");
        let e = &events[0];
        assert_eq!(e.kind, crate::port::SUBJECT_KIND_UPDATED);
        assert_eq!(e.source, "subject-kinds");
        assert_eq!(e.payload["kind"], json!("asset"));
        assert_eq!(e.payload["changed"], json!(["icon", "module"]));
        assert_eq!(e.payload["before"], json!({"icon": "box", "module": "old"}));
        assert_eq!(e.payload["after"], json!({"module": "equipment"}));
        assert_eq!(
            e.payload["updated_by"], e.payload["_actor"],
            "the fact's author is the caller the request was signed as"
        );
        assert!(
            e.payload["updated_by"]
                .as_str()
                .is_some_and(|a| a.contains("claude@algedonic.dev")),
            "{:?}",
            e.payload
        );
    }

    /// Every refusal writes nothing and records nothing: no identity is
    /// 401, a role the defaults grant nothing is 403, a policy service
    /// that cannot answer is its own 503 — never an allow.
    #[tokio::test]
    async fn the_door_refuses_before_it_writes() {
        let body = json!({"module": "equipment"});
        let cases: [(Option<String>, Arc<dyn PolicyClient>, StatusCode); 3] = [
            (None, default_policy(), StatusCode::UNAUTHORIZED),
            (
                Some(signed("emp-audit", "audit-readonly")),
                default_policy(),
                StatusCode::FORBIDDEN,
            ),
            (
                Some(signed("claude@algedonic.dev", "platform-admin")),
                Arc::new(DarkPolicy),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];
        for (user, policy, want) in cases {
            let repo = seeded();
            let resp = door(repo.clone(), policy)
                .oneshot(patch_request(user.as_deref(), "asset", body.clone()))
                .await
                .unwrap();
            assert_eq!(resp.status(), want, "caller {user:?}");
            assert!(repo.recorded_events().is_empty(), "caller {user:?}");
            let held = repo.get("asset").await.unwrap().unwrap();
            assert_eq!(held.metadata["module"], json!("old"), "caller {user:?}");
        }
    }

    /// LOW-1 of the review: `is_anonymous` reads the ROLE, while the
    /// actor is resolved from the ID — so a header claiming the anonymous
    /// id with a platform role passed the identity check and the fact
    /// was signed by this service's automation. No resolvable actor is
    /// now 401, whatever role the header claims.
    #[tokio::test]
    async fn a_caller_with_no_resolvable_actor_is_refused_whatever_its_role() {
        let repo = seeded();
        let forged = signed(User::ANONYMOUS_ID, "platform-admin");
        let resp = door(repo.clone(), default_policy())
            .oneshot(patch_request(
                Some(&forged),
                "asset",
                json!({"module": "x"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(repo.recorded_events().is_empty());
    }

    /// MED-1 of the review, at the door: a reserved key is 422 naming
    /// the key, and nothing is written or recorded.
    #[tokio::test]
    async fn a_reserved_key_is_422_naming_it() {
        let repo = seeded();
        let admin = signed("claude@algedonic.dev", "platform-admin");
        let resp = door(repo.clone(), default_policy())
            .oneshot(patch_request(
                Some(&admin),
                "asset",
                json!({"birth": "job"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("birth") && text.contains("reserved"),
            "{text}"
        );
        assert!(repo.recorded_events().is_empty());
        assert!(
            repo.get("asset")
                .await
                .unwrap()
                .unwrap()
                .metadata
                .get("birth")
                .is_none()
        );
    }

    /// LOW-2: the door's body is bounded — a registry row's description
    /// is not a document store.
    #[tokio::test]
    async fn an_oversized_body_is_refused() {
        let repo = seeded();
        let admin = signed("claude@algedonic.dev", "platform-admin");
        let big = "x".repeat(METADATA_BODY_LIMIT + 1);
        let resp = door(repo.clone(), default_policy())
            .oneshot(patch_request(Some(&admin), "asset", json!({"note": big})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(repo.recorded_events().is_empty());
    }

    /// A kind the registry does not carry is 404 — the door never mints
    /// a kind (declaring one is not this door's, ca7bf46c) — and a body
    /// that is not a JSON object is refused before anything is written.
    #[tokio::test]
    async fn an_unknown_kind_is_404_and_a_non_object_body_is_refused() {
        let repo = seeded();
        let admin = signed("claude@algedonic.dev", "platform-admin");
        let resp = door(repo.clone(), default_policy())
            .oneshot(patch_request(
                Some(&admin),
                "no-such-kind",
                json!({"module": "x"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let resp = door(repo.clone(), default_policy())
            .oneshot(patch_request(Some(&admin), "asset", json!(["module", "x"])))
            .await
            .unwrap();
        assert!(resp.status().is_client_error(), "{}", resp.status());
        assert!(repo.recorded_events().is_empty());
    }

    #[tokio::test]
    async fn list_returns_active_rows() {
        let app = app(vec![sk("asset", None), sk("vendor", None)]);
        let req = Request::builder()
            .uri("/api/subject-kinds")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn get_returns_404_for_unknown_kind() {
        let app = app(vec![sk("asset", None)]);
        let req = Request::builder()
            .uri("/api/subject-kinds/nope")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn exists_endpoint_envelope_shape() {
        let app = app(vec![sk("asset", None)]);
        let req = Request::builder()
            .uri("/api/subject-kinds/asset/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["exists"], json!(true));
    }

    #[tokio::test]
    async fn children_endpoint_walks_parent_kind() {
        let app = app(vec![
            sk("account", None),
            sk("medical-practice", Some("account")),
            sk("wholesale-customer", Some("account")),
        ]);
        let req = Request::builder()
            .uri("/api/subject-kinds/account/children")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let kinds: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, vec!["medical-practice", "wholesale-customer"]);
    }
}
