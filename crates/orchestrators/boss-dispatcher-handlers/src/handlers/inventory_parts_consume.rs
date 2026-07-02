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
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
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
        // step don't collide on idempotency. The shared parser
        // (common::overhead_absorbed) aggregates same-account rows and
        // is the same parse the drain side reconstructs fact ids from,
        // so what gets capitalized and what gets drained agree.
        let absorb_url = format!(
            "{}/api/inventory/overhead-absorbed",
            self.inventory_base.trim_end_matches('/')
        );
        for ab in common::overhead_absorbed(step.metadata, step.step_id) {
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
