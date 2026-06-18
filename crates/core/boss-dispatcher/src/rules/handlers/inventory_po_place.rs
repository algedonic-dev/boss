//! `inventory.po.place` — drop a PurchaseOrder row on
//! step.done.procurement.
//!
//! The dispatcher-side replacement for
//! `boss-inventory-sim-bridge::PoPlaceEmitter`. Reads step metadata
//! for the explicit po_id + lines (the bill-approval step that runs
//! later references the SAME po_id, so the FK has to resolve at
//! both ends); falls back to a subject-derived id when the JobKind
//! author didn't set one.
//!
//! POST `/api/inventory/orders/batch` with a single-element array so
//! the explicit po_id survives — the non-batch /create endpoint
//! mints its own UUID and discards the caller's id.

use super::common::{self, StepEvent};
use crate::rules::expr::Value;
use crate::rules::handler::{Handler, HandlerError, InvocationContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// A PO line as authored in step metadata. The JobKind usually
/// supplies only `part_sku` (templated from the Job's
/// `metadata.part_sku`); qty and price are resolved at placement.
#[derive(Deserialize)]
struct OrderedItem {
    part_sku: String,
    #[serde(default)]
    qty: Option<i64>,
    #[serde(default)]
    unit_cost_cents: Option<i64>,
}

pub struct InventoryPoPlace {
    client: reqwest::Client,
    inventory_base: String,
}

impl InventoryPoPlace {
    pub fn new(inventory_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            inventory_base: inventory_base.into(),
        })
    }

    /// Resolve a line's qty + unit price from the inventory item.
    /// Quantity defaults to the item's `reorder_qty` (our reorder
    /// decision); price comes from `vendor_price_cents` — the
    /// supplier's agreed price, set as data. `avg_cost_cents` is
    /// deliberately NOT consulted: it is our emergent cost basis,
    /// an output of purchasing, never an input to it.
    async fn price_line(&self, sku: &str, header: &str) -> Result<(i64, i64), HandlerError> {
        let url = format!(
            "{}/api/inventory/items/{}",
            self.inventory_base.trim_end_matches('/'),
            sku
        );
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", header)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {} — cannot price PO line for {sku}",
                resp.status()
            )));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("decode {url}: {e}")))?;
        let qty = v
            .get("reorder_qty")
            .and_then(|x| x.as_i64())
            .filter(|q| *q > 0)
            .ok_or_else(|| {
                HandlerError::Downstream(format!("item {sku} has no usable reorder_qty"))
            })?;
        let price = v
            .get("vendor_price_cents")
            .and_then(|x| x.as_i64())
            .ok_or_else(|| {
                HandlerError::Downstream(format!(
                    "item {sku} has no vendor_price_cents — refusing to place an unpriced PO"
                ))
            })?;
        Ok((qty, price))
    }
}

#[async_trait]
impl Handler for InventoryPoPlace {
    fn name(&self) -> &'static str {
        "inventory.po.place"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;

        let po_id = step.meta_string_or("po_id", |s| format!("PO-{}", s.subject_id));
        let vendor = step.meta_string_or("vendor", |s| s.subject_id.to_string());

        let placed_on = step.completed_on.ok_or_else(|| {
            HandlerError::Downstream("step.done payload missing completed_on".into())
        })?;
        let expected_offset_days = args
            .iter()
            .find(|(k, _)| k == "expected_offset_days")
            .and_then(|(_, v)| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(14);
        let expected_on = placed_on + chrono::Duration::days(expected_offset_days);

        // Lines come from step metadata. The restock JobKinds author
        // a single `{ part_sku = "{metadata.part_sku}" }` entry; any
        // line missing qty / unit_cost is priced here, once, at
        // placement — qty from the item's reorder_qty, price from
        // its vendor_price_cents (the supplier's agreed price). The
        // PO is the purchasing contract: receive and bill-approval
        // read ITS lines instead of re-deriving, so receipt value,
        // billed amount, and the emergent avg_cost all chain from
        // the same numbers.
        let raw: Vec<OrderedItem> = match step.metadata.get("items") {
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| HandlerError::Downstream(format!("decode items: {e}")))?,
            None => Vec::new(),
        };
        let header = common::dispatcher_actor_header(&ctx.rule_name);
        let mut lines_vec = Vec::with_capacity(raw.len());
        for it in raw {
            let (qty, price) = match (it.qty, it.unit_cost_cents) {
                (Some(q), Some(c)) => (q, c),
                (q, c) => {
                    let (dq, dp) = self.price_line(&it.part_sku, &header).await?;
                    (q.unwrap_or(dq), c.unwrap_or(dp))
                }
            };
            lines_vec.push(json!({
                "part_sku": it.part_sku,
                "qty": qty,
                "unit_cost_cents": price,
            }));
        }
        let lines = json!(lines_vec);

        let body = json!([{
            "id": po_id,
            "vendor": vendor,
            "status": "submitted",
            "placed_on": placed_on,
            "expected_on": expected_on,
            "received_on": null,
            "lines": lines,
        }]);

        let url = format!(
            "{}/api/inventory/orders/batch",
            self.inventory_base.trim_end_matches('/')
        );
        common::post_json(&self.client, &url, &body, &ctx.rule_name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step_done_payload() -> serde_json::Value {
        json!({
            "job_id": "job-1",
            "step_id": "step-1",
            "kind": "procurement",
            "subject_kind": "vendor",
            "subject_id": "vnd-001",
            "completed_on": "2025-04-15",
            "metadata": {
                "po_id": "PO-vnd-001-2025-04-15",
                "items": [
                    { "part_sku": "ING-MALT-2ROW-50", "qty": 40, "unit_cost_cents": 4500 }
                ]
            }
        })
    }

    #[tokio::test]
    async fn rejects_when_completed_on_missing() {
        let h = InventoryPoPlace::new("http://127.0.0.1:1");
        let mut bad = step_done_payload();
        bad.as_object_mut().unwrap().remove("completed_on");
        let ctx = InvocationContext {
            rule_name: "test".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.done.procurement".into(),
            event_payload: bad,
        };
        let res = h.invoke(&[], &ctx).await;
        assert!(matches!(res, Err(HandlerError::Downstream(_))));
    }

    #[tokio::test]
    async fn rejects_when_metadata_missing() {
        let h = InventoryPoPlace::new("http://127.0.0.1:1");
        let mut bad = step_done_payload();
        bad.as_object_mut().unwrap().remove("metadata");
        let ctx = InvocationContext {
            rule_name: "test".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.done.procurement".into(),
            event_payload: bad,
        };
        let res = h.invoke(&[], &ctx).await;
        assert!(matches!(res, Err(HandlerError::Downstream(_))));
    }

    /// Stub inventory-api: serves one item (optionally priced) and
    /// captures the PO batch body the handler POSTs.
    async fn stub_inventory(
        item: serde_json::Value,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    ) {
        use axum::{Json, Router, routing::get, routing::post};
        let captured: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
            Default::default();
        let cap = captured.clone();
        let app = Router::new()
            .route(
                "/api/inventory/items/{sku}",
                get(move || {
                    let item = item.clone();
                    async move { Json(item) }
                }),
            )
            .route(
                "/api/inventory/orders/batch",
                post(move |Json(body): Json<serde_json::Value>| {
                    let cap = cap.clone();
                    async move {
                        *cap.lock().unwrap() = Some(body);
                        Json(json!({"ok": true}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), captured)
    }

    fn sku_only_payload() -> serde_json::Value {
        json!({
            "job_id": "job-1",
            "step_id": "step-1",
            "kind": "procurement",
            "subject_kind": "vendor",
            "subject_id": "vnd-001",
            "completed_on": "2025-04-15",
            "metadata": {
                "po_id": "PO-vnd-001-2025-04-15",
                "items": [ { "part_sku": "ING-MALT-2ROW-50" } ]
            }
        })
    }

    fn ctx_for(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "test".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.done.procurement".into(),
            event_payload: payload,
        }
    }

    #[tokio::test]
    async fn sku_only_line_is_priced_from_vendor_price_at_placement() {
        let (base, captured) = stub_inventory(json!({
            "part_sku": "ING-MALT-2ROW-50",
            "reorder_qty": 29000,
            "avg_cost_cents": 9999,       // must NOT be consulted
            "vendor_price_cents": 2500,   // the supplier's price wins
        }))
        .await;
        let h = InventoryPoPlace::new(base);
        h.invoke(&[], &ctx_for(sku_only_payload())).await.unwrap();
        let body = captured.lock().unwrap().clone().expect("PO posted");
        let line = &body[0]["lines"][0];
        assert_eq!(line["qty"], json!(29000));
        assert_eq!(
            line["unit_cost_cents"],
            json!(2500),
            "PO line must be priced from vendor_price_cents, not our avg_cost"
        );
    }

    #[tokio::test]
    async fn unpriced_part_refuses_po_placement() {
        let (base, captured) = stub_inventory(json!({
            "part_sku": "ING-MALT-2ROW-50",
            "reorder_qty": 29000,
            "avg_cost_cents": 9999,
            // no vendor_price_cents
        }))
        .await;
        let h = InventoryPoPlace::new(base);
        let res = h.invoke(&[], &ctx_for(sku_only_payload())).await;
        assert!(
            matches!(res, Err(HandlerError::Downstream(_))),
            "missing vendor_price_cents must refuse placement loudly"
        );
        assert!(captured.lock().unwrap().is_none(), "no PO may be posted");
    }
}
