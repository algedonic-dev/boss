//! Purchase-order handlers.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use boss_policy_client::CurrentUser;

use super::InventoryApiState;
use crate::port::{InventoryError, InventoryRepository};
use crate::types::{PoStatus, PurchaseOrder, PurchaseOrderLine};

pub(super) async fn list_orders<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
) -> Response {
    match state.inventory.all_purchase_orders().await {
        Ok(orders) => Json(orders).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn get_order<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.inventory.purchase_order_by_id(&id).await {
        Ok(Some(order)) => Json(order).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("no purchase order with ID {id}"),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct CreateOrderRequest {
    // Identity-first: a PO can be created as a bare Draft (id only) and
    // its vendor/lines filled in before it's placed.
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    lines: Vec<CreateOrderLine>,
}

#[derive(Deserialize)]
pub(super) struct CreateOrderLine {
    part_sku: String,
    qty: u32,
    unit_cost_cents: i64,
    #[serde(default = "default_currency_http")]
    currency: String,
}

fn default_currency_http() -> String {
    "USD".to_string()
}

pub(super) async fn create_order<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateOrderRequest>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let po_id = format!(
        "PO-{}",
        uuid::Uuid::new_v4().as_simple().to_string()[..8].to_uppercase()
    );

    // Identity-first: this endpoint mints a Draft PO. A Draft isn't
    // placed, so it carries no placement dates yet — they're stamped
    // when the PO is placed (the auto-restock path posts a Submitted PO
    // with full data via /orders/batch). `validate_placement` is the
    // required-at-place gate; a Draft always passes it.
    let po = PurchaseOrder {
        id: po_id.clone(),
        vendor: body.vendor,
        status: PoStatus::Draft,
        placed_on: None,
        expected_on: None,
        received_on: None,
        lines: body
            .lines
            .into_iter()
            .map(|l| PurchaseOrderLine {
                part_sku: l.part_sku,
                qty: l.qty,
                unit_cost_cents: l.unit_cost_cents,
                currency: l.currency,
            })
            .collect(),
    };
    if let Err(reason) = po.validate_placement() {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }
    match state.inventory.create_purchase_order_at(&po, now).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                // State event — full PO row state (incl. lines).
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::PO_UPSERTED,
                    actor,
                    serde_json::to_value(&po).unwrap_or_default(),
                    now,
                )
                .await;
            }
            (
                StatusCode::CREATED,
                Json(serde_json::json!({"ok": true, "id": po_id})),
            )
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn batch_create_orders<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<Vec<PurchaseOrder>>,
) -> Response {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let mut inserted = 0usize;
    let batch_now = boss_clock_client::now_from(&state.clock).await;
    for po in &body {
        let now = batch_now;
        // Required-at-place: a placed PO (status past Draft — the
        // auto-restock posts Submitted POs here) must carry vendor,
        // lines, and placed_on. A bare Draft passes.
        if let Err(reason) = po.validate_placement() {
            return (StatusCode::BAD_REQUEST, reason).into_response();
        }
        if let Err(e) = state.inventory.create_purchase_order_at(po, now).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        if let Some(pub_) = &state.publisher {
            pub_.emit_with_actor_at(
                crate::events::PO_UPSERTED,
                actor.clone(),
                serde_json::to_value(po).unwrap_or_default(),
                now,
            )
            .await;
        }
        inserted += 1;
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "inserted": inserted})),
    )
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct UpdateStatusRequest {
    status: String,
}

pub(super) async fn update_order_status<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<UpdateStatusRequest>,
) -> Response {
    match state.inventory.update_po_status(&id, &body.status).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                // Read back the full PO so the state event carries
                // post-update row state for the rebuild.
                if let Ok(Some(po)) = state.inventory.purchase_order_by_id(&id).await {
                    pub_.emit_with_actor_at(
                        crate::events::PO_UPSERTED,
                        actor.clone(),
                        serde_json::to_value(&po).unwrap_or_default(),
                        now,
                    )
                    .await;
                }
                // Marker event for downstream consumers.
                pub_.emit_with_actor_at(
                    crate::events::PO_STATUS_CHANGED,
                    actor,
                    serde_json::json!({ "id": id, "new_status": body.status }),
                    now,
                )
                .await;
            }
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(InventoryError::NotFound(id)) => {
            (StatusCode::NOT_FOUND, format!("PO not found: {id}")).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
