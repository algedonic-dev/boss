//! `ledger.tax.accrue` — POST a standalone tax accrual to
//! `/api/ledger/tax-accruals` (DR an expense account / CR a liability
//! account) the moment a production step completes.
//!
//! Used for federal beer excise tax: each brew batch accrues
//! `excise_bbl × rate_cents_per_bbl` ($3.50/bbl TTB small-brewer rate)
//! as DR 6550 Excise Tax Expense / CR 2320 Excise Tax Payable, exactly
//! the way sales tax accrues per invoice line. The quarterly
//! excise-tax-filing JobKind later drains 2320 → 1000 Cash. The
//! liability is credited by this production source fact, not at filing
//! time, so the filing's `period-excise` derive_basis sums the 2320
//! credit balance for the period.

use super::common::{self, StepEvent};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use serde_json::json;
use std::sync::Arc;

pub struct LedgerTaxAccrue {
    client: reqwest::Client,
    ledger_base: String,
}

impl LedgerTaxAccrue {
    pub fn new(ledger_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            ledger_base: ledger_base.into(),
        })
    }
}

#[async_trait]
impl Handler for LedgerTaxAccrue {
    fn name(&self) -> &'static str {
        "ledger.tax.accrue"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;

        // Taxable barrels for this batch live in step metadata
        // (`excise_bbl`, seeded per package step from the brew's
        // produces_products volume). A batch with no/zero taxable
        // barrels is a no-op — nothing to accrue.
        let excise_bbl = step
            .metadata
            .get("excise_bbl")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if excise_bbl <= 0 {
            return Ok(());
        }

        let rate_cents_per_bbl = arg(args, "rate_cents_per_bbl")
            .and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .ok_or_else(|| {
                HandlerError::Downstream("rate_cents_per_bbl arg missing or not an int".into())
            })?;
        let liability_account = arg(args, "liability_account")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                HandlerError::Downstream("liability_account arg missing or not a string".into())
            })?;
        let expense_account = arg(args, "expense_account")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                HandlerError::Downstream("expense_account arg missing or not a string".into())
            })?;
        let jurisdiction = arg(args, "jurisdiction")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                HandlerError::Downstream("jurisdiction arg missing or not a string".into())
            })?;

        let amount_cents = excise_bbl * rate_cents_per_bbl;

        let posted_on = step.completed_on.ok_or_else(|| {
            HandlerError::Downstream("step.done payload missing completed_on".into())
        })?;

        let body = json!({
            "id": format!("excise-{}", step.step_id),
            "expense_account": expense_account,
            "liability_account": liability_account,
            "amount_cents": amount_cents,
            "posted_on": posted_on,
            "jurisdiction": jurisdiction,
        });

        let url = format!(
            "{}/api/ledger/tax-accruals",
            self.ledger_base.trim_end_matches('/')
        );
        common::post_json(&self.client, &url, &body, &ctx.rule_name).await
    }
}
