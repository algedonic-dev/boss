//! Vendor-invoice three-way-match handlers + AP aging.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_policy_client::CurrentUser;

use super::InventoryApiState;
use crate::port::InventoryRepository;
use crate::types::VendorInvoice;

pub(super) async fn ap_aging<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
) -> Response {
    // `today` comes from ClockClient so AP-aging buckets respect sim-time.
    let today = state.clock.now().await.now.date_naive();
    match state.inventory.ap_aging(today).await {
        Ok(payload) => Json(payload).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct ListVendorInvoicesQuery {
    status: Option<String>,
    limit: Option<i64>,
}

pub(super) async fn list_vendor_invoices<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    axum::extract::Query(q): axum::extract::Query<ListVendorInvoicesQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    match state
        .inventory
        .all_vendor_invoices(q.status.as_deref(), limit)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Validate a vendor-invoice `discrepancy_kind` against the Class
/// registry under `(subject_kind='vendor-invoice')`. Same contract as
/// the catalog `check_category` gate: permissive when no registry is
/// wired, fail-closed (503) when it's unreachable, 400 on an
/// unregistered code. The caller is responsible for the optional-skip —
/// a `VendorInvoice` with no `discrepancy_kind` (a clean three-way
/// match) never reaches this function.
async fn check_discrepancy_kind(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    kind: &str,
) -> Result<(), Response> {
    let Some(client) = classes_client else {
        return Ok(());
    };
    let class_ref = ClassRef::new("vendor-invoice", kind);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown discrepancy kind `{kind}` — register it as a Class first \
                 (subject_kind='vendor-invoice')"
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

pub(super) async fn upsert_vendor_invoice<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(invoice): Json<VendorInvoice>,
) -> Response {
    // Optional-skip: only a *present* discrepancy_kind is validated.
    // A clean match (None) passes straight through.
    if let Some(kind) = &invoice.discrepancy_kind
        && let Err(resp) =
            check_discrepancy_kind(state.classes_client.as_ref(), kind.as_str()).await
    {
        return resp;
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    match state
        .inventory
        .upsert_vendor_invoice_at(&invoice, now)
        .await
    {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::VENDOR_INVOICE_UPSERTED,
                    actor.clone(),
                    serde_json::to_value(&invoice).unwrap_or_default(),
                    now,
                )
                .await;

                // Transition events drive the bill.* fact projections.
                // Emitted whenever approved_on/paid_on is present;
                // financial_facts natural-key idempotency absorbs the
                // duplicate-on-re-upsert case so the fact log stays
                // 1:1 with the underlying state transition.
                if let Some(approved_on) = invoice.approved_on {
                    pub_.emit_with_actor_at(
                        crate::events::VENDOR_INVOICE_APPROVED,
                        actor.clone(),
                        serde_json::json!({
                            "vendor_invoice_id": invoice.id,
                            "po_id": invoice.po_id,
                            "vendor": invoice.vendor,
                            "amount_cents": invoice.amount_cents,
                            "currency": invoice.currency,
                            "approved_on": approved_on,
                        }),
                        now,
                    )
                    .await;
                }
                if let Some(paid_on) = invoice.paid_on {
                    pub_.emit_with_actor_at(
                        crate::events::VENDOR_INVOICE_PAID,
                        actor.clone(),
                        serde_json::json!({
                            "vendor_invoice_id": invoice.id,
                            "po_id": invoice.po_id,
                            "vendor": invoice.vendor,
                            "amount_cents": invoice.amount_cents,
                            "currency": invoice.currency,
                            "paid_on": paid_on,
                        }),
                        now,
                    )
                    .await;
                }
            }
            (StatusCode::CREATED, Json(invoice)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct BatchPayRequest {
    /// Settlement date stamped onto each invoice.
    paid_on: chrono::NaiveDate,
    /// Cap how many invoices to settle in this run. Defaults to 500.
    max_count: Option<i64>,
}

#[derive(Serialize)]
pub(super) struct BatchPayResponse {
    paid_count: usize,
    total_paid_cents: i64,
    invoice_ids: Vec<String>,
}

/// Settle every `approved` vendor invoice with a single side-effect call.
/// Used by the daily `ap-payment-run` JobKind. Re-runnable: invoices
/// already in `paid` are skipped because the listing filter is `approved`.
pub(super) async fn batch_pay_vendor_invoices<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<BatchPayRequest>,
) -> Response {
    let limit = req.max_count.unwrap_or(500).clamp(1, 5000);
    let approved = match state
        .inventory
        .all_vendor_invoices(Some("approved"), limit)
        .await
    {
        Ok(rows) => rows,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let now = boss_clock_client::now_from(&state.clock).await;
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let mut paid_ids = Vec::with_capacity(approved.len());
    let mut total: i64 = 0;
    for mut invoice in approved {
        invoice.status = crate::types::VendorInvoiceStatus::Paid;
        invoice.paid_on = Some(req.paid_on);
        if let Err(e) = state
            .inventory
            .upsert_vendor_invoice_at(&invoice, now)
            .await
        {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        if let Some(pub_) = &state.publisher {
            // VENDOR_INVOICE_UPSERTED carries the full row at its new
            // state — this is what the rebuild path reads to re-derive
            // vendor_invoices from audit_log. Without it a rebuild after
            // a batch-pay sweep re-creates the row at status='approved'
            // (its last UPSERTED state) and the projection drifts from
            // reality. The PAID event below is the transition signal that
            // drives the finance.bill.paid ledger projection.
            pub_.emit_with_actor_at(
                crate::events::VENDOR_INVOICE_UPSERTED,
                actor.clone(),
                serde_json::to_value(&invoice).unwrap_or_default(),
                now,
            )
            .await;
            pub_.emit_with_actor_at(
                crate::events::VENDOR_INVOICE_PAID,
                actor.clone(),
                serde_json::json!({
                    "vendor_invoice_id": invoice.id,
                    "po_id": invoice.po_id,
                    "vendor": invoice.vendor,
                    "amount_cents": invoice.amount_cents,
                    "currency": invoice.currency,
                    "paid_on": req.paid_on,
                }),
                now,
            )
            .await;
        }
        total += invoice.amount_cents;
        paid_ids.push(invoice.id);
    }

    Json(BatchPayResponse {
        paid_count: paid_ids.len(),
        total_paid_cents: total,
        invoice_ids: paid_ids,
    })
    .into_response()
}
