//! `shipping.create` — POST a Shipment to `/api/shipping/shipments`.
//! Reads direction, origin, destination, carrier, tracking_number,
//! account_id, line_items from step metadata.

use super::common::{self, StepEvent};
use crate::rules::expr::Value;
use crate::rules::handler::{Handler, HandlerError, InvocationContext};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct ShippingCreate {
    client: reqwest::Client,
    shipping_base: String,
}

impl ShippingCreate {
    pub fn new(shipping_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            shipping_base: shipping_base.into(),
        })
    }
}

#[async_trait]
impl Handler for ShippingCreate {
    fn name(&self) -> &'static str {
        "shipping.create"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;

        let direction = step
            .metadata
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("outbound")
            .to_string();
        let origin = step
            .metadata
            .get("origin")
            .and_then(|v| v.as_str())
            .unwrap_or("brewery")
            .to_string();
        let destination = step
            .metadata
            .get("destination")
            .and_then(|v| v.as_str())
            .unwrap_or(step.subject_id)
            .to_string();
        let carrier = step
            .metadata
            .get("carrier")
            .and_then(|v| v.as_str())
            .unwrap_or("local-pickup")
            .to_string();
        let tracking_number = step
            .metadata
            .get("tracking_number")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let account_id = step
            .metadata
            .get("account_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let line_items = step
            .metadata
            .get("line_items")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
        let created_on = step.completed_on.ok_or_else(|| {
            HandlerError::Downstream("step.done payload missing completed_on".into())
        })?;

        let id = format!("ship-step-{}", step.step_id);
        let body = json!({
            "id": id,
            "direction": direction,
            "status": "label-created",
            "carrier": carrier,
            "tracking_number": tracking_number,
            "origin": origin,
            "destination": destination,
            "asset_ids": [],
            "line_items": line_items,
            "po_id": null,
            "order_id": null,
            "account_id": account_id,
            "created_on": created_on,
            "shipped_on": null,
            "estimated_delivery": null,
            "delivered_on": null,
        });

        let url = format!(
            "{}/api/shipping/shipments",
            self.shipping_base.trim_end_matches('/')
        );
        common::post_json(&self.client, &url, &body, &ctx.rule_name).await
    }
}
