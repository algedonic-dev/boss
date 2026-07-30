//! Vendor CRUD handlers.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_policy_client::CurrentUser;

use super::InventoryApiState;
use crate::port::{InventoryError, InventoryRepository};
use crate::types::{Vendor, VendorBehavior};

/// Validate a vendor's `payment_terms` against the Class registry under
/// `(subject_kind='vendor')`. Terms are a curated vocabulary (net-30,
/// prepaid, …) the tenant extends by adding a Class row, not by editing
/// a DB CHECK — the CHECK this replaces forced a core fork to add a
/// term. Same contract as [`super::vendor_invoices`]' status gate:
/// permissive when no registry is wired, 503 when unreachable, 400 on an
/// unregistered code. The caller skips the optional field — an absent
/// `payment_terms` (nullable, enriched later) never reaches here.
/// Vendor `category` and `payment_terms` share the `vendor` code
/// namespace; their `member_attribute` keeps them distinct.
async fn check_payment_terms(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    terms: &str,
) -> Result<(), Response> {
    let Some(client) = classes_client else {
        return Ok(());
    };
    let class_ref = ClassRef::new("vendor", terms);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown payment terms `{terms}` — register it as a Class first \
                 (subject_kind='vendor')"
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
    /// How the system expects this vendor to behave (supply lead time,
    /// fulfilment, AP timing) — the simulator stamps this from the
    /// category Class template at birth.
    #[serde(default)]
    behavior: Option<VendorBehavior>,
}

fn default_vendor_lead_time_days() -> u16 {
    7
}

pub(super) async fn create_vendor<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateVendorRequest>,
) -> Response {
    // Optional-skip: only a *present* payment_terms is validated; an
    // absent one (nullable, enriched later) passes straight through.
    if let Some(terms) = body.payment_terms.as_deref()
        && let Err(resp) = check_payment_terms(state.classes_client.as_ref(), terms).await
    {
        return resp;
    }

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
        behavior: body.behavior,
    };

    let now = boss_clock_client::now_from(&state.clock).await;
    // Outbox phase 2: `inventory.vendor.created` records inside the
    // repository transaction via the stamp; no post-commit emit.
    let stamp = super::event_stamp(&state, &user, now).await;
    match state.inventory.create_vendor_at(&vendor, now, &stamp).await {
        Ok(created_id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"ok": true, "id": created_id})),
        )
            .into_response(),
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
    // Optional-skip: only a *present* payment_terms is validated; an
    // absent one (nullable, enriched later) passes straight through.
    if let Some(terms) = body.payment_terms.as_deref()
        && let Err(resp) = check_payment_terms(state.classes_client.as_ref(), terms).await
    {
        return resp;
    }

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
        behavior: body.behavior,
    };

    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = super::event_stamp(&state, &user, now).await;
    match state.inventory.update_vendor(&id, &vendor, &stamp).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(InventoryError::NotFound(msg)) => (StatusCode::NOT_FOUND, msg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn delete_vendor<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = super::event_stamp(&state, &user, now).await;
    match state.inventory.delete_vendor(&id, &stamp).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(InventoryError::NotFound(msg)) => (StatusCode::NOT_FOUND, msg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
