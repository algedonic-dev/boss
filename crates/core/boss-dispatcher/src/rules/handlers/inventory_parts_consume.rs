//! `inventory.parts.consume` — per-item POST to
//! `/api/inventory/items/{sku}/consume`. Also fires the
//! overhead-absorbed JEs for production-consume steps that carry an
//! `overhead_absorbed` array — one DR 1310 / CR <expense> entry per
//! granular production driver (direct labor → 6100, process utilities
//! → 6300, production depreciation → 6900, …).
//!
//! Tolerant of missing/empty consumption arrays — shipment steps
//! bind this handler alongside `products.consume` and only one of
//! them lights up depending on whether the step's metadata names
//! parts vs products.

use super::common::{self, StepEvent};
use crate::rules::expr::Value;
use crate::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct ConsumedPart {
    part_sku: String,
    qty: u32,
}

/// One production-overhead driver absorbed into WIP at a
/// production-consume step: `amount_cents` capitalized DR 1310 / CR
/// `credit_account`. Stamped by the step author in the
/// `overhead_absorbed` metadata array — one entry per granular driver
/// (direct labor, process utilities, production depreciation, …), so
/// the books model burden as separable drivers rather than a single
/// $/bbl plug.
#[derive(Debug, Deserialize)]
struct OverheadAbsorbed {
    amount_cents: i64,
    credit_account: String,
    #[serde(default)]
    memo: Option<String>,
}

/// Parse the `overhead_absorbed` array from a production-consume step's
/// metadata. Each row is `{ amount_cents, credit_account, memo? }`. A
/// missing array, malformed rows, or non-positive amounts yield nothing
/// — absorption is optional, so steps (or tenants) that don't model
/// burden simply skip it.
fn overhead_absorbed(meta: &serde_json::Map<String, serde_json::Value>) -> Vec<OverheadAbsorbed> {
    meta.get("overhead_absorbed")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| serde_json::from_value::<OverheadAbsorbed>(r.clone()).ok())
                .filter(|a| a.amount_cents > 0)
                .collect()
        })
        .unwrap_or_default()
}

pub struct InventoryPartsConsume {
    client: reqwest::Client,
    inventory_base: String,
}

impl InventoryPartsConsume {
    pub fn new(inventory_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            inventory_base: inventory_base.into(),
        })
    }
}

#[async_trait]
impl Handler for InventoryPartsConsume {
    fn name(&self) -> &'static str {
        "inventory.parts.consume"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;
        let reason = arg_string(args, "reason")
            .unwrap_or("production")
            .to_string();

        // Try ingredients_consumed → parts_consumed → consumed aliases.
        // Missing/empty no-ops (shipment steps bind multiple consume
        // handlers and only one matches).
        let raw = match step
            .metadata
            .get("ingredients_consumed")
            .or_else(|| step.metadata.get("parts_consumed"))
            .or_else(|| step.metadata.get("consumed"))
        {
            Some(v) => v,
            None => return Ok(()),
        };
        let items: Vec<ConsumedPart> = serde_json::from_value(raw.clone())
            .map_err(|e| HandlerError::Downstream(format!("decode consumption: {e}")))?;
        if items.is_empty() {
            return Ok(());
        }
        let row_reason = format!("step:{} ({reason})", step.step_id);

        for it in items {
            if it.qty == 0 {
                return Err(HandlerError::Downstream(format!(
                    "qty must be positive for sku {}",
                    it.part_sku
                )));
            }
            let url = format!(
                "{}/api/inventory/items/{}/consume",
                self.inventory_base.trim_end_matches('/'),
                it.part_sku
            );
            // Deterministic idempotency key: `{step_id}:{part_sku}`. On a
            // redelivered step.done event this resolves to the same
            // consume `source_id`, so the relative `on_hand -= qty` is
            // applied exactly once even when this multi-handler subject
            // (production-produce / shipment) re-runs after a sibling
            // handler failed.
            common::post_json(
                &self.client,
                &url,
                &json!({
                    "qty": it.qty,
                    "reason": row_reason,
                    "idempotency_key": format!("{}:{}", step.step_id, it.part_sku),
                }),
                &ctx.rule_name,
            )
            .await?;
        }

        // Production overhead absorbed into WIP at production-consume
        // time — closes the balance-sheet identity gap between
        // production-consume (raw cost) and production-produce (FG cost
        // basis). Each entry capitalizes one granular driver DR 1310 /
        // CR <credit_account> (direct labor → 6100, process utilities →
        // 6300, production depreciation → 6900, …), so the driver flows
        // WIP → FG → COGS at sale instead of staying in period OpEx.
        // Data-driven: only the entries the step author stamped in
        // `overhead_absorbed` fire, each keyed by its credit account
        // (see the endpoint's source_id) so multiple drivers on one
        // step don't collide on idempotency.
        let absorb_url = format!(
            "{}/api/inventory/labor-absorbed",
            self.inventory_base.trim_end_matches('/')
        );
        for ab in overhead_absorbed(step.metadata) {
            let memo = ab.memo.unwrap_or_else(|| {
                format!(
                    "Production overhead absorbed into WIP — CR {} (step:{})",
                    ab.credit_account, step.step_id
                )
            });
            common::post_json(
                &self.client,
                &absorb_url,
                &json!({
                    "total_cost_cents": ab.amount_cents,
                    "debit_account": "1310",
                    "credit_account": ab.credit_account,
                    "memo": memo,
                    "step_id": step.step_id,
                }),
                &ctx.rule_name,
            )
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn overhead_absorbed_reads_each_driver() {
        let m = meta(json!({
            "overhead_absorbed": [
                { "amount_cents": 578_280, "credit_account": "6100", "memo": "Direct labor" },
                { "amount_cents": 88_480,  "credit_account": "6300" },
                { "amount_cents": 135_880, "credit_account": "6900" }
            ]
        }));
        let abs = overhead_absorbed(&m);
        assert_eq!(abs.len(), 3);
        assert_eq!(abs[0].credit_account, "6100");
        assert_eq!(abs[0].memo.as_deref(), Some("Direct labor"));
        assert_eq!(abs[1].amount_cents, 88_480);
        assert!(abs[2].memo.is_none());
    }

    #[test]
    fn overhead_absorbed_drops_nonpositive_and_missing() {
        // No array → nothing.
        assert!(overhead_absorbed(&meta(json!({}))).is_empty());
        // Zero / negative amounts are dropped.
        let m = meta(json!({
            "overhead_absorbed": [
                { "amount_cents": 0,    "credit_account": "6100" },
                { "amount_cents": -5,   "credit_account": "6300" },
                { "amount_cents": 1_000, "credit_account": "6900" }
            ]
        }));
        let abs = overhead_absorbed(&m);
        assert_eq!(abs.len(), 1);
        assert_eq!(abs[0].credit_account, "6900");
    }
}
