//! `inventory.parts.consume` — per-item POST to
//! `/api/inventory/items/{sku}/consume`. Optionally fires the
//! labor-absorbed JE for production-consume steps that carry the
//! `labor_overhead_absorbed_cents` field.
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

        // Labor + overhead absorbed into WIP at production-consume
        // time — closes the balance-sheet identity gap between
        // production-consume (raw cost) and production-produce (FG
        // cost basis). Optional: only fires when the step author
        // stamped labor_overhead_absorbed_cents > 0.
        if let Some(absorbed) = step
            .metadata
            .get("labor_overhead_absorbed_cents")
            .and_then(|v| v.as_i64())
            .filter(|&n| n > 0)
        {
            let memo = format!("Labor + overhead absorbed into WIP (step:{})", step.step_id);
            let url = format!(
                "{}/api/inventory/labor-absorbed",
                self.inventory_base.trim_end_matches('/')
            );
            common::post_json(
                &self.client,
                &url,
                &json!({
                    "total_cost_cents": absorbed,
                    "debit_account": "1310",
                    "credit_account": "6100",
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
