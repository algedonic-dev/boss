//! `ledger.bill.approve` — approve a general operating bill (rent,
//! utilities, insurance, …) through the ledger's AP subledger. POST
//! `/api/ledger/bills`.
//!
//! Reads `bill_category` + `amount_cents` (and optional `vendor` /
//! `currency`) from the completed `expense-bill` step's metadata. The
//! ledger routes the GL debit by `bill_category` via bill_accounts.toml;
//! the credit is always 2100 A/P. Decoupled from the inventory parts
//! vendor-invoice (`inventory.bill.approve`) — no PO, no per-SKU lines.

use super::common::{self, StepEvent};
use crate::rules::expr::Value;
use crate::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct LedgerBillApprove {
    client: reqwest::Client,
    ledger_base: String,
}

impl LedgerBillApprove {
    pub fn new(ledger_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            ledger_base: ledger_base.into(),
        })
    }
}

#[async_trait]
impl Handler for LedgerBillApprove {
    fn name(&self) -> &'static str {
        "ledger.bill.approve"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;

        let bill_category = step
            .metadata
            .get("bill_category")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step metadata missing bill_category".into()))?
            .to_string();

        let amount_cents = step
            .metadata
            .get("amount_cents")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| HandlerError::Downstream("step metadata missing amount_cents".into()))?;
        if amount_cents <= 0 {
            return Err(HandlerError::Downstream(format!(
                "bill amount must be positive, got {amount_cents}"
            )));
        }

        // Vendor defaults to the Job's subject (e.g. the facility Location).
        let vendor = step
            .metadata
            .get("vendor")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| step.subject_id.to_string());

        let currency = step
            .metadata
            .get("currency")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                arg(args, "currency").and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| "USD".to_string());

        let completed_on = step.completed_on.ok_or_else(|| {
            HandlerError::Downstream("step.done payload missing completed_on".into())
        })?;

        // Deterministic id from the step so a replay is idempotent (the
        // ledger upserts on this id + the fact's natural key).
        let id = format!("bill-step-{}", step.step_id);
        let body = json!({
            "id": id,
            "vendor": vendor,
            "bill_category": bill_category,
            "amount_cents": amount_cents,
            "currency": currency,
            "issued_on": completed_on,
            "approved_on": completed_on,
        });

        let url = format!(
            "{}/api/ledger/bills",
            self.ledger_base.trim_end_matches('/')
        );
        common::post_json(&self.client, &url, &body, &ctx.rule_name).await
    }
}
