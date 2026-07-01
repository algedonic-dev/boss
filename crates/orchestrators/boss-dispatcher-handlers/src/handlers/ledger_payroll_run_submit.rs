//! `ledger.payroll.run.submit` — POST to
//! `/api/ledger/payroll-runs/synthesize`. The server computes
//! per-employee gross/withheld/net and posts the canonical payroll
//! event + financial fact + journal entry.

use super::common::{self, StepEvent, parse_date};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use serde_json::json;
use std::sync::Arc;

pub struct LedgerPayrollRunSubmit {
    client: reqwest::Client,
    ledger_base: String,
}

impl LedgerPayrollRunSubmit {
    pub fn new(ledger_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            ledger_base: ledger_base.into(),
        })
    }
}

#[async_trait]
impl Handler for LedgerPayrollRunSubmit {
    fn name(&self) -> &'static str {
        "ledger.payroll.run.submit"
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

        let periods_per_year = arg(args, "periods_per_year")
            .and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(26);
        if periods_per_year <= 0 {
            return Err(HandlerError::Downstream(
                "periods_per_year must be > 0".into(),
            ));
        }
        let withholding_bps = arg(args, "withholding_bps")
            .and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(2200);
        let employer_cost_bps = arg(args, "employer_cost_bps")
            .and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(1500);
        let provider = arg(args, "provider")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "in-house".to_string());

        let run_date = parse_date(step.metadata.get("run_date")).unwrap_or(completed_on);
        let period_start = parse_date(step.metadata.get("period_start")).ok_or_else(|| {
            HandlerError::Downstream(
                "step metadata missing or unparseable period_start (YYYY-MM-DD)".into(),
            )
        })?;
        let period_end = parse_date(step.metadata.get("period_end")).ok_or_else(|| {
            HandlerError::Downstream(
                "step metadata missing or unparseable period_end (YYYY-MM-DD)".into(),
            )
        })?;

        let body = json!({
            "run_date": run_date,
            "period_start": period_start,
            "period_end": period_end,
            "periods_per_year": periods_per_year,
            "withholding_bps": withholding_bps,
            "employer_cost_bps": employer_cost_bps,
            "provider": provider,
        });

        let url = format!(
            "{}/api/ledger/payroll-runs/synthesize",
            self.ledger_base.trim_end_matches('/')
        );
        common::post_json(&self.client, &url, &body, &ctx.rule_name).await
    }
}
