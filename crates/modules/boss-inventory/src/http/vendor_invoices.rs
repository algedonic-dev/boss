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
use crate::types::{BillLine, VendorInvoice, VendorInvoiceStatus};

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
                    // Same shared helper as the in-tx fact write
                    // (types::bill_approved_payload) so the event and the
                    // live fact are byte-identical on rebuild.
                    pub_.emit_with_actor_at(
                        crate::events::VENDOR_INVOICE_APPROVED,
                        actor.clone(),
                        crate::types::bill_approved_payload(&invoice, approved_on),
                        now,
                    )
                    .await;
                }
                if let Some(paid_on) = invoice.paid_on {
                    pub_.emit_with_actor_at(
                        crate::events::VENDOR_INVOICE_PAID,
                        actor.clone(),
                        crate::types::bill_paid_payload(&invoice, paid_on),
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

/// Body for "a vendor posts its invoice for a PO". All fields optional —
/// the PO is the source of truth for vendor/lines/amount, so a bare `{}`
/// (or the simulator's webhook payload, whose extra fields serde ignores)
/// is a valid post.
#[derive(Deserialize, Default)]
pub(super) struct FromPoRequest {
    /// The date the invoice was received; defaults to the current sim day
    /// (the vendor posts on its own schedule, so post-time is the receipt).
    #[serde(default)]
    received_on: Option<chrono::NaiveDate>,
    /// Explicit vendor invoice number; defaults to `VI-{po_id}`.
    #[serde(default)]
    vendor_invoice_no: Option<String>,
}

/// A vendor "posts" its invoice for an existing PO — the automated
/// counterparty path. The simulator's per-vendor supplier chain routes
/// `inventory.vendor_invoice_received` here ~lead-time after the PO is
/// placed (the vendor's "API" responding); the human bill-approval step
/// later APPROVES it. The PO is the source of truth for the lines + amount
/// (the webhook only names the PO), so we resolve them from the PO row
/// rather than trusting the caller. Lands the invoice in **`received`**
/// state, idempotent on `vi-{po_id}` (the underlying upsert), so a
/// redelivered webhook is harmless.
pub(super) async fn create_vendor_invoice_from_po<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Path(po_id): axum::extract::Path<String>,
    Json(req): Json<FromPoRequest>,
) -> Response {
    let id = format!("vi-{po_id}");
    // Guard: the vendor's post must never DOWNGRADE an invoice the human
    // bill-approval step already advanced (to approved/paid). If a row for
    // this PO already exists it's authoritative — no-op. (The human flow is
    // often faster than the vendor's lead time, so bill-approval can land
    // first; without this a late vendor post would strand the invoice back
    // at `received`, where batch-pay never settles it.)
    match state.inventory.vendor_invoice_by_id(&id).await {
        Ok(Some(_)) => {
            return (
                StatusCode::OK,
                Json(serde_json::json!({ "ok": true, "id": id, "existing": true })),
            )
                .into_response();
        }
        Ok(None) => {}
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
    let po = match state.inventory.purchase_order_by_id(&po_id).await {
        Ok(Some(po)) => po,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, format!("PO {po_id} not found")).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let Some(vendor) = po.vendor.clone() else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("PO {po_id} has no vendor; cannot raise an invoice"),
        )
            .into_response();
    };
    let lines: Vec<BillLine> = po
        .lines
        .iter()
        .map(|l| BillLine {
            part_sku: l.part_sku.clone(),
            qty: l.qty as i64,
            unit_cost_cents: l.unit_cost_cents,
        })
        .collect();
    let amount_cents: i64 = lines.iter().map(|l| l.qty * l.unit_cost_cents).sum();
    let currency = po
        .lines
        .first()
        .map(|l| l.currency.clone())
        .unwrap_or_else(|| "USD".to_string());

    let now = boss_clock_client::now_from(&state.clock).await;
    let received_on = req.received_on.unwrap_or_else(|| now.date_naive());
    let invoice = VendorInvoice {
        id,
        po_id: po_id.clone(),
        vendor,
        vendor_invoice_no: req
            .vendor_invoice_no
            .unwrap_or_else(|| format!("VI-{po_id}")),
        amount_cents,
        currency,
        received_on,
        matched_on: None,
        approved_on: None,
        paid_on: None,
        status: VendorInvoiceStatus::Received,
        discrepancy_cents: None,
        discrepancy_kind: None,
        lines,
    };

    match state
        .inventory
        .upsert_vendor_invoice_at(&invoice, now)
        .await
    {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                // Received state → only the full-row UPSERTED event (no
                // approved/paid transition yet; that's the human step).
                let actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                pub_.emit_with_actor_at(
                    crate::events::VENDOR_INVOICE_UPSERTED,
                    actor,
                    serde_json::to_value(&invoice).unwrap_or_default(),
                    now,
                )
                .await;
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
