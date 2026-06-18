//! HTTP API for the Locations registry. Read-only in v1;
//! authoring lands when the admin UI does.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use crate::port::LocationRepository;

#[derive(Clone)]
pub struct LocationsApiState {
    pub locations: Arc<dyn LocationRepository>,
}

pub fn router(state: LocationsApiState) -> Router {
    Router::new()
        .route("/api/locations/health", get(health))
        .route("/api/locations", get(list_locations))
        .route("/api/locations/{id}", get(get_location))
        .route("/api/locations/{id}/exists", get(location_exists))
        .route("/api/locations/{id}/children", get(children_of))
        .with_state(state)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

/// Standard health probe — see boss-classes/src/http.rs for context.
async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "service": "boss-locations-api",
        "storage": STORAGE,
    }))
}

#[derive(Deserialize)]
struct ListQuery {
    /// Filter by Class registry kind code. If absent, returns
    /// roots (Locations with no parent).
    kind: Option<String>,
    /// Filter to direct children of this Location id. If absent
    /// and `kind` is absent, returns roots.
    parent_id: Option<String>,
}

async fn list_locations(
    State(state): State<LocationsApiState>,
    Query(q): Query<ListQuery>,
) -> Response {
    let result = match (q.kind.as_deref(), q.parent_id.as_deref()) {
        (Some(k), None) => state.locations.list_for_kind(k).await,
        (None, parent) => state.locations.children_of(parent).await,
        (Some(_), Some(_)) => {
            return (
                StatusCode::BAD_REQUEST,
                "use either `kind` or `parent_id`, not both",
            )
                .into_response();
        }
    };
    match result {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_location(State(state): State<LocationsApiState>, Path(id): Path<String>) -> Response {
    match state.locations.get(&id).await {
        Ok(Some(l)) => Json(l).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such location").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn location_exists(
    State(state): State<LocationsApiState>,
    Path(id): Path<String>,
) -> Response {
    match state.locations.exists_active(&id).await {
        Ok(b) => Json(serde_json::json!({ "exists": b })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn children_of(State(state): State<LocationsApiState>, Path(id): Path<String>) -> Response {
    match state.locations.children_of(Some(&id)).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemoryLocations;
    use axum::body::to_bytes;
    use axum::http::Request;
    use boss_core::primitives::Location;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    fn loc(id: &str, name: &str, kind: &str, parent_id: Option<&str>) -> Location {
        Location {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            parent_id: parent_id.map(String::from),
            timezone: "America/Los_Angeles".into(),
            latitude: None,
            longitude: None,
            address: None,
            account_id: None,
            metadata: json!({}),
            retired_at: None,
        }
    }

    fn build_app(rows: Vec<Location>) -> Router {
        let state = LocationsApiState {
            locations: Arc::new(InMemoryLocations::new(rows)),
        };
        router(state)
    }

    #[tokio::test]
    async fn list_by_kind_returns_filtered_rows() {
        let app = build_app(vec![
            loc("loc-hq", "HQ", "hq", None),
            loc("loc-mission", "Mission", "storefront", None),
            loc("loc-bay", "Bay", "storefront", None),
        ]);
        let req = Request::builder()
            .uri("/api/locations?kind=storefront")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_with_no_args_returns_roots() {
        let app = build_app(vec![
            loc("loc-hq", "HQ", "hq", None),
            loc("loc-zone-a", "Zone A", "warehouse-zone", Some("loc-hq")),
        ]);
        let req = Request::builder()
            .uri("/api/locations")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1, "only loc-hq is a root");
        assert_eq!(arr[0]["id"], "loc-hq");
    }

    #[tokio::test]
    async fn list_rejects_kind_and_parent_id_together() {
        let app = build_app(vec![]);
        let req = Request::builder()
            .uri("/api/locations?kind=hq&parent_id=loc-hq")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_returns_404_for_missing_location() {
        let app = build_app(vec![loc("loc-hq", "HQ", "hq", None)]);
        let req = Request::builder()
            .uri("/api/locations/loc-bogus")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn exists_returns_boolean_envelope() {
        let app = build_app(vec![loc("loc-hq", "HQ", "hq", None)]);
        let req = Request::builder()
            .uri("/api/locations/loc-hq/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["exists"], json!(true));
    }

    #[tokio::test]
    async fn children_endpoint_returns_direct_kids() {
        let app = build_app(vec![
            loc("loc-hq", "HQ", "hq", None),
            loc("loc-zone-a", "Zone A", "warehouse-zone", Some("loc-hq")),
            loc("loc-zone-b", "Zone B", "warehouse-zone", Some("loc-hq")),
            loc("loc-bin-a01", "A01", "warehouse-zone", Some("loc-zone-a")),
        ]);
        let req = Request::builder()
            .uri("/api/locations/loc-hq/children")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        let ids: Vec<&str> = arr.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            vec!["loc-zone-a", "loc-zone-b"],
            "only direct children of loc-hq"
        );
    }
}
