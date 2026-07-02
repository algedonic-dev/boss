//! HTTP surface for the Vendor CRM. Mounted under `/api/inventory/vendors/:id/…`
//! so the vendor-scoped paths live alongside the existing vendor routes.
//!
//! Read endpoints return lists or single objects; write endpoints are
//! idempotent on `id` so sim scenarios and replays converge.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use boss_clock_client::ClockClient;
use boss_core::publisher::DomainPublisher;
use serde::Deserialize;
use sqlx::PgPool;

use super::postgres::PgProcurement;
use super::types::{
    NewVendorAccountTeamMember, NewVendorContact, NewVendorContract, NewVendorInteraction,
};
use crate::events::{
    VENDOR_CONTACT_DELETED, VENDOR_CONTACT_UPSERTED, VENDOR_CONTRACT_UPSERTED,
    VENDOR_INTERACTION_DELETED, VENDOR_INTERACTION_RECORDED, VENDOR_TEAM_ASSIGNED,
    VENDOR_TEAM_UNASSIGNED,
};
use crate::port::InventoryError;

#[derive(Clone)]
pub struct ProcurementApiState {
    pub pool: PgPool,
    /// Audit-log + NATS publisher. `None` allowed for tests that
    /// only exercise the projection write.
    pub publisher: Option<DomainPublisher>,
    /// Authoritative clock — every emit stamps from here so sim
    /// mode produces sim-dated audit_log rows. The trait is
    /// dyn-safe; brewery wires `ReqwestClockClient` against
    /// clock-api.
    pub clock: Arc<dyn ClockClient>,
}

pub fn router(state: ProcurementApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route(
            "/api/inventory/vendors/{id}/contacts",
            get(list_contacts).post(upsert_contact),
        )
        .route(
            "/api/inventory/vendors/{vendor_id}/contacts/{id}",
            delete(delete_contact),
        )
        .route(
            "/api/inventory/vendors/{id}/interactions",
            get(list_interactions).post(insert_interaction),
        )
        .route(
            "/api/inventory/vendors/{vendor_id}/interactions/{id}",
            delete(soft_delete_interaction),
        )
        .route(
            "/api/inventory/vendors/{id}/account-team",
            get(list_account_team).post(upsert_account_team_member),
        )
        .route(
            "/api/inventory/vendors/{vendor_id}/account-team/{role}",
            delete(remove_account_team_member),
        )
        .route(
            "/api/inventory/vendors/{id}/contracts",
            get(list_contracts).post(upsert_contract),
        )
        .with_state(shared)
}

// --- contacts -----------------------------------------------------------

async fn list_contacts(
    State(state): State<Arc<ProcurementApiState>>,
    Path(vendor_id): Path<String>,
) -> Response {
    match PgProcurement::new(state.pool.clone())
        .list_contacts(&vendor_id)
        .await
    {
        Ok(list) => Json(list).into_response(),
        Err(e) => err(e),
    }
}

async fn upsert_contact(
    State(state): State<Arc<ProcurementApiState>>,
    Path(vendor_id): Path<String>,
    Json(mut body): Json<NewVendorContact>,
) -> Response {
    if body.vendor_id.is_empty() {
        body.vendor_id = vendor_id.clone();
    } else if body.vendor_id != vendor_id {
        return (
            StatusCode::BAD_REQUEST,
            "vendor_id in path does not match body",
        )
            .into_response();
    }
    match PgProcurement::new(state.pool.clone())
        .upsert_contact(body)
        .await
    {
        Ok(c) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    VENDOR_CONTACT_UPSERTED,
                    serde_json::to_value(&c).unwrap_or_default(),
                    boss_clock_client::now_from(&state.clock).await,
                )
                .await;
            }
            Json(c).into_response()
        }
        Err(e) => err(e),
    }
}

async fn delete_contact(
    State(state): State<Arc<ProcurementApiState>>,
    Path((vendor_id, id)): Path<(String, String)>,
) -> Response {
    match PgProcurement::new(state.pool.clone())
        .delete_contact(&id)
        .await
    {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    VENDOR_CONTACT_DELETED,
                    serde_json::json!({
                        "id": id,
                        "vendor_id": vendor_id,
                        "deleted_at": now,
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

// --- interactions -------------------------------------------------------

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    100
}

async fn list_interactions(
    State(state): State<Arc<ProcurementApiState>>,
    Path(vendor_id): Path<String>,
    Query(q): Query<ListQuery>,
) -> Response {
    match PgProcurement::new(state.pool.clone())
        .list_interactions(&vendor_id, q.limit)
        .await
    {
        Ok(list) => Json(list).into_response(),
        Err(e) => err(e),
    }
}

async fn insert_interaction(
    State(state): State<Arc<ProcurementApiState>>,
    Path(vendor_id): Path<String>,
    Json(mut body): Json<NewVendorInteraction>,
) -> Response {
    if body.vendor_id.is_empty() {
        body.vendor_id = vendor_id.clone();
    } else if body.vendor_id != vendor_id {
        return (
            StatusCode::BAD_REQUEST,
            "vendor_id in path does not match body",
        )
            .into_response();
    }
    match PgProcurement::new(state.pool.clone())
        .insert_interaction(body)
        .await
    {
        Ok(i) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    VENDOR_INTERACTION_RECORDED,
                    serde_json::to_value(&i).unwrap_or_default(),
                    boss_clock_client::now_from(&state.clock).await,
                )
                .await;
            }
            Json(i).into_response()
        }
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct SoftDeleteBody {
    deleted_by: String,
}

async fn soft_delete_interaction(
    State(state): State<Arc<ProcurementApiState>>,
    Path((_vendor_id, id)): Path<(String, String)>,
    Json(body): Json<SoftDeleteBody>,
) -> Response {
    match PgProcurement::new(state.pool.clone())
        .soft_delete_interaction(&id, &body.deleted_by)
        .await
    {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    VENDOR_INTERACTION_DELETED,
                    serde_json::json!({
                        "id": id,
                        "deleted_by": body.deleted_by,
                        "deleted_at": now,
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

// --- account team -------------------------------------------------------

async fn list_account_team(
    State(state): State<Arc<ProcurementApiState>>,
    Path(vendor_id): Path<String>,
) -> Response {
    match PgProcurement::new(state.pool.clone())
        .list_account_team(&vendor_id)
        .await
    {
        Ok(list) => Json(list).into_response(),
        Err(e) => err(e),
    }
}

async fn upsert_account_team_member(
    State(state): State<Arc<ProcurementApiState>>,
    Path(vendor_id): Path<String>,
    Json(mut body): Json<NewVendorAccountTeamMember>,
) -> Response {
    if body.vendor_id.is_empty() {
        body.vendor_id = vendor_id.clone();
    } else if body.vendor_id != vendor_id {
        return (
            StatusCode::BAD_REQUEST,
            "vendor_id in path does not match body",
        )
            .into_response();
    }
    match PgProcurement::new(state.pool.clone())
        .upsert_account_team_member(body)
        .await
    {
        Ok(m) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    VENDOR_TEAM_ASSIGNED,
                    serde_json::to_value(&m).unwrap_or_default(),
                    boss_clock_client::now_from(&state.clock).await,
                )
                .await;
            }
            Json(m).into_response()
        }
        Err(e) => err(e),
    }
}

async fn remove_account_team_member(
    State(state): State<Arc<ProcurementApiState>>,
    Path((vendor_id, role)): Path<(String, String)>,
) -> Response {
    // Capture the prior assignment so the unassigned event carries
    // the employee_id at the time of removal (auditors should see
    // who was unassigned without walking the full history).
    let pg = PgProcurement::new(state.pool.clone());
    let prior_employee_id: Option<String> =
        pg.list_account_team(&vendor_id)
            .await
            .ok()
            .and_then(|members| {
                members
                    .into_iter()
                    .find(|m| m.role == role)
                    .map(|m| m.employee_id)
            });
    match pg.remove_account_team_member(&vendor_id, &role).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    VENDOR_TEAM_UNASSIGNED,
                    serde_json::json!({
                        "vendor_id": vendor_id,
                        "role": role,
                        "employee_id": prior_employee_id,
                        "unassigned_at": now,
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

// --- contracts ----------------------------------------------------------

#[derive(Deserialize)]
struct ContractsQuery {
    #[serde(default)]
    status: Option<String>,
}

async fn list_contracts(
    State(state): State<Arc<ProcurementApiState>>,
    Path(vendor_id): Path<String>,
    Query(q): Query<ContractsQuery>,
) -> Response {
    match PgProcurement::new(state.pool.clone())
        .list_contracts(&vendor_id, q.status.as_deref())
        .await
    {
        Ok(list) => Json(list).into_response(),
        Err(e) => err(e),
    }
}

async fn upsert_contract(
    State(state): State<Arc<ProcurementApiState>>,
    Path(vendor_id): Path<String>,
    Json(mut body): Json<NewVendorContract>,
) -> Response {
    if body.vendor_id.is_empty() {
        body.vendor_id = vendor_id.clone();
    } else if body.vendor_id != vendor_id {
        return (
            StatusCode::BAD_REQUEST,
            "vendor_id in path does not match body",
        )
            .into_response();
    }
    match PgProcurement::new(state.pool.clone())
        .upsert_contract(body)
        .await
    {
        Ok(c) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    VENDOR_CONTRACT_UPSERTED,
                    serde_json::to_value(&c).unwrap_or_default(),
                    boss_clock_client::now_from(&state.clock).await,
                )
                .await;
            }
            Json(c).into_response()
        }
        Err(e) => err(e),
    }
}

// --- error mapping ------------------------------------------------------

fn err(e: InventoryError) -> Response {
    match e {
        InventoryError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        InventoryError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        InventoryError::InsufficientStock(sku, on_hand, need) => (
            StatusCode::CONFLICT,
            format!("insufficient stock: {sku} has {on_hand}, need {need}"),
        )
            .into_response(),
        e @ InventoryError::InvalidAccount(_) => {
            (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response()
        }
        InventoryError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}
