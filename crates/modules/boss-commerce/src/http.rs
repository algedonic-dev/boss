//! Axum HTTP handlers for the commerce API.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_core::publisher::DomainPublisher;
use boss_people_client::PeopleClient;
use boss_policy::{Action, Decision, Resource};
use boss_policy_client::{CurrentUser, PolicyClient};

use crate::port::{CommerceError, CommerceRepository};

fn error_response(err: CommerceError) -> Response {
    match err {
        CommerceError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        CommerceError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        CommerceError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 1000;

#[derive(Deserialize)]
struct ListFilter {
    limit: Option<i64>,
    offset: Option<i64>,
    /// Account-scoped filter for the unified account detail view.
    /// Optional — when omitted, returns all rows.
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

pub struct CommerceApiState<R: CommerceRepository> {
    pub commerce: Arc<R>,
    pub publisher: Option<DomainPublisher>,
    /// Cross-service guard for validating account_id at write time.
    /// Wrapped in Arc<dyn> so the production binary plugs in
    /// `ReqwestPeopleClient` and tests can substitute a fake.
    pub people_client: Arc<dyn PeopleClient>,
    /// Row-level authorization. Null in tests that don't exercise
    /// the policy path — those handlers skip the gate and treat the
    /// request as allowed (preserves existing test surface until the
    /// broader rollout swaps every test's harness over).
    pub policy: Option<Arc<dyn PolicyClient>>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
    /// Class registry for `InvoiceStatus` validation. When configured,
    /// every invoice create checks the incoming status against the
    /// active Class set under `(subject_kind='invoice')`. When `None`,
    /// the API is permissive (test path) — matching the carrier gate
    /// in boss-shipping. The production binary always wires `Some`
    /// from the required `classes_api_url`.
    pub classes_client: Option<Arc<dyn ClassesClient>>,
}

pub fn router<R: CommerceRepository + 'static>(state: CommerceApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/commerce/health", get(health))
        .route("/api/commerce/revenue", get(list_revenue::<R>))
        .route("/api/commerce/summary", get(commerce_summary::<R>))
        .route("/api/commerce/invoices", get(list_invoices::<R>))
        .route("/api/commerce/invoices/{id}", get(get_invoice::<R>))
        .route("/api/commerce/invoices/create", post(create_invoice::<R>))
        .route("/api/commerce/invoices/batch", post(batch_invoices::<R>))
        .route(
            "/api/commerce/invoices/{id}/paid",
            put(mark_invoice_paid::<R>),
        )
        .route(
            "/api/commerce/invoices/{id}/past-due",
            put(mark_invoice_past_due::<R>),
        )
        .route(
            "/api/commerce/invoices/{id}/write-off",
            put(mark_invoice_written_off::<R>),
        )
        .with_state(shared)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-commerce-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

async fn list_revenue<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
) -> Response {
    match state.commerce.all_revenue().await {
        Ok(data) => Json(data).into_response(),
        Err(e) => error_response(e),
    }
}

async fn commerce_summary<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
) -> Response {
    // Source `today` from ClockClient so AR aging buckets +
    // TTM revenue window respect sim-time. Pre-Clock fix: both
    // queries used PostgreSQL's `CURRENT_DATE` (wallclock) and
    // every sim-time invoice landed in "90+" with TTM showing
    // $0 once the sim was more than a year behind wallclock.
    let today = state.clock.now().await.now.date_naive();
    match state.commerce.invoice_summary(today).await {
        Ok(summary) => Json(summary).into_response(),
        Err(e) => error_response(e),
    }
}

async fn list_invoices<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    Query(filter): Query<ListFilter>,
) -> Response {
    let limit = filter.limit();
    let offset = filter.offset();
    match state
        .commerce
        .list_invoices(limit, offset, filter.account_id.as_deref())
        .await
    {
        Ok((data, total)) => Json(PaginatedResponse {
            data,
            total,
            limit,
            offset,
        })
        .into_response(),
        Err(e) => error_response(e),
    }
}

async fn get_invoice<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.commerce.invoice_by_id(&id).await {
        Ok(Some(inv)) => Json(inv).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("no invoice with ID {id}")).into_response(),
        Err(e) => error_response(e),
    }
}

/// Validate an incoming `InvoiceStatus` against the Class registry.
///
/// `InvoiceStatus` is a free-text wrapper (the closed enum was lifted to
/// a String-newtype in v1.1.0), so the registry is what makes a status
/// string mean something. Status is a non-optional field, so the gate
/// fires on every create. Same contract as `check_carrier` in
/// boss-shipping: permissive when no registry is wired (test path),
/// fail-closed 503 when unreachable, 400 on an unregistered code. Keys
/// on `(subject_kind='invoice', code)`.
async fn check_status(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    status: &str,
) -> Result<(), Response> {
    let Some(client) = classes_client else {
        return Ok(());
    };
    let class_ref = ClassRef::new("invoice", status);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown invoice status `{status}` — register it as a Class \
                 first (subject_kind='invoice')"
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

async fn create_invoice<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(invoice): Json<crate::types::Invoice>,
) -> Response {
    // Class-registry gate: the invoice status must be a registered
    // Class under (subject_kind='invoice'). Permissive when no registry
    // is wired (test path). Runs before policy so a malformed status is
    // a clean 400 regardless of the caller's role.
    if let Err(resp) = check_status(state.classes_client.as_ref(), invoice.status.as_str()).await {
        return resp;
    }
    // Policy: creating an invoice requires an active Create rule on
    // Resource::invoice() for the caller's role. If the state doesn't
    // carry a policy client (test path), skip — the existing tests
    // cover the invariants without role gating.
    if let Some(ref policy) = state.policy {
        match policy
            .check(&user, Action::Create, Resource::invoice())
            .await
        {
            Ok(Decision::Allow { .. }) => {}
            Ok(Decision::Deny { reason }) => {
                return (StatusCode::FORBIDDEN, reason).into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("policy check failed: {e}"),
                )
                    .into_response();
            }
        }
    }
    let invoice_id = invoice.id.clone();
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.commerce.create_invoice_at(&invoice, now).await {
        Ok(enriched) => {
            if let Some(pub_) = &state.publisher {
                // Emit the ENRICHED invoice — line_items[].cost_basis_cents
                // is now populated from the FG drawdown. The audit_log
                // replay path depends on it to reconstruct COGS legs.
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::INVOICE_CREATED,
                    actor,
                    crate::events::invoice_created_payload(&enriched),
                    now,
                )
                .await;
            }
            (
                StatusCode::CREATED,
                Json(serde_json::json!({"ok": true, "id": invoice_id})),
            )
                .into_response()
        }
        Err(e) => error_response(e),
    }
}

async fn batch_invoices<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(invoices): Json<Vec<crate::types::Invoice>>,
) -> Response {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let mut inserted = 0u64;
    let mut skipped: Vec<(String, String)> = Vec::new();
    let batch_now = boss_clock_client::now_from(&state.clock).await;
    for inv in &invoices {
        let now = batch_now;
        // Status registry gate — reject rows with an unregistered (or
        // unreachable-registry) status, matching this endpoint's
        // skip-and-report batch semantics. Permissive when no registry
        // is wired.
        if let Err(resp) = check_status(state.classes_client.as_ref(), inv.status.as_str()).await {
            let _ = resp;
            tracing::warn!(
                invoice_id = %inv.id,
                status = %inv.status,
                "invoice status not registered as a Class; skipping invoice in batch"
            );
            skipped.push((
                inv.id.clone(),
                format!("unregistered status `{}`", inv.status),
            ));
            continue;
        }
        // Retry a transient Postgres deadlock (40P01): concurrent invoice
        // txs can briefly contend on finished-goods FOR UPDATE row locks
        // under burst load. Deterministic lock ordering in the adapter
        // (sort by SKU) prevents the common case; this rides out any
        // residual so the batch recovers in-request instead of skipping →
        // NAK → dead-letter (which then 404s the downstream collection).
        // create_invoice_at is idempotent (ON CONFLICT + already-issued
        // guard), so re-invoking is safe.
        let mut attempt = 0u32;
        let result = loop {
            match state.commerce.create_invoice_at(inv, now).await {
                Err(e) if attempt < 4 && e.to_string().contains("deadlock detected") => {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(15 * attempt as u64)).await;
                }
                other => break other,
            }
        };
        match result {
            Ok(enriched) => {
                inserted += 1;
                if let Some(pub_) = &state.publisher {
                    pub_.emit_with_actor_at(
                        crate::events::INVOICE_CREATED,
                        actor.clone(),
                        crate::events::invoice_created_payload(&enriched),
                        now,
                    )
                    .await;
                }
            }
            Err(e) => {
                // On a rejected row, log per-row + return the
                // rejected ids so the batch caller can act on the
                // loss rather than silently seeing a lower
                // `inserted` count than it sent.
                tracing::warn!(
                    invoice_id = %inv.id,
                    account_id = %inv.account_id,
                    error = %e,
                    "create_invoice_at failed; skipping invoice in batch"
                );
                skipped.push((inv.id.clone(), e.to_string()));
            }
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "inserted": inserted,
            "skipped": skipped.iter()
                .map(|(id, err)| serde_json::json!({"id": id, "error": err}))
                .collect::<Vec<_>>(),
        })),
    )
        .into_response()
}

#[derive(Debug, Default, serde::Deserialize)]
struct MarkPaidBody {
    /// Sim-day the invoice was paid on. Optional — when absent,
    /// the handler stamps wall-clock NOW() for backwards
    /// compatibility with manual operator clicks. Sim drivers
    /// supply the sim-day so projections age correctly instead
    /// of bunching on whichever wall-clock day the engine
    /// ticked.
    ///
    /// Aliased to `_day` so counterparty triggers in the
    /// shape-driven engine — which auto-inject `_day` into
    /// every event payload (see `engines/batch.rs::inject_day`)
    /// — flow through this path without a per-event payload
    /// rewrite. Sim's manual `end_of_day` flush sends `paid_on`
    /// directly.
    #[serde(alias = "_day")]
    paid_on: Option<chrono::NaiveDate>,
}

async fn mark_invoice_paid<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    body: Option<axum::Json<MarkPaidBody>>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let paid_on = body
        .and_then(|axum::Json(b)| b.paid_on)
        .unwrap_or_else(|| now.date_naive());
    match state.commerce.mark_invoice_paid_at(&id, paid_on).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                // Read back the post-update Invoice (header + lines)
                // so the event carries full row state.
                if let Ok(Some(inv)) = state.commerce.invoice_by_id(&id).await {
                    let actor = user.ambient_actor().unwrap_or_else(|| {
                        boss_core::actor::ActorId::Automation("platform".into())
                    });
                    pub_.emit_with_actor_at(
                        crate::events::INVOICE_PAID,
                        actor,
                        serde_json::to_value(&inv).unwrap_or_default(),
                        now,
                    )
                    .await;
                }
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error_response(e),
    }
}

async fn mark_invoice_past_due<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.commerce.mark_invoice_past_due(&id).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher
                && let Ok(Some(inv)) = state.commerce.invoice_by_id(&id).await
            {
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::INVOICE_PAST_DUE,
                    actor,
                    serde_json::to_value(&inv).unwrap_or_default(),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error_response(e),
    }
}

async fn mark_invoice_written_off<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.commerce.mark_invoice_written_off(&id).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher
                && let Ok(Some(inv)) = state.commerce.invoice_by_id(&id).await
            {
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::INVOICE_WRITTEN_OFF,
                    actor,
                    serde_json::to_value(&inv).unwrap_or_default(),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_people_client::PeopleClientError;
    use tower::ServiceExt;

    use crate::in_memory::InMemoryCommerce;
    use crate::types::*;

    /// Test stub: every account and employee "exists." Lets the
    /// inline tests in this module exercise the existing happy
    /// paths without spinning up an HTTP server.
    struct AlwaysExistsPeople;

    #[async_trait::async_trait]
    impl PeopleClient for AlwaysExistsPeople {
        async fn employee_exists(&self, _id: &str) -> Result<bool, PeopleClientError> {
            Ok(true)
        }
        async fn account_exists(&self, _id: &str) -> Result<bool, PeopleClientError> {
            Ok(true)
        }
    }

    fn test_invoice(id: &str) -> Invoice {
        Invoice {
            id: id.to_string(),
            account_id: "account-001".to_string(),
            issued_on: chrono::NaiveDate::from_ymd_opt(2025, 3, 15).unwrap(),
            due_on: chrono::NaiveDate::from_ymd_opt(2025, 4, 15).unwrap(),
            paid_on: None,
            status: InvoiceStatus::OUTSTANDING.into(),
            amount_cents: 1_200_000,
            currency: "USD".to_string(),
            tax_cents: 0,
            tax_jurisdiction: None,
            payment_method: None,
            line_items: vec![InvoiceLineItem {
                id: format!("{id}-l1"),
                invoice_id: id.to_string(),
                revenue_category: RevenueCategory::from("new-sales"),
                amount_cents: 1_200_000,
                currency: "USD".to_string(),
                description: "Test device sale".to_string(),
                ref_id: None,
                sku: None,
                qty: None,
                cost_basis_cents: None,
            }],
        }
    }

    #[test]
    fn invoice_created_payload_omits_tax_lines_when_untaxed() {
        let payload = crate::events::invoice_created_payload(&test_invoice("inv-notax"));
        assert!(
            payload.get("tax_lines").is_none(),
            "zero-tax invoice must not carry tax_lines in the audit payload"
        );
        // Header + line_items still present.
        assert_eq!(payload["id"], "inv-notax");
        assert!(payload["line_items"].is_array());
    }

    #[test]
    fn invoice_created_payload_injects_tax_lines_when_taxed() {
        // Mirror the live fact: a taxed invoice's audit event MUST carry
        // tax_lines so the rebuild reconstructs the 2300 credit.
        let mut inv = test_invoice("inv-taxed");
        inv.amount_cents = 1_300_000;
        inv.tax_cents = 100_000;
        inv.tax_jurisdiction = Some("US-CA".to_string());

        let payload = crate::events::invoice_created_payload(&inv);
        let tax_lines = payload
            .get("tax_lines")
            .and_then(|v| v.as_array())
            .expect("taxed invoice carries tax_lines");
        assert_eq!(tax_lines.len(), 1);
        assert_eq!(tax_lines[0]["account"], "2300");
        assert_eq!(tax_lines[0]["amount_cents"], 100_000);
        assert_eq!(tax_lines[0]["jurisdiction"], "US-CA");
    }

    fn test_app() -> Router {
        let commerce = Arc::new(InMemoryCommerce::new(vec![
            test_invoice("inv-001"),
            test_invoice("inv-002"),
        ]));
        let policy: Arc<dyn PolicyClient> = Arc::new(boss_policy_client::PermissivePolicyClient);
        router(CommerceApiState {
            commerce,
            publisher: None,
            people_client: Arc::new(AlwaysExistsPeople),
            policy: Some(policy),
            clock: Arc::new(boss_clock_client::WallClockClient),
            classes_client: None,
        })
    }

    #[tokio::test]
    async fn health_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/commerce/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_invoice_not_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/commerce/invoices/inv-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn mark_paid_body_accepts_paid_on_and_day_alias() {
        // Sim's end_of_day flush sends `paid_on` directly.
        let direct: MarkPaidBody = serde_json::from_value(serde_json::json!({
            "paid_on": "2026-04-15"
        }))
        .unwrap();
        assert_eq!(
            direct.paid_on,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 15).unwrap()),
        );

        // Counterparty triggers go through `engines/batch.rs::inject_day`,
        // which adds `_day` to every payload. Alias picks it up so the
        // PUT-to-route path stamps sim-day instead of falling through
        // to the wall-clock NOW() default.
        let aliased: MarkPaidBody = serde_json::from_value(serde_json::json!({
            "_day": "2026-04-16",
            "trigger": { "step_id": "step-123" }
        }))
        .unwrap();
        assert_eq!(
            aliased.paid_on,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 16).unwrap()),
        );

        // Empty body: SPA-driven operator click. Handler falls back to
        // wall-clock NOW() (verified at the call site, not here).
        let empty: MarkPaidBody = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(empty.paid_on.is_none());
    }
}
