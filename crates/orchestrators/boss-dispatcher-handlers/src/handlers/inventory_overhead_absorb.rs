//! `inventory.overhead.absorb` — capitalize one production-overhead
//! driver into WIP (`DR 1310 / CR <credit_account>`) the moment a
//! production-consume step completes, sized `rate_cents_per_bbl ×
//! batch bbl` at runtime.
//!
//! The excise pattern (`ledger.tax.accrue`) applied to absorption:
//! the per-bbl rate is a **rule arg** — data in the dispatcher-rules
//! registry — and the batch size comes from the model's own job data,
//! so the seed carries no hand-multiplied amounts. One `[[rule.do]]`
//! per driver (direct labor → 6100, process utilities → 6300,
//! production depreciation → 6900, …); each posts its own fact,
//! idempotent on the absorption endpoint's
//! `overhead-absorbed@{step_id}:{credit_account}` source id, so a
//! redelivered event re-applies as a no-op and distinct drivers never
//! collide.
//!
//! Batch bbl is derived exactly the way the brewery model states it
//! (and the seed test pins it): any step carrying `batch_bbl`
//! (morning-brew styles stamp it on packaging-allocate), else the
//! summed `excise_bbl` of the production-produce steps
//! (seasonal-release, single format). A job with neither is a no-op —
//! a non-brewing tenant's production-consume step has nothing to
//! absorb, mirroring the excise handler's zero-barrels no-op.
//!
//! The drain side (`products.produce`, basis `drain-actual-wip`)
//! reconstructs these facts by source id from its own
//! `overhead_accounts` rule arg — the capitalize-set and the
//! drain-set are both rules data, and the brewery seed test asserts
//! they agree.

use super::common::{self, StepEvent};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use serde_json::json;
use std::sync::Arc;

pub struct InventoryOverheadAbsorb {
    client: reqwest::Client,
    jobs_base: String,
    inventory_base: String,
}

impl InventoryOverheadAbsorb {
    pub fn new(jobs_base: impl Into<String>, inventory_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            jobs_base: jobs_base.into(),
            inventory_base: inventory_base.into(),
        })
    }
}

#[async_trait]
impl Handler for InventoryOverheadAbsorb {
    fn name(&self) -> &'static str {
        "inventory.overhead.absorb"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;

        let rate_cents_per_bbl = arg(args, "rate_cents_per_bbl")
            .and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .ok_or_else(|| {
                HandlerError::Downstream("rate_cents_per_bbl arg missing or not an int".into())
            })?;
        let credit_account = arg(args, "credit_account")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                HandlerError::Downstream("credit_account arg missing or not a string".into())
            })?;
        let driver = arg(args, "driver")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| HandlerError::Downstream("driver arg missing or not a string".into()))?;

        // The batch size lives on sibling steps, not the completing
        // consume step — fetch the job (the same read the produce
        // handler's drain basis does).
        let url = format!(
            "{}/api/jobs/{}",
            self.jobs_base.trim_end_matches('/'),
            step.job_id
        );
        let job = common::get_json(&self.client, &url, &ctx.rule_name).await?;
        let bbl = batch_bbl_from_job(&job);
        if bbl <= 0 {
            return Ok(());
        }

        let amount_cents = rate_cents_per_bbl.saturating_mul(bbl);
        let body = json!({
            "total_cost_cents": amount_cents,
            "debit_account": "1310",
            "credit_account": credit_account,
            "memo": format!(
                "{driver} — {bbl} bbl × {}¢/bbl (step:{})",
                rate_cents_per_bbl, step.step_id
            ),
            "step_id": step.step_id,
        });
        let absorb_url = format!(
            "{}/api/inventory/overhead-absorbed",
            self.inventory_base.trim_end_matches('/')
        );
        common::post_json(&self.client, &absorb_url, &body, &ctx.rule_name).await
    }
}

/// Batch size in BBL from the job's own data: the first step carrying
/// `batch_bbl` in its metadata (morning-brew styles stamp it on
/// packaging-allocate), else the summed `excise_bbl` of the
/// production-produce steps (seasonal-release). 0 when the job states
/// neither — the caller no-ops.
fn batch_bbl_from_job(job: &serde_json::Value) -> i64 {
    let Some(steps) = job.get("steps").and_then(|v| v.as_array()) else {
        return 0;
    };
    let stamped = steps.iter().find_map(|s| {
        s.get("metadata")
            .and_then(|m| m.get("batch_bbl"))
            .and_then(|v| v.as_i64())
    });
    if let Some(bbl) = stamped {
        return bbl;
    }
    steps
        .iter()
        .filter(|s| s.get("kind").and_then(|v| v.as_str()) == Some("production-produce"))
        .filter_map(|s| {
            s.get("metadata")
                .and_then(|m| m.get("excise_bbl"))
                .and_then(|v| v.as_i64())
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn batch_bbl_prefers_stamped_batch_bbl() {
        let job = json!({ "steps": [
            { "kind": "production-consume", "metadata": {} },
            { "kind": "packaging-allocate", "metadata": { "batch_bbl": 158 } },
            { "kind": "production-produce", "metadata": { "excise_bbl": 79 } },
        ]});
        assert_eq!(batch_bbl_from_job(&job), 158);
    }

    #[test]
    fn batch_bbl_falls_back_to_summed_excise_bbl() {
        // Seasonal-release: no batch_bbl anywhere; the produce steps'
        // excise_bbl sum IS the batch.
        let job = json!({ "steps": [
            { "kind": "production-consume", "metadata": {} },
            { "kind": "production-produce", "metadata": { "excise_bbl": 30 } },
            { "kind": "production-produce", "metadata": { "excise_bbl": 12 } },
            { "kind": "task", "metadata": { "excise_bbl": 999 } },
        ]});
        assert_eq!(batch_bbl_from_job(&job), 42);
    }

    #[test]
    fn batch_bbl_zero_when_job_states_neither() {
        let job = json!({ "steps": [
            { "kind": "production-consume", "metadata": {} },
            { "kind": "task", "metadata": {} },
        ]});
        assert_eq!(batch_bbl_from_job(&job), 0);
        assert_eq!(batch_bbl_from_job(&json!({})), 0);
    }
}
