//! HTTP surface for `boss-content-api`. Covers bulletins (v1a) and the
//! company manual (v1c).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::ContentError;

use crate::port::ContentRepository;
use crate::types::{BulletinDraft, BulletinPatch, ManualPatch, ManualSectionDraft, UserContext};

pub struct ContentApiState {
    pub repo: Arc<dyn ContentRepository>,
    /// Domain event publisher. Optional so test setups that don't
    /// wire NATS keep working; production binaries always attach a
    /// `PgAuditWriter` so bulletin events land in `audit_log`.
    pub publisher: Option<boss_core::publisher::DomainPublisher>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router(state: ContentApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/content/health", get(health))
        .route("/api/content/bulletins/my-day", get(list_my_day))
        .route("/api/content/bulletins", get(list_all).post(create))
        .route("/api/content/bulletins/{id}", put(update).delete(remove))
        .route("/api/content/bulletins/{id}/dismiss", post(dismiss))
        .route("/api/content/manual", get(manual_tree).post(section_create))
        .route("/api/content/manual-history/{*slug}", get(section_history))
        .route(
            "/api/content/manual/{*slug}",
            get(section_get).put(section_update),
        )
        .with_state(shared)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-content-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

/// Pull the caller's identity from the `X-Boss-User` header (injected
/// by the gateway). v1a uses this for audience filtering + dismissal;
/// later we'll also use it for policy checks. Returns a status + body
/// tuple so callers shape the `Response` at the call site — avoids the
/// `result_large_err` hit from carrying a full `Response` in `Err`.
fn user_from_headers(headers: &HeaderMap) -> Result<UserContext, (StatusCode, String)> {
    let Some(raw) = headers.get("x-boss-user").and_then(|v| v.to_str().ok()) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "missing X-Boss-User header".into(),
        ));
    };
    serde_json::from_str::<UserContext>(raw)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("bad user header: {e}")))
}

fn err(e: ContentError) -> Response {
    match e {
        ContentError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        ContentError::Validation(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        ContentError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
    }
}

// --- bulletins ------------------------------------------------------------

#[derive(Deserialize)]
struct ListMyDayQuery {
    #[serde(default)]
    include_dismissed: bool,
}

async fn list_my_day(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Query(q): Query<ListMyDayQuery>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let today = boss_clock_client::now_from(&state.clock).await.date_naive();
    match state
        .repo
        .list_bulletins_for(&user, today, q.include_dismissed)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn list_all(State(state): State<Arc<ContentApiState>>) -> Response {
    match state.repo.list_all_bulletins().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn create(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Json(draft): Json<BulletinDraft>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.repo.create_bulletin_at(draft, &user.id, now).await {
        Ok(b) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    crate::events::BULLETIN_CREATED,
                    serde_json::to_value(&b).unwrap_or_default(),
                    now,
                )
                .await;
            }
            (StatusCode::CREATED, Json(b)).into_response()
        }
        Err(e) => err(e),
    }
}

async fn update(
    State(state): State<Arc<ContentApiState>>,
    Path(id): Path<Uuid>,
    _headers: HeaderMap,
    Json(patch): Json<BulletinPatch>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.repo.update_bulletin_at(id, patch, now).await {
        Ok(b) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    crate::events::BULLETIN_UPDATED,
                    serde_json::to_value(&b).unwrap_or_default(),
                    now,
                )
                .await;
            }
            Json(b).into_response()
        }
        Err(e) => err(e),
    }
}

async fn remove(
    State(state): State<Arc<ContentApiState>>,
    Path(id): Path<Uuid>,
    _headers: HeaderMap,
) -> Response {
    match state.repo.delete_bulletin(id).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    crate::events::BULLETIN_DELETED,
                    serde_json::json!({ "id": id, "deleted_at": now }),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err(e),
    }
}

async fn dismiss(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.repo.dismiss_bulletin_at(id, &user.id, now).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    crate::events::BULLETIN_DISMISSED,
                    serde_json::json!({
                        "bulletin_id": id,
                        "employee_id": user.id,
                        "dismissed_at": now,
                    }),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err(e),
    }
}

// --- manual ---------------------------------------------------------------

async fn manual_tree(State(state): State<Arc<ContentApiState>>, headers: HeaderMap) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.repo.manual_tree(&user).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn section_get(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.repo.get_section(&slug, &user).await {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("section {slug}")).into_response(),
        Err(e) => err(e),
    }
}

async fn section_create(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Json(draft): Json<ManualSectionDraft>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.repo.create_section(draft, &user.id).await {
        Ok(s) => (StatusCode::CREATED, Json(s)).into_response(),
        Err(e) => err(e),
    }
}

async fn section_update(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(patch): Json<ManualPatch>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.repo.update_section(&slug, patch, &user.id).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => err(e),
    }
}

async fn section_history(
    State(state): State<Arc<ContentApiState>>,
    Path(slug): Path<String>,
) -> Response {
    match state.repo.section_history(&slug).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}
