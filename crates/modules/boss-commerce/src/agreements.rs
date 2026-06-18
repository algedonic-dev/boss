//! Service agreement endpoints — contract management for accounts.
//!
//! Audit-chain note: POST routes through `DomainPublisher` so every
//! agreement upsert lands in `audit_log`. `rebuild_commerce`
//! consumes `commerce.service_agreement.upserted` and reproduces
//! the projection from the log alone.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_core::publisher::DomainPublisher;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::events::SERVICE_AGREEMENT_UPSERTED;

fn default_currency() -> String {
    "USD".to_string()
}

#[derive(Clone)]
pub struct AgreementsState {
    pub pool: Arc<PgPool>,
    /// Audit-log + NATS publisher. `None` allowed for tests that
    /// only exercise projection writes.
    pub publisher: Option<DomainPublisher>,
    /// Authoritative clock — every emit stamps from here so sim
    /// mode produces sim-dated audit_log rows.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn agreements_router(
    pool: PgPool,
    publisher: Option<DomainPublisher>,
    clock: Arc<dyn boss_clock_client::ClockClient>,
) -> Router {
    let state = AgreementsState {
        pool: Arc::new(pool),
        publisher,
        clock,
    };
    Router::new()
        .route(
            "/api/commerce/agreements",
            get(list_agreements).post(create_agreement),
        )
        .with_state(state)
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ServiceAgreement {
    pub id: String,
    pub account_id: String,
    pub agreement_type: String,
    pub status: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub annual_value_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub billing_frequency: String,
    pub auto_renew: bool,
    pub covers_parts: bool,
    pub covers_labor: bool,
    pub covers_travel: bool,
    pub pm_visits_per_year: i16,
    pub response_sla_hours: i16,
    pub owner_id: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListAgreementsQuery {
    /// Filter to a single account when present. Callers that
    /// navigate to a AccountPage always supply this; omitting it
    /// returns agreements across all accounts (admin view).
    pub account_id: Option<String>,
    /// Filter by status (typically `"active"`). Omit for all.
    pub status: Option<String>,
    /// Cap the returned set. Zero/unset defaults to `DEFAULT_LIMIT`;
    /// anything larger clamps to `MAX_LIMIT`.
    pub limit: Option<i64>,
}

const DEFAULT_LIMIT: i64 = 200;
const MAX_LIMIT: i64 = 5_000;

async fn list_agreements(
    State(state): State<AgreementsState>,
    Query(params): Query<ListAgreementsQuery>,
) -> Response {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let rows: Result<Vec<ServiceAgreement>, _> = sqlx::query_as(
        "SELECT id, account_id, type AS agreement_type, status, start_date, end_date, \
         annual_value_cents, currency, billing_frequency, auto_renew, covers_parts, covers_labor, \
         covers_travel, pm_visits_per_year, response_sla_hours, owner_id \
         FROM service_agreements \
         WHERE ($1::text IS NULL OR account_id = $1) \
           AND ($2::text IS NULL OR status = $2) \
         ORDER BY start_date DESC \
         LIMIT $3",
    )
    .bind(params.account_id.as_deref())
    .bind(params.status.as_deref())
    .bind(limit)
    .fetch_all(state.pool.as_ref())
    .await;

    match rows {
        Ok(data) => Json(data).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn create_agreement(
    State(state): State<AgreementsState>,
    Json(req): Json<ServiceAgreement>,
) -> Response {
    if let Err(e) = upsert_agreement(state.pool.as_ref(), &req).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    if let Some(pub_) = &state.publisher {
        pub_.emit_at(
            SERVICE_AGREEMENT_UPSERTED,
            serde_json::to_value(&req).unwrap_or_default(),
            state.clock.now().await.now,
        )
        .await;
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"ok": true, "id": req.id})),
    )
        .into_response()
}

/// Single canonical UPSERT for `service_agreements`. Used by both
/// the handler and the rebuilder, so re-emission of the same id
/// transitions status / end_date / annual_value / currency exactly
/// the way live writes do.
pub(crate) async fn upsert_agreement<'e, E>(executor: E, req: &ServiceAgreement) -> sqlx::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO service_agreements (id, account_id, type, status, start_date, end_date, \
         annual_value_cents, currency, billing_frequency, auto_renew, covers_parts, covers_labor, \
         covers_travel, pm_visits_per_year, response_sla_hours, owner_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
         ON CONFLICT (id) DO UPDATE SET \
         status = EXCLUDED.status, \
         end_date = EXCLUDED.end_date, \
         annual_value_cents = EXCLUDED.annual_value_cents, \
         currency = EXCLUDED.currency",
    )
    .bind(&req.id)
    .bind(&req.account_id)
    .bind(&req.agreement_type)
    .bind(&req.status)
    .bind(req.start_date)
    .bind(req.end_date)
    .bind(req.annual_value_cents)
    .bind(&req.currency)
    .bind(&req.billing_frequency)
    .bind(req.auto_renew)
    .bind(req.covers_parts)
    .bind(req.covers_labor)
    .bind(req.covers_travel)
    .bind(req.pm_visits_per_year)
    .bind(req.response_sla_hours)
    .bind(&req.owner_id)
    .execute(executor)
    .await
    .map(|_| ())
}
