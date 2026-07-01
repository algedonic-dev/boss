//! `products.consume` — per-row POST to
//! `/api/products/{sku}/inventory/consume`. Mirror of
//! `products.produce` for the sale side.

use super::common::{self, StepEvent};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct ConsumedProduct {
    sku: String,
    qty: i32,
    location_id: String,
    #[serde(default)]
    revenue_category: Option<String>,
}

pub struct ProductsConsume {
    client: reqwest::Client,
    products_base: String,
}

impl ProductsConsume {
    pub fn new(products_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            products_base: products_base.into(),
        })
    }
}

#[async_trait]
impl Handler for ProductsConsume {
    fn name(&self) -> &'static str {
        "products.consume"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;
        let Some(raw) = step.metadata.get("consumes_products") else {
            return Ok(());
        };
        let items: Vec<ConsumedProduct> = serde_json::from_value(raw.clone())
            .map_err(|e| HandlerError::Downstream(format!("decode consumes_products: {e}")))?;
        if items.is_empty() {
            return Ok(());
        }

        for it in items {
            if it.qty <= 0 {
                return Err(HandlerError::Downstream(format!(
                    "qty must be positive for sku {}",
                    it.sku
                )));
            }
            let mut body = json!({
                "sku": it.sku,
                "location_id": it.location_id,
                "qty": it.qty,
                // Deterministic key so a redelivered consume applies the
                // relative on_hand decrement exactly once.
                "idempotency_key": format!("{}:{}", step.step_id, it.sku),
            });
            if let Some(cat) = it.revenue_category.as_deref() {
                body["revenue_category"] = json!(cat);
            }
            let url = format!(
                "{}/api/products/{}/inventory/consume",
                self.products_base.trim_end_matches('/'),
                it.sku
            );
            common::post_json(&self.client, &url, &body, &ctx.rule_name).await?;
        }
        Ok(())
    }
}
