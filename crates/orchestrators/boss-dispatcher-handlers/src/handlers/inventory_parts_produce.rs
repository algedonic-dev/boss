//! `inventory.parts.produce` — per-item POST to
//! `/api/inventory/items/{sku}/receive` (the same endpoint
//! procurement receipts use). Mirrors `inventory.parts.consume`
//! for the produces side.

use super::common::{self, StepEvent};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct ProducedPart {
    part_sku: String,
    qty: u32,
}

pub struct InventoryPartsProduce {
    client: reqwest::Client,
    inventory_base: String,
}

impl InventoryPartsProduce {
    pub fn new(inventory_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            inventory_base: inventory_base.into(),
        })
    }
}

#[async_trait]
impl Handler for InventoryPartsProduce {
    fn name(&self) -> &'static str {
        "inventory.parts.produce"
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

        let raw = step
            .metadata
            .get("produces_parts")
            .or_else(|| step.metadata.get("parts_produced"))
            .ok_or_else(|| {
                HandlerError::Downstream(
                    "step metadata missing produces_parts / parts_produced array".into(),
                )
            })?;
        let items: Vec<ProducedPart> = serde_json::from_value(raw.clone())
            .map_err(|e| HandlerError::Downstream(format!("decode production: {e}")))?;
        if items.is_empty() {
            return Err(HandlerError::Downstream("production array is empty".into()));
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
                "{}/api/inventory/items/{}/receive",
                self.inventory_base.trim_end_matches('/'),
                it.part_sku
            );
            // Deterministic idempotency key: `{step_id}:{part_sku}`. On a
            // redelivered step.done event this resolves to the same receive
            // `source_id`, so the relative `on_hand += qty` is applied
            // exactly once even when this multi-handler subject re-runs
            // after a sibling handler failed. One-line parity with
            // inventory_parts_consume.rs.
            common::post_json(
                &self.client,
                &url,
                &json!({
                    "part_sku": it.part_sku,
                    "qty": it.qty,
                    "reason": row_reason,
                    "idempotency_key": format!("{}:{}", step.step_id, it.part_sku),
                }),
                &ctx.rule_name,
            )
            .await?;
        }
        Ok(())
    }
}
