//! Batch settlement of approved AP bills on the run date. One
//! parameterized handler backs two registrations that share byte-
//! identical behaviour, differing only in topic + target API path:
//!
//! - `inventory.bill.payment_batch` — settle every approved AP
//!   invoice on the run date. POST `/api/inventory/vendor-invoices/batch-pay`.
//! - `ledger.bill.payment_batch` — settle every approved ledger bill
//!   on the run date. POST `/api/ledger/bills/pay-run`. Drains 2100
//!   A/P → 1000 Cash per bill. The ledger-side sibling of
//!   `inventory.bill.payment_batch`.

use super::common::{self, StepEvent};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use serde_json::json;
use std::sync::Arc;

pub struct BillPaymentBatch {
    client: reqwest::Client,
    topic: &'static str,
    base: String,
    path: &'static str,
}

impl BillPaymentBatch {
    pub fn new(topic: &'static str, base: impl Into<String>, path: &'static str) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            topic,
            base: base.into(),
            path,
        })
    }
}

#[async_trait]
impl Handler for BillPaymentBatch {
    fn name(&self) -> &'static str {
        self.topic
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;
        let completed_on = step.completed_on.ok_or_else(|| {
            HandlerError::Downstream("step.done payload missing completed_on".into())
        })?;
        let paid_on = step
            .metadata
            .get("paid_on")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .unwrap_or(completed_on);

        let mut body = json!({ "paid_on": paid_on });
        if let Some(Value::Int(cap)) = arg(args, "max_count") {
            body["max_count"] = json!(*cap);
        }

        let url = format!("{}{}", self.base.trim_end_matches('/'), self.path);
        common::post_json(&self.client, &url, &body, &ctx.rule_name).await
    }
}
