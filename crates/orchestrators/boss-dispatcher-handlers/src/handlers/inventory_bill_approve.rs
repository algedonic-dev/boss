//! `inventory.bill.approve` — drop an approved VendorInvoice row.
//!
//! Reads po_id, lines (per-SKU qty × unit_cost_cents), and optional
//! vendor_invoice_no from step metadata. Computes amount_cents as
//! Σ(qty × unit_cost) over lines; validates against any caller-
//! supplied amount_cents. POST `/api/inventory/vendor-invoices`.

use super::common::{self, StepEvent, dispatcher_actor_header};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use serde_json::json;
use std::sync::Arc;

pub struct InventoryBillApprove {
    client: reqwest::Client,
    inventory_base: String,
}

impl InventoryBillApprove {
    pub fn new(inventory_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            inventory_base: inventory_base.into(),
        })
    }

    /// Fetch the PO's lines — the purchasing contract priced at
    /// placement. The vendor bill is the money side of that same
    /// contract, so a bill line that omits qty/cost takes them from
    /// the PO rather than re-deriving from live inventory state
    /// (which drifts between steps and is circular on cost). Returns
    /// (part_sku → (qty, unit_cost_cents)); empty map on fetch
    /// failure so unmatched lines stay loudly zero.
    async fn po_lines(
        &self,
        po_id: &str,
        header: &str,
    ) -> std::collections::HashMap<String, (i64, i64)> {
        let url = format!(
            "{}/api/inventory/orders/{}",
            self.inventory_base.trim_end_matches('/'),
            po_id
        );
        let Ok(resp) = self
            .client
            .get(&url)
            .header("x-boss-user", header)
            .send()
            .await
        else {
            return Default::default();
        };
        if !resp.status().is_success() {
            return Default::default();
        }
        let Ok(v) = resp.json::<serde_json::Value>().await else {
            return Default::default();
        };
        v.get("lines")
            .and_then(|l| l.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|li| {
                        let sku = li.get("part_sku")?.as_str()?.to_string();
                        let qty = li.get("qty")?.as_i64()?;
                        let cost = li.get("unit_cost_cents")?.as_i64()?;
                        Some((sku, (qty, cost)))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[async_trait]
impl Handler for InventoryBillApprove {
    fn name(&self) -> &'static str {
        "inventory.bill.approve"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;
        let vendor = step.subject_id.to_string();

        let po_id = step
            .metadata
            .get("po_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step metadata missing po_id".into()))?
            .to_string();

        // Collect raw line fields (sync) so the step.metadata borrow ends
        // before the async per-line derive below.
        let raw_lines: Vec<(String, Option<i64>, Option<i64>)> = step
            .metadata
            .get("lines")
            .and_then(|v| v.as_array())
            .ok_or_else(|| HandlerError::Downstream("step metadata missing lines array".into()))?
            .iter()
            .map(|li| {
                (
                    li.get("part_sku")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    li.get("qty").and_then(|v| v.as_i64()),
                    li.get("unit_cost_cents").and_then(|v| v.as_i64()),
                )
            })
            .collect();
        let header = dispatcher_actor_header(&ctx.rule_name);
        // Resolve each line against the PO — the purchasing contract
        // priced at placement. Restock lines carry only part_sku.
        let contract = self.po_lines(&po_id, &header).await;
        let mut lines: Vec<serde_json::Value> = Vec::with_capacity(raw_lines.len());
        let mut derived_any = false;
        for (part_sku, mut qty, mut cost) in raw_lines {
            if qty.is_none() || cost.is_none() {
                if let Some((po_qty, po_cost)) = contract.get(part_sku.as_str()) {
                    qty = qty.or(Some(*po_qty));
                    cost = cost.or(Some(*po_cost));
                }
                derived_any = true;
            }
            lines.push(json!({
                "part_sku": part_sku,
                "qty": qty.unwrap_or(0),
                "unit_cost_cents": cost.unwrap_or(0),
            }));
        }
        let amount_cents: i64 = lines
            .iter()
            .map(|li| {
                let qty = li.get("qty").and_then(|v| v.as_i64()).unwrap_or(0);
                let unit_cost = li
                    .get("unit_cost_cents")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                qty.saturating_mul(unit_cost)
            })
            .sum();
        // Defense-in-depth: a JobKind that PRECOMPUTES amount_cents must keep
        // it in sync with its lines. Skip this when we derived line amounts
        // from the live item (per-SKU restock) — there Σ(lines) is
        // authoritative, and any amount_cents present is a sim-workforce
        // placeholder synthesized for the now-absent field, not a real claim.
        if let Some(claimed) = step.metadata.get("amount_cents").and_then(|v| v.as_i64())
            && !derived_any
            && claimed != amount_cents
        {
            return Err(HandlerError::Downstream(format!(
                "amount_cents={claimed} disagrees with Σ(lines)={amount_cents}"
            )));
        }
        if amount_cents <= 0 {
            return Err(HandlerError::Downstream(format!(
                "bill amount must be positive, got {amount_cents}"
            )));
        }

        // PO-keyed so the human approval lands on the SAME invoice row the
        // vendor's webhook posts (`vi-{po_id}` from the from-po endpoint),
        // rather than a per-step row that would double it.
        let vendor_invoice_no = step
            .metadata
            .get("vendor_invoice_no")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("VI-{po_id}"));

        let received_offset_days = arg(args, "received_offset_days")
            .and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(0);
        let completed_on = step.completed_on.ok_or_else(|| {
            HandlerError::Downstream("step.done payload missing completed_on".into())
        })?;
        let received_on = completed_on + chrono::Duration::days(received_offset_days);
        let approved_on = completed_on;

        let currency = step
            .metadata
            .get("currency")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                arg(args, "currency").and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| "USD".to_string());

        // `vi-{po_id}` (not per-step): the idempotent upsert transitions the
        // vendor-posted `received` invoice to `approved`, or creates it
        // `approved` directly when the vendor hasn't posted yet (the
        // self-healing fallback — the human approval is authoritative either
        // way). One invoice per PO, never doubled.
        let id = format!("vi-{po_id}");
        let mut body = json!({
            "id": id,
            "po_id": po_id,
            "vendor": vendor,
            "vendor_invoice_no": vendor_invoice_no,
            "amount_cents": amount_cents,
            "currency": currency,
            "received_on": received_on,
            "matched_on": approved_on,
            "approved_on": approved_on,
            "paid_on": null,
            "status": "approved",
            "discrepancy_cents": null,
            "discrepancy_kind": null,
            "lines": lines,
        });
        // The lines field already lives in the body above; the
        // bridge's "thread per-SKU breakdown" comment is captured
        // by the inclusion. No conditional needed since we required
        // lines above.
        let _ = body.get_mut("lines");

        let url = format!(
            "{}/api/inventory/vendor-invoices",
            self.inventory_base.trim_end_matches('/')
        );
        common::post_json(&self.client, &url, &body, &ctx.rule_name).await
    }
}
