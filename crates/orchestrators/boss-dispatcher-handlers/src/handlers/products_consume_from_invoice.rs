//! `products.consume_from_invoice` — drive the FG drawdown + COGS
//! recognition for every finished-goods line on an issued invoice,
//! through the products surface.
//!
//! The Q2 decision (docs/design/inventory-value-conservation.md,
//! resolved 2026-07-07): the CONSUME — the physical event — owns COGS.
//! Commerce used to UPDATE `finished_product_inventory` directly inside
//! the invoice tx (a cross-module projection write) and the
//! `invoice_issued` posting rule carried a DR 5100 / CR 1320 leg sized
//! at invoice-time cost. Both retire: this handler reacts to
//! `commerce.invoice.created` and POSTs
//! `/api/products/{sku}/inventory/consume` per FG line, which drains
//! the row's conserved value proportionally and posts
//! `finance.cogs.recognized` at exactly the drained cents (PR 6a),
//! tagged with the line's `revenue_category` so margin rollups sum
//! exact per-category COGS.
//!
//! Semantics that change with ownership:
//! - An invoice no longer 409s on insufficient FG — revenue posts, and
//!   the consume NAKs until stock lands (redelivery converges when the
//!   next produce completes) or dead-letters loudly. A persistent
//!   shortage is a visible backorder, not a silently blocked sale.
//! - Same-SKU lines aggregate into one consume (the idempotency key is
//!   per (invoice, sku), so two legs would collapse on the fact key
//!   and silently drop quantity — the same authoring hazard
//!   `inventory.parts.consume` guards).
//!
//! Idempotent end-to-end: key `inv:{invoice_id}:{sku}` becomes the
//! consume's `source_id`, so a redelivered invoice event re-applies as
//! a no-op.

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value as ExprValue;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use serde_json::{Value, json};
use std::sync::Arc;

use super::common;

pub struct ProductsConsumeFromInvoice {
    client: reqwest::Client,
    products_base: String,
}

impl ProductsConsumeFromInvoice {
    pub fn new(products_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            products_base: products_base.into(),
        })
    }
}

#[async_trait]
impl Handler for ProductsConsumeFromInvoice {
    fn name(&self) -> &'static str {
        "products.consume_from_invoice"
    }

    async fn invoke(
        &self,
        _args: &[(String, ExprValue)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let invoice_id = ctx
            .event_payload
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("invoice.created payload missing id".into()))?;
        let lines = fg_lines(&ctx.event_payload);
        for (sku, qty, revenue_category) in lines {
            // Resolve the FG location through the products surface —
            // the row holding the most stock, mirroring the drawdown
            // commerce used to run. No rows = the FG projection has
            // never seen this SKU → NAK (converges once the first
            // produce lands; dead-letters loudly if it never does).
            let detail_url = format!(
                "{}/api/products/{}",
                self.products_base.trim_end_matches('/'),
                sku
            );
            let detail = common::get_json(&self.client, &detail_url, &ctx.rule_name).await?;
            let location_id = detail
                .get("inventory")
                .and_then(|v| v.as_array())
                .and_then(|rows| {
                    rows.iter()
                        .max_by_key(|r| r.get("on_hand").and_then(|v| v.as_i64()).unwrap_or(0))
                })
                .and_then(|r| r.get("location_id").and_then(|v| v.as_str()))
                .map(str::to_string)
                .ok_or_else(|| {
                    HandlerError::Downstream(format!(
                        "no finished_product_inventory row for {sku} — cannot consume for invoice {invoice_id}"
                    ))
                })?;

            let mut body = json!({
                "location_id": location_id,
                "qty": qty,
                "idempotency_key": format!("inv:{invoice_id}:{sku}"),
            });
            if let Some(cat) = revenue_category {
                body["revenue_category"] = json!(cat);
            }
            let url = format!(
                "{}/api/products/{}/inventory/consume",
                self.products_base.trim_end_matches('/'),
                sku
            );
            common::post_json(&self.client, &url, &body, &ctx.rule_name).await?;
        }
        Ok(())
    }
}

/// The invoice's finished-goods legs: (sku, qty, revenue_category),
/// same-SKU lines aggregated (first line's category wins — one SKU
/// selling under two categories on one invoice would need per-line
/// consume keys, which the fact key can't carry today). Lines without
/// sku or with non-positive qty are non-FG (service work, contracts)
/// — revenue without COGS is the correct shape there.
fn fg_lines(payload: &Value) -> Vec<(String, i64, Option<String>)> {
    let Some(lines) = payload.get("line_items").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, i64, Option<String>)> = Vec::new();
    for line in lines {
        let Some(sku) = line.get("sku").and_then(|v| v.as_str()) else {
            continue;
        };
        let qty = line.get("qty").and_then(|v| v.as_i64()).unwrap_or(0);
        if qty <= 0 {
            continue;
        }
        let cat = line
            .get("revenue_category")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        match out.iter_mut().find(|(s, _, _)| s == sku) {
            Some((_, existing_qty, _)) => *existing_qty += qty,
            None => out.push((sku.to_string(), qty, cat)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fg_lines_extracts_aggregates_and_skips_non_fg() {
        let payload = json!({
            "id": "inv-1",
            "line_items": [
                { "sku": "FP-PALE-1-2-BBL", "qty": 3, "revenue_category": "wholesale" },
                { "sku": "FP-PALE-1-2-BBL", "qty": 2, "revenue_category": "wholesale" },
                { "sku": "FP-IPA-1-6-BBL", "qty": 1, "revenue_category": "taproom" },
                { "description": "delivery fee", "qty": 1 },
                { "sku": "FP-STOUT-1-2-BBL", "qty": 0 },
            ]
        });
        let lines = fg_lines(&payload);
        assert_eq!(
            lines,
            vec![
                (
                    "FP-PALE-1-2-BBL".to_string(),
                    5,
                    Some("wholesale".to_string())
                ),
                ("FP-IPA-1-6-BBL".to_string(), 1, Some("taproom".to_string())),
            ]
        );
    }

    #[test]
    fn fg_lines_empty_when_no_line_items() {
        assert!(fg_lines(&json!({ "id": "inv-1" })).is_empty());
    }
}
