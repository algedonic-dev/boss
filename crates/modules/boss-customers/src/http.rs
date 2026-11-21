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
//!
//! The two reads ask policy (backlog cda177ef, 2026-11-19): `Read` on
//! `customer`, at scope `all` — see [`read_customers`].

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use boss_policy_client::{CurrentUser, Decision, PolicyClient, Scope, User, controls};
use serde::Deserialize;

use crate::port::{CustomersError, CustomersRepository};
use crate::types::{Customer, email_key, id_from_email};

#[derive(Clone)]
pub struct CustomersApiState<R: CustomersRepository> {
    pub customers: Arc<R>,
    /// Authoritative clock — every stamp routes through clock-api.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
    /// What every read asks (backlog cda177ef). Not optional: a read
    /// that runs without a policy client answers its caller unasked,
    /// which is the defect this field closes.
    pub policy: Arc<dyn PolicyClient>,
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

/// The shared health body, so the read names the build that answered
/// (backlog 56f88ada). `postgres`: the one binary serving this router
/// requires that feature and wires `PgCustomers`.
async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-customers-api",
        env!("CARGO_PKG_VERSION"),
        "postgres",
    ))
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
        (_, Some(email)) if !email_key(email).is_empty() => id_from_email(email),
        _ => {
            return (StatusCode::BAD_REQUEST, "id or email is required").into_response();
        }
    };
    // Record stamp + row created_at: wall time (sim time is retired
    // from the record — David, 2026-08-22, packet a7a4cae5). The
    // adapter builds the event from this same instant, so live row,
    // event stamp, and rebuild (which reads audit_log.timestamp)
    // agree.
    let now = boss_clock_client::wall_now();
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

/// Whether `user` may read the buyers, or the response that refuses
/// (backlog cda177ef). Both reads asked nobody, so a Basic guest — the
/// `visitor` role, granted nothing on `customer` — read every buyer's
/// name, email and phone, while the OSS README said it read only what
/// policy grants it.
///
/// Only a grant at scope `all` reads: a buyer row has no owner,
/// account team, team or department, so a narrower grant names none of
/// them, and is refused saying so rather than answered with an empty
/// list that reads as "no buyers". A policy service that cannot answer
/// refuses — a gate that cannot be asked is not a gate that passed.
async fn read_customers(policy: &dyn PolicyClient, user: &User) -> Result<(), Response> {
    match policy.ask(user, controls::READ_CUSTOMER).await {
        Ok(Decision::Allow { scope: Scope::All }) => Ok(()),
        Ok(Decision::Allow { scope }) => Err((
            StatusCode::FORBIDDEN,
            format!(
                "a customer row has no owner, account team, team or department, so only a grant \
                 of read on customer at scope all reads one; this caller holds it at scope {}",
                scope.to_db_string()
            ),
        )
            .into_response()),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err(e.into_response()),
    }
}

async fn get_customer<R: CustomersRepository + 'static>(
    State(state): State<Arc<CustomersApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    if let Err(refused) = read_customers(state.policy.as_ref(), &user).await {
        return refused;
    }
    match state.customers.get_customer(&id).await {
        Ok(Some(c)) => Json(c).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => error_response(e),
    }
}

async fn list_customers<R: CustomersRepository + 'static>(
    State(state): State<Arc<CustomersApiState<R>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if let Err(refused) = read_customers(state.policy.as_ref(), &user).await {
        return refused;
    }
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
