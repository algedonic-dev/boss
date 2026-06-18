//! Axum HTTP handlers for the shipping API.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_core::publisher::DomainPublisher;
use boss_policy_client::CurrentUser;

use crate::port::{ShippingError, ShippingRepository};

use crate::summary::STATUS_SUMMARY_RECENT_LIMIT;
use crate::types::{Shipment, ShipmentDirection};
use chrono::NaiveDate;

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 1000;

#[derive(Deserialize)]
struct ListFilter {
    limit: Option<i64>,
    offset: Option<i64>,
    /// Optional account-scoped filter for the unified account detail view.
    account_id: Option<String>,
}

impl ListFilter {
    fn limit(&self) -> i64 {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }
    fn offset(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }
}

#[derive(Serialize)]
struct PaginatedResponse<T: Serialize> {
    data: Vec<T>,
    total: i64,
    limit: i64,
    offset: i64,
}

pub struct ShippingApiState<R: ShippingRepository> {
    pub shipping: Arc<R>,
    pub publisher: Option<DomainPublisher>,
    /// Optional Class registry for `Carrier` validation. When
    /// configured, every shipment create checks that the incoming
    /// carrier code exists under `(subject_kind='shipment')` in the
    /// Class registry. When `None`, the API is permissive (matches
    /// `boss-catalog::http::check_category` and the `Subject::Custom`
    /// validation in `boss-jobs`).
    pub classes_client: Option<Arc<dyn ClassesClient>>,
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router<R: ShippingRepository + 'static>(state: ShippingApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/shipping/health", get(health))
        .route(
            "/api/shipping/shipments",
            get(list_shipments::<R>).post(create_shipment::<R>),
        )
        .route("/api/shipping/shipments/batch", post(batch_shipments::<R>))
        .route(
            "/api/shipping/shipments/status-summary",
            get(status_summary::<R>),
        )
        .route(
            "/api/shipping/shipments/from-tracking-scan",
            post(record_tracking_scan::<R>),
        )
        .route(
            "/api/shipping/shipments/{id}",
            get(get_shipment::<R>)
                .put(update_shipment::<R>)
                .delete(delete_shipment::<R>),
        )
        .with_state(shared)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-shipping-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

#[derive(Deserialize)]
struct StatusSummaryQuery {
    /// "outbound" | "inbound". Required — callers always want one side.
    direction: String,
}

async fn status_summary<R: ShippingRepository + 'static>(
    State(state): State<Arc<ShippingApiState<R>>>,
    Query(q): Query<StatusSummaryQuery>,
) -> Response {
    let requested = match q.direction.as_str() {
        "outbound" => ShipmentDirection::Outbound,
        "inbound" => ShipmentDirection::Inbound,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                format!("direction must be 'outbound' or 'inbound', got '{other}'"),
            )
                .into_response();
        }
    };

    match state
        .shipping
        .status_summary(
            requested,
            boss_clock_client::now_from(&state.clock).await.date_naive(),
            STATUS_SUMMARY_RECENT_LIMIT as i64,
        )
        .await
    {
        Ok(summary) => Json(summary).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn list_shipments<R: ShippingRepository + 'static>(
    State(state): State<Arc<ShippingApiState<R>>>,
    Query(filter): Query<ListFilter>,
) -> Response {
    let limit = filter.limit();
    let offset = filter.offset();
    match state
        .shipping
        .list_shipments(limit, offset, filter.account_id.as_deref())
        .await
    {
        Ok((data, total)) => Json(PaginatedResponse {
            data,
            total,
            limit,
            offset,
        })
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_shipment<R: ShippingRepository + 'static>(
    State(state): State<Arc<ShippingApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.shipping.shipment_by_id(&id).await {
        Ok(Some(ship)) => Json(ship).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("no shipment with ID {id}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Validate an incoming `Carrier` against the Class registry.
///
/// When `classes_client` is `None`, the function is permissive — the
/// service is running without registry validation (a regen with
/// `BOSS_CLASSES_URL` unset accepts any carrier). When configured,
/// the carrier code must exist as an active Class under
/// `(subject_kind='shipment')`. `Carrier` is a free-text wrapper;
/// this gate is what makes a given carrier string actually mean
/// something.
///
/// Returns `Ok(())` on success, a 400 response on an unregistered
/// code, or 503 when the registry is unreachable (fail-closed: an
/// unreachable registry shouldn't accept arbitrary carrier strings).
async fn check_carrier(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    carrier: &str,
) -> Result<(), Response> {
    let Some(client) = classes_client else {
        return Ok(());
    };
    let class_ref = ClassRef::new("shipment", carrier);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown carrier `{carrier}` — register it as a Class first \
                 (subject_kind='shipment')"
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("classes registry unreachable: {e}"),
        )
            .into_response()),
    }
}

/// Validate an incoming `ShipmentStatus` against the Class registry.
///
/// `ShipmentStatus` is a free-text wrapper (the closed enum was lifted
/// to a String-newtype in v1.1.0), so the registry is what makes a
/// status string mean something. Unlike `carrier`, status is a
/// non-optional field, so this gate fires on every create/update —
/// there is no skip-when-absent path. Same contract as `check_carrier`:
/// permissive when no registry is wired (test path), fail-closed 503
/// when unreachable, 400 on an unregistered code. The code keys on
/// `(subject_kind='shipment', code)`; `member_attribute='status'` is
/// metadata narration, not part of the `ClassRef` key.
async fn check_status(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    status: &str,
) -> Result<(), Response> {
    let Some(client) = classes_client else {
        return Ok(());
    };
    let class_ref = ClassRef::new("shipment", status);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown shipment status `{status}` — register it as a Class \
                 first (subject_kind='shipment')"
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("classes registry unreachable: {e}"),
        )
            .into_response()),
    }
}

async fn create_shipment<R: ShippingRepository + 'static>(
    State(state): State<Arc<ShippingApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(shipment): Json<Shipment>,
) -> Response {
    // Status is mandatory, so it's always validated. Optional-skip:
    // only a *present* carrier is validated. An identity-first shipment
    // created without a carrier (None) passes straight through; it can
    // be enriched later.
    if let Err(resp) = check_status(state.classes_client.as_ref(), shipment.status.as_str()).await {
        return resp;
    }
    if let Some(carrier) = &shipment.carrier
        && let Err(resp) = check_carrier(state.classes_client.as_ref(), carrier.as_str()).await
    {
        return resp;
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.shipping.create_shipment_at(&shipment, now).await {
        Ok(id) => {
            if let Some(pub_) = &state.publisher {
                // State event — full Shipment row state.
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::SHIPMENT_CREATED,
                    actor,
                    serde_json::to_value(&shipment).unwrap_or_default(),
                    now,
                )
                .await;
            }
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Err(e) => shipping_error_response(e),
    }
}

async fn batch_shipments<R: ShippingRepository + 'static>(
    State(state): State<Arc<ShippingApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(shipments): Json<Vec<Shipment>>,
) -> Response {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let mut inserted = 0u64;
    let batch_now = boss_clock_client::now_from(&state.clock).await;
    for s in &shipments {
        let now = batch_now;
        // Status + carrier registry gates — skip rows with an
        // unregistered (or unreachable-registry) code, matching this
        // endpoint's existing skip-on-failure batch semantics.
        // Permissive when no registry is wired. Status is mandatory so
        // it's always checked; a row with no carrier (None) is
        // identity-first valid and is not skipped on carrier.
        if check_status(state.classes_client.as_ref(), s.status.as_str())
            .await
            .is_err()
        {
            continue;
        }
        if let Some(carrier) = &s.carrier
            && check_carrier(state.classes_client.as_ref(), carrier.as_str())
                .await
                .is_err()
        {
            continue;
        }
        if state.shipping.create_shipment_at(s, now).await.is_ok() {
            inserted += 1;
            if let Some(pub_) = &state.publisher {
                pub_.emit_with_actor_at(
                    crate::events::SHIPMENT_CREATED,
                    actor.clone(),
                    serde_json::to_value(s).unwrap_or_default(),
                    now,
                )
                .await;
            }
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({"inserted": inserted})),
    )
        .into_response()
}

async fn update_shipment<R: ShippingRepository + 'static>(
    State(state): State<Arc<ShippingApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(shipment): Json<Shipment>,
) -> Response {
    // Status is mandatory and validated on every update; a present
    // carrier is validated, an absent one (None) passes through.
    if let Err(resp) = check_status(state.classes_client.as_ref(), shipment.status.as_str()).await {
        return resp;
    }
    if let Some(carrier) = &shipment.carrier
        && let Err(resp) = check_carrier(state.classes_client.as_ref(), carrier.as_str()).await
    {
        return resp;
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.shipping.update_shipment_at(&id, &shipment, now).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::SHIPMENT_UPDATED,
                    actor,
                    serde_json::to_value(&shipment).unwrap_or_default(),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => shipping_error_response(e),
    }
}

async fn delete_shipment<R: ShippingRepository + 'static>(
    State(state): State<Arc<ShippingApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Response {
    match state.shipping.delete_shipment(&id).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::SHIPMENT_DELETED,
                    actor,
                    serde_json::json!({ "id": id, "deleted_at": now }),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => shipping_error_response(e),
    }
}

/// Adapter shape for the keg-courier counterparty scans. The
/// CounterpartyEngine wraps the prior trigger payload at each
/// hop, so by stage N the original `step.done.shipment` payload
/// (carrying `step_id`) lives at depth N+1. This handler walks
/// `trigger.trigger.…` recursively to find `step_id`, derives
/// `shipment_id = ship-step-{step_id}` (matching what
/// boss-shipping-sim-bridge mints on shipment creation),
/// records a `shipment_tracking_events` row, and rolls up the
/// shipment's `status` column when the scan moves it to a
/// row-state-changing value (in-transit, delivered).
/// Idempotent on (shipment_id, status, occurred_on).
#[derive(serde::Deserialize)]
struct TrackingScanBody {
    /// The chained counterparty trigger. Walked recursively to
    /// find `step_id`.
    trigger: serde_json::Value,
    /// The scan's status — set by the counterparty engine's
    /// `stage_payload` helper from the spec's `status` field.
    status: String,
    /// Optional stage_index for downstream UI ("stage 2 of 3").
    #[serde(default)]
    stage_index: Option<i16>,
    /// Counterparty drain date. Used as `occurred_on`.
    #[serde(default, rename = "_day")]
    day: Option<NaiveDate>,
}

fn find_step_id(payload: &serde_json::Value) -> Option<&str> {
    if let Some(step_id) = payload.get("step_id").and_then(|v| v.as_str()) {
        return Some(step_id);
    }
    payload.get("trigger").and_then(find_step_id)
}

async fn record_tracking_scan<R: ShippingRepository + 'static>(
    State(state): State<Arc<ShippingApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<TrackingScanBody>,
) -> Response {
    let Some(step_id) = find_step_id(&body.trigger) else {
        return (
            StatusCode::BAD_REQUEST,
            "no step_id found in trigger chain".to_string(),
        )
            .into_response();
    };
    let shipment_id = format!("ship-step-{step_id}");
    // The scan status rolls up into the shipment's `status` column, so
    // it validates against the same `(subject_kind='shipment')` Class
    // set. This is a sim-facing counterparty endpoint with non-fatal
    // skip semantics (out-of-order scans return 200/skipped to keep the
    // error budget clean), so an unregistered status is skipped the
    // same way rather than hard-400'd.
    if check_status(state.classes_client.as_ref(), &body.status)
        .await
        .is_err()
    {
        return Json(serde_json::json!({
            "ok": false,
            "skipped": true,
            "shipment_id": shipment_id,
            "reason": "unregistered shipment status",
        }))
        .into_response();
    }
    let occurred_on = body
        .day
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());

    match state
        .shipping
        .record_tracking_scan(&shipment_id, &body.status, occurred_on, body.stage_index)
        .await
    {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::TRACKING_RECORDED,
                    actor,
                    serde_json::json!({
                        "shipment_id": shipment_id,
                        "status": body.status,
                        "occurred_on": occurred_on,
                        "stage_index": body.stage_index,
                    }),
                    now,
                )
                .await;
            }
            (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
        }
        // Out-of-order scan or shipment not yet projected — return
        // 200 with skipped so the sim's error budget stays clean.
        Err(ShippingError::NotFound(_)) => Json(serde_json::json!({
            "ok": false,
            "skipped": true,
            "shipment_id": shipment_id,
            "reason": "shipment not yet projected",
        }))
        .into_response(),
        Err(e) => shipping_error_response(e),
    }
}

fn shipping_error_response(e: ShippingError) -> Response {
    match e {
        ShippingError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        ShippingError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        ShippingError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::in_memory::InMemoryShipping;
    use crate::summary::summarise_shipments;
    use crate::types::*;

    fn test_shipment(id: &str) -> Shipment {
        Shipment {
            id: id.to_string(),
            direction: ShipmentDirection::Outbound,
            status: ShipmentStatus::IN_TRANSIT.into(),
            carrier: Some(Carrier::new("fedex")),
            tracking_number: Some("1Z999AA10123456784".to_string()),
            origin: "HQ Warehouse".to_string(),
            destination: "Account Alpha".to_string(),
            asset_ids: vec!["SN-001".to_string()],
            line_items: Vec::new(),
            po_id: None,
            order_id: Some("ORD-200".to_string()),
            account_id: Some("account-001".to_string()),
            created_on: chrono::NaiveDate::from_ymd_opt(2025, 6, 1).unwrap(),
            shipped_on: Some(chrono::NaiveDate::from_ymd_opt(2025, 6, 2).unwrap()),
            estimated_delivery: Some(chrono::NaiveDate::from_ymd_opt(2025, 6, 5).unwrap()),
            delivered_on: None,
        }
    }

    fn test_app() -> Router {
        let shipping = Arc::new(InMemoryShipping::new(vec![
            test_shipment("ship-001"),
            test_shipment("ship-002"),
        ]));
        router(ShippingApiState {
            shipping,
            publisher: None,
            classes_client: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    fn app_with_classes_client(classes: Arc<dyn ClassesClient>) -> Router {
        let shipping = Arc::new(InMemoryShipping::new(vec![]));
        router(ShippingApiState {
            shipping,
            publisher: None,
            classes_client: Some(classes),
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    #[tokio::test]
    async fn health_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/shipping/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_shipments_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/shipping/shipments")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(envelope["total"], 2);
        assert_eq!(envelope["data"].as_array().unwrap().len(), 2);
        assert_eq!(envelope["limit"], 100);
        assert_eq!(envelope["offset"], 0);
    }

    #[tokio::test]
    async fn get_shipment_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/shipping/shipments/ship-001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn summary_counts_by_status_and_ignores_other_direction() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap();
        let out_it = test_shipment("S1");
        let mut out_excp = test_shipment("S2");
        out_excp.status = ShipmentStatus::EXCEPTION.into();
        let mut inbound = test_shipment("S3");
        inbound.direction = ShipmentDirection::Inbound;
        inbound.status = ShipmentStatus::IN_TRANSIT.into();
        let shipments = vec![out_it, out_excp, inbound];
        let summary = summarise_shipments(&shipments, ShipmentDirection::Outbound, today);
        assert_eq!(summary.in_transit, 1);
        assert_eq!(summary.exception, 1);
        // Inbound shipment must not leak into outbound aggregate.
        assert_eq!(summary.label_created, 0);
        assert_eq!(summary.recent.len(), 2);
    }

    #[test]
    fn summary_delivered_7d_window_filters_older_deliveries() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap();
        let mut recent_delivery = test_shipment("S-R");
        recent_delivery.status = ShipmentStatus::DELIVERED.into();
        recent_delivery.delivered_on = Some(today - chrono::Duration::days(3));
        let mut old_delivery = test_shipment("S-O");
        old_delivery.status = ShipmentStatus::DELIVERED.into();
        old_delivery.delivered_on = Some(today - chrono::Duration::days(30));
        let summary = summarise_shipments(
            &[recent_delivery, old_delivery],
            ShipmentDirection::Outbound,
            today,
        );
        assert_eq!(summary.delivered_7d, 1);
    }

    #[tokio::test]
    async fn get_shipment_not_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/shipping/shipments/ship-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    fn post_shipment(
        app: Router,
        shipment: &Shipment,
    ) -> impl std::future::Future<Output = Response> {
        let body = serde_json::to_vec(shipment).unwrap();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shipping/shipments")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    }

    #[tokio::test]
    async fn create_shipment_rejected_when_carrier_unknown() {
        use boss_classes_client::FakeClassesClient;
        // Registry knows the fixture status (`in-transit`) so the
        // status gate passes, plus carrier `ups`; the fixture uses
        // carrier `fedex` → 400 with the actionable error message.
        let classes = Arc::new(FakeClassesClient::with(vec![
            ClassRef::new("shipment", "ups"),
            ClassRef::new("shipment", ShipmentStatus::IN_TRANSIT),
        ])) as Arc<dyn ClassesClient>;
        let app = app_with_classes_client(classes);
        let resp = post_shipment(app, &test_shipment("ship-new")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(
            body.contains("fedex") && body.contains("subject_kind='shipment'"),
            "error message must name both the rejected code and the registry shape, got: {body}"
        );
    }

    #[tokio::test]
    async fn create_shipment_accepted_when_carrier_registered() {
        use boss_classes_client::FakeClassesClient;
        let classes = Arc::new(FakeClassesClient::permissive()) as Arc<dyn ClassesClient>;
        let app = app_with_classes_client(classes);
        let resp = post_shipment(app, &test_shipment("ship-new")).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_shipment_skips_validation_when_classes_client_unset() {
        // No Class registry configured → permissive. Even an
        // obviously-junk carrier lands.
        let mut shipment = test_shipment("ship-new");
        shipment.carrier = Some(Carrier::new("definitely-not-a-real-carrier"));
        let resp = post_shipment(test_app(), &shipment).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_shipment_skips_gate_when_carrier_absent() {
        use boss_classes_client::FakeClassesClient;
        // Identity-first: a shipment created with no carrier (None)
        // must pass the carrier gate untouched even with a strict
        // registry wired — the gate validates only a *present* carrier.
        // The fixture status (`in-transit`) is registered so the
        // mandatory status gate passes.
        let classes = Arc::new(FakeClassesClient::with(vec![
            ClassRef::new("shipment", "ups"),
            ClassRef::new("shipment", ShipmentStatus::IN_TRANSIT),
        ])) as Arc<dyn ClassesClient>;
        let app = app_with_classes_client(classes);
        let mut shipment = test_shipment("ship-no-carrier");
        shipment.carrier = None;
        let resp = post_shipment(app, &shipment).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_shipment_rejected_when_status_unknown() {
        use boss_classes_client::FakeClassesClient;
        // Registry knows the carrier but not the status code → 400.
        // Status is mandatory, so the gate fires unconditionally.
        let classes = Arc::new(FakeClassesClient::with(vec![ClassRef::new(
            "shipment", "fedex",
        )])) as Arc<dyn ClassesClient>;
        let app = app_with_classes_client(classes);
        let mut shipment = test_shipment("ship-bad-status");
        shipment.status = ShipmentStatus::new("teleported");
        let resp = post_shipment(app, &shipment).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(
            body.contains("teleported") && body.contains("subject_kind='shipment'"),
            "error must name the rejected status and the registry shape, got: {body}"
        );
    }
}
