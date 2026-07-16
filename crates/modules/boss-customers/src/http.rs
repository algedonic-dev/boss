//! HTTP surface for `boss-customers`.
//!
//! - `GET  /api/customers/health` — liveness
//! - `GET  /api/customers`        — list, newest first
//! - `GET  /api/customers/{id}`   — one customer
//! - `POST /api/customers`        — create (the birth path: domain
//!   row + identity row + outbox event in one tx). Without an `id`,
//!   the R3 mint derives `cust-<sha256(email)[..12]>` — so the /shop
//!   checkout can re-POST the same buyer forever and always land on
//!   the same row. Sim births pass explicit pool ids.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::port::{CustomersError, CustomersRepository};
use crate::types::{Customer, id_from_email};

#[derive(Clone)]
pub struct CustomersApiState<R: CustomersRepository> {
    pub customers: Arc<R>,
    /// Authoritative clock — every stamp routes through clock-api.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router<R: CustomersRepository + 'static>(state: CustomersApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/customers/health", get(health))
        .route(
            "/api/customers",
            get(list_customers::<R>).post(create_customer::<R>),
        )
        .route("/api/customers/{id}", get(get_customer::<R>))
        .with_state(shared)
}

async fn health() -> Response {
    Json(serde_json::json!({"status": "ok"})).into_response()
}

#[derive(Deserialize)]
struct CreateBody {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    #[serde(alias = "label")]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    phone: Option<String>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

async fn create_customer<R: CustomersRepository + 'static>(
    State(state): State<Arc<CustomersApiState<R>>>,
    Json(body): Json<CreateBody>,
) -> Response {
    // Identity: explicit id wins (sim pool births, operator
    // tooling); otherwise the R3 mint derives it from the email.
    let id = match (&body.id, &body.email) {
        (Some(id), _) if !id.trim().is_empty() => id.clone(),
        (_, Some(email)) if !email.trim().is_empty() => id_from_email(email),
        _ => {
            return (StatusCode::BAD_REQUEST, "id or email is required").into_response();
        }
    };
    let now = state.clock.now().await.now;
    let customer = Customer {
        name: body
            .name
            .or_else(|| body.email.clone())
            .unwrap_or_else(|| id.clone()),
        id,
        email: body.email,
        phone: body.phone,
        metadata: body.metadata.unwrap_or_else(|| serde_json::json!({})),
        created_at: None,
    };
    match state.customers.create_customer_at(&customer, now).await {
        // Idempotent re-POST reports 200 (nothing written, nothing
        // emitted); a fresh birth is 201. Either way the caller gets
        // the id — that's what /shop stamps into the order metadata.
        Ok(true) => (StatusCode::CREATED, Json(customer)).into_response(),
        Ok(false) => (StatusCode::OK, Json(customer)).into_response(),
        Err(e) => error_response(e),
    }
}

async fn get_customer<R: CustomersRepository + 'static>(
    State(state): State<Arc<CustomersApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.customers.get_customer(&id).await {
        Ok(Some(c)) => Json(c).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => error_response(e),
    }
}

async fn list_customers<R: CustomersRepository + 'static>(
    State(state): State<Arc<CustomersApiState<R>>>,
) -> Response {
    match state.customers.list_customers().await {
        Ok(all) => Json(all).into_response(),
        Err(e) => error_response(e),
    }
}

fn error_response(e: CustomersError) -> Response {
    match e {
        CustomersError::NotFound(m) => (StatusCode::NOT_FOUND, m).into_response(),
        CustomersError::Invalid(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        CustomersError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}
