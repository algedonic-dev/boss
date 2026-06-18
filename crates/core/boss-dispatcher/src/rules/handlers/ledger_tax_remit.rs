//! `ledger.tax.remit` — POST a TaxFilingSnapshot to
//! `/api/ledger/tax-filings`, then if remit=true follow with a
//! `/api/ledger/tax-filings/{id}/remit` POST so the JE posts same-
//! day.

use super::common::{self, StepEvent, parse_date};
use crate::rules::expr::Value;
use crate::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct LedgerTaxRemit {
    client: reqwest::Client,
    ledger_base: String,
}

impl LedgerTaxRemit {
    pub fn new(ledger_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            ledger_base: ledger_base.into(),
        })
    }
}

fn quarter_prior_to(day: chrono::NaiveDate) -> (chrono::NaiveDate, chrono::NaiveDate) {
    use chrono::Datelike;
    let m = day.month();
    let y = day.year();
    let (start_y, start_m) = match m {
        1..=3 => (y - 1, 10),
        4..=6 => (y, 1),
        7..=9 => (y, 4),
        _ => (y, 7),
    };
    let start = chrono::NaiveDate::from_ymd_opt(start_y, start_m, 1)
        .expect("quarter start is always valid");
    let end_m = start_m + 2;
    let end_first =
        chrono::NaiveDate::from_ymd_opt(start_y, end_m, 1).expect("end first is always valid");
    // last day of quarter end month
    let next_month = if end_m == 12 {
        chrono::NaiveDate::from_ymd_opt(start_y + 1, 1, 1)
    } else {
        chrono::NaiveDate::from_ymd_opt(start_y, end_m + 1, 1)
    }
    .expect("next month is always valid");
    let end = next_month - chrono::Duration::days(1);
    let _ = end_first;
    (start, end)
}

fn due_on_default(period_end: chrono::NaiveDate) -> chrono::NaiveDate {
    use chrono::Datelike;
    let (y, m) = if period_end.month() == 12 {
        (period_end.year() + 1, 1)
    } else {
        (period_end.year(), period_end.month() + 1)
    };
    chrono::NaiveDate::from_ymd_opt(y, m, 20).expect("20th of any month is valid")
}

#[async_trait]
impl Handler for LedgerTaxRemit {
    fn name(&self) -> &'static str {
        "ledger.tax.remit"
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

        let kind = step
            .metadata
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step metadata missing kind".into()))?
            .to_string();
        let jurisdiction = step
            .metadata
            .get("jurisdiction")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step metadata missing jurisdiction".into()))?
            .to_string();
        let amount_cents = step
            .metadata
            .get("amount_cents")
            .and_then(|v| v.as_i64())
            .unwrap_or(1);
        if amount_cents <= 0 {
            return Err(HandlerError::Downstream(format!(
                "amount_cents must be positive, got {amount_cents}"
            )));
        }
        let (default_start, default_end) = quarter_prior_to(completed_on);
        let period_start = parse_date(step.metadata.get("period_start")).unwrap_or(default_start);
        let period_end = parse_date(step.metadata.get("period_end")).unwrap_or(default_end);
        if period_end < period_start {
            return Err(HandlerError::Downstream(
                "period_end must be >= period_start".into(),
            ));
        }

        // The ledger resolves the GL accounts + amount-derivation for
        // the kind from its `tax_kinds` reference table; the dispatcher
        // forwards only what the step knows (kind + jurisdiction +
        // period + amount), keeping the chart of accounts out of core.

        let provider = arg(args, "provider")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "in-house".to_string());
        let remit = arg(args, "remit")
            .map(|v| matches!(v, Value::Bool(true)))
            .unwrap_or(false);

        let id = format!(
            "tax-{}-{}-{}-{}",
            kind, jurisdiction, period_start, period_end
        );
        let due_on = due_on_default(period_end);
        let filing = json!({
            "id": id,
            "kind": kind,
            "jurisdiction": jurisdiction,
            "period_start": period_start,
            "period_end": period_end,
            "due_on": due_on,
            "filed_on": completed_on,
            "amount_cents": amount_cents,
            "provider": provider,
        });

        let create_url = format!(
            "{}/api/ledger/tax-filings",
            self.ledger_base.trim_end_matches('/')
        );
        common::post_json(&self.client, &create_url, &filing, &ctx.rule_name).await?;

        if remit {
            let remit_url = format!(
                "{}/api/ledger/tax-filings/{}/remit",
                self.ledger_base.trim_end_matches('/'),
                id
            );
            common::post_json(&self.client, &remit_url, &json!({}), &ctx.rule_name).await?;
        }
        Ok(())
    }
}
