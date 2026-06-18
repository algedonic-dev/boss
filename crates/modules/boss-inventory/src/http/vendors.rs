//! Vendor CRUD handlers.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use boss_policy_client::CurrentUser;

use super::InventoryApiState;
use crate::port::{InventoryError, InventoryRepository};
use crate::types::Vendor;

pub(super) async fn list_vendors<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
) -> Response {
    match state.inventory.all_vendors().await {
        Ok(vendors) => Json(vendors).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct CreateVendorRequest {
    #[serde(default)]
    id: Option<String>,
    // Identity-first: only `id` is required; descriptive fields are
    // enriched later. `lead_time_days` keeps a sane default (7d).
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    contact_name: Option<String>,
    #[serde(default)]
    contact_email: Option<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default = "default_vendor_lead_time_days")]
    lead_time_days: u16,
    #[serde(default)]
    payment_terms: Option<String>,
    #[serde(default)]
    category: Option<String>,
}

fn default_vendor_lead_time_days() -> u16 {
    7
}

pub(super) async fn create_vendor<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateVendorRequest>,
) -> Response {
    let id = body.id.unwrap_or_else(|| {
        format!(
            "VND-{}",
            uuid::Uuid::new_v4().as_simple().to_string()[..8].to_uppercase()
        )
    });

    let vendor = Vendor {
        id: id.clone(),
        name: body.name,
        contact_name: body.contact_name,
        contact_email: body.contact_email,
        city: body.city,
        state: body.state,
        lead_time_days: body.lead_time_days,
        payment_terms: body.payment_terms,
        category: body.category,
    };

    let now = boss_clock_client::now_from(&state.clock).await;
    match state.inventory.create_vendor_at(&vendor, now).await {
        Ok(created_id) => {
            if let Some(pub_) = &state.publisher {
                // State event — full Vendor row state.
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::VENDOR_CREATED,
                    actor,
                    serde_json::to_value(&vendor).unwrap_or_default(),
                    now,
                )
                .await;
            }
            (
                StatusCode::CREATED,
                Json(serde_json::json!({"ok": true, "id": created_id})),
            )
                .into_response()
        }
        Err(InventoryError::Conflict(msg)) => (StatusCode::CONFLICT, msg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn update_vendor<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateVendorRequest>,
) -> Response {
    let vendor = Vendor {
        id: id.clone(),
        name: body.name,
        contact_name: body.contact_name,
        contact_email: body.contact_email,
        city: body.city,
        state: body.state,
        lead_time_days: body.lead_time_days,
        payment_terms: body.payment_terms,
        category: body.category,
    };

    match state.inventory.update_vendor(&id, &vendor).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                // State event — full Vendor row state.
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::VENDOR_UPDATED,
                    actor,
                    serde_json::to_value(&vendor).unwrap_or_default(),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(InventoryError::NotFound(msg)) => (StatusCode::NOT_FOUND, msg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn delete_vendor<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Response {
    match state.inventory.delete_vendor(&id).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::VENDOR_DELETED,
                    actor,
                    serde_json::json!({ "id": id, "deleted_at": now }),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(InventoryError::NotFound(msg)) => (StatusCode::NOT_FOUND, msg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
