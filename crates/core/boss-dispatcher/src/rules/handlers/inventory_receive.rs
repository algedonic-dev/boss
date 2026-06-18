//! `inventory.receive` — close out a delivery and flip the PO to received.
//!
//! Reads expected_items array from step metadata. For each item with
//! a positive qty: POST `/api/inventory/items/{sku}/receive`. Then
//! PUT the PO status to `received`. The PO id falls back to the same
//! `PO-{subject_id}` template the procurement step uses when the
//! author didn't stamp one.

use super::common::{self, StepEvent, dispatcher_actor_header};
use crate::rules::expr::Value;
use crate::rules::handler::{Handler, HandlerError, InvocationContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct ReceivedItem {
    part_sku: String,
    #[serde(default)]
    received_qty: Option<i64>,
    #[serde(default)]
    expected_qty: Option<i64>,
    #[serde(default)]
    unit_cost_cents: Option<i64>,
}

pub struct InventoryReceive {
    client: reqwest::Client,
    inventory_base: String,
}

impl InventoryReceive {
    pub fn new(inventory_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            inventory_base: inventory_base.into(),
        })
    }

    /// Fetch the PO's lines — the purchasing contract priced at
    /// placement (`inventory.po.place`). Receiving consumes the
    /// contract: each line's qty + unit_cost is what we agreed to
    /// buy at the vendor's price, so the receipt (and therefore the
    /// emergent `avg_cost_cents`) chains from the same numbers the
    /// vendor bill will. Returns (part_sku → (qty, unit_cost_cents)).
    /// Best-effort: a fetch failure returns an empty map and the
    /// caller skips unmatched lines (loudly zero, not silently
    /// mispriced).
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
impl Handler for InventoryReceive {
    fn name(&self) -> &'static str {
        "inventory.receive"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;
        let po_id = step.meta_string_or("po_id", |s| format!("PO-{}", s.subject_id));
        let header = dispatcher_actor_header(&ctx.rule_name);

        if let Some(raw) = step.metadata.get("expected_items") {
            let items: Vec<ReceivedItem> = serde_json::from_value(raw.clone())
                .map_err(|e| HandlerError::Downstream(format!("decode expected_items: {e}")))?;
            let contract = self.po_lines(&po_id, &header).await;
            for it in items {
                // A line carrying only part_sku receives what the PO
                // ordered, at the PO's price.
                let mut qty = it.received_qty.or(it.expected_qty);
                let mut cost = it.unit_cost_cents;
                if (qty.is_none() || cost.is_none())
                    && let Some((po_qty, po_cost)) = contract.get(it.part_sku.as_str())
                {
                    qty = qty.or(Some(*po_qty));
                    cost = cost.or(Some(*po_cost));
                }
                let qty = qty.unwrap_or(0);
                if qty == 0 {
                    continue;
                }
                // Deterministic idempotency key: `{step_id}:{part_sku}`. On a
                // redelivered step.done event this resolves to the same
                // receive `source_id`, so the relative `on_hand += qty` is
                // applied exactly once — closing the GL-1300 decoupling where
                // a redelivered receive double-incremented on_hand while the
                // once-posted DR-1300 stayed put. Exact mirror of
                // inventory_parts_consume.rs.
                let mut body = json!({
                    "part_sku": it.part_sku,
                    "qty": qty,
                    "po_id": po_id,
                    "idempotency_key": format!("{}:{}", step.step_id, it.part_sku),
                });
                if let Some(unit_cost) = cost {
                    body["unit_cost_cents"] = json!(unit_cost);
                }
                let sku = body["part_sku"].as_str().unwrap_or("");
                let url = format!(
                    "{}/api/inventory/items/{}/receive",
                    self.inventory_base.trim_end_matches('/'),
                    sku
                );
                common::post_json(&self.client, &url, &body, &ctx.rule_name).await?;
            }
        }

        // Flip PO status to received.
        let url = format!(
            "{}/api/inventory/orders/{}/status",
            self.inventory_base.trim_end_matches('/'),
            po_id
        );
        let resp = self
            .client
            .put(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", header)
            .json(&json!({ "status": "received" }))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PUT {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PUT {url} returned {status}: {body}"
            )));
        }
        Ok(())
    }
}
