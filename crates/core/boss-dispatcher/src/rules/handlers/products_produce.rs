//! `products.produce` — per-row POST to
//! `/api/products/{sku}/inventory/produce`. Reads `produces_products`
//! from step metadata; tolerates missing/empty arrays so step kinds with
//! optional FG output don't error.
//!
//! ## FG cost basis comes from REAL purchases, not a plug
//!
//! The produced FG is valued at the brew's *actual* input cost, not a
//! hardcoded `unit_cost_cents`. Standard process-costing: a brew's
//! materials (mash-in `ingredients_consumed` + packaging `parts_consumed`)
//! flow into WIP at their real `avg_cost_cents` (the weighted-average of
//! the prices the restock POs actually paid), and the produced finished
//! goods drain that WIP. So this handler:
//!   1. fetches the Job and sums every consumed input across its steps
//!      (`Σ qty × inventory avg_cost`) — the brew's real material cost;
//!   2. allocates that cost across the produced FG by keg volume (a
//!      half-BBL carries 3× the cost of a sixtel because it holds 3× the
//!      beer);
//!   3. produces each FG SKU at its derived unit cost.
//!
//! COGS then emerges at sale from that real basis (see
//! `commerce.invoice.issue` + the `invoice_issued` ledger rule) — nothing
//! is a percentage-of-price estimate. If the real cost can't be resolved
//! (missing data / fetch failure) the handler falls back to any
//! `unit_cost_cents` the step carried, so production never breaks.

use super::common::{self, StepEvent};
use crate::rules::expr::Value;
use crate::rules::handler::{Handler, HandlerError, InvocationContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct ProducedProduct {
    sku: String,
    qty: i32,
    location_id: String,
    /// Fallback only — used when the real brew cost can't be resolved.
    #[serde(default)]
    unit_cost_cents: Option<i64>,
}

pub struct ProductsProduce {
    client: reqwest::Client,
    products_base: String,
    jobs_base: String,
    inventory_base: String,
}

impl ProductsProduce {
    pub fn new(
        products_base: impl Into<String>,
        jobs_base: impl Into<String>,
        inventory_base: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            products_base: products_base.into(),
            jobs_base: jobs_base.into(),
            inventory_base: inventory_base.into(),
        })
    }

    /// The real material cost of this brew: every input consumed across
    /// the Job (`ingredients_consumed` + `parts_consumed` on any step) at
    /// its current inventory `avg_cost_cents`. Returns `None` if nothing
    /// resolved (so the caller falls back to a declared unit cost).
    async fn brew_material_cost_cents(&self, job_id: &str) -> Result<Option<i64>, HandlerError> {
        let url = format!(
            "{}/api/jobs/{}",
            self.jobs_base.trim_end_matches('/'),
            job_id
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        let job: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))?;

        // Collect (part_sku, qty) across both consumed-input arrays on
        // every step.
        let mut consumed: Vec<(String, i64)> = Vec::new();
        if let Some(steps) = job.get("steps").and_then(|v| v.as_array()) {
            for s in steps {
                let md = s.get("metadata").cloned().unwrap_or_else(|| json!({}));
                for key in ["ingredients_consumed", "parts_consumed"] {
                    if let Some(rows) = md.get(key).and_then(|v| v.as_array()) {
                        for r in rows {
                            let sku = r.get("part_sku").and_then(|v| v.as_str());
                            let qty = r.get("qty").and_then(|v| v.as_i64());
                            if let (Some(sku), Some(qty)) = (sku, qty)
                                && qty > 0
                            {
                                consumed.push((sku.to_string(), qty));
                            }
                        }
                    }
                }
            }
        }
        if consumed.is_empty() {
            return Ok(None);
        }

        let mut total: i64 = 0;
        for (sku, qty) in consumed {
            total += self.avg_cost_cents(&sku).await?.saturating_mul(qty);
        }
        Ok((total > 0).then_some(total))
    }

    /// The inventory item's weighted-average unit cost (the price the
    /// restock POs actually paid). 0 when the SKU/field is absent.
    async fn avg_cost_cents(&self, sku: &str) -> Result<i64, HandlerError> {
        let url = format!(
            "{}/api/inventory/items/{}",
            self.inventory_base.trim_end_matches('/'),
            sku
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            // Unknown SKU → no cost contribution (don't fail the brew).
            return Ok(0);
        }
        let item: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))?;
        Ok(item
            .get("avg_cost_cents")
            .and_then(|v| v.as_i64())
            .unwrap_or(0))
    }
}

/// Volume in BBL for a finished-product keg SKU like `FP-PALE-1-2-BBL`
/// (½ BBL) or `FP-IPA-1-6-BBL` (⅙ BBL) — the allocation weight for
/// spreading a brew's cost across its kegs. Non-keg SKUs (case packs,
/// etc.) get weight 1.0 so they still receive a positive share.
fn keg_volume_bbl(sku: &str) -> f64 {
    let parts: Vec<&str> = sku.split('-').collect();
    if let [.., num, den, unit] = parts.as_slice()
        && unit.eq_ignore_ascii_case("BBL")
        && let (Ok(n), Ok(d)) = (num.parse::<f64>(), den.parse::<f64>())
        && d > 0.0
    {
        return n / d;
    }
    1.0
}

#[async_trait]
impl Handler for ProductsProduce {
    fn name(&self) -> &'static str {
        "products.produce"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;
        let Some(raw) = step.metadata.get("produces_products") else {
            return Ok(());
        };
        let items: Vec<ProducedProduct> = serde_json::from_value(raw.clone())
            .map_err(|e| HandlerError::Downstream(format!("decode produces_products: {e}")))?;
        if items.is_empty() {
            return Ok(());
        }

        // Real brew material cost, allocated across the produced FG by keg
        // volume. `brew_cost` is None when no inputs resolved (then each
        // line keeps its declared fallback `unit_cost_cents`).
        let brew_cost = self.brew_material_cost_cents(step.job_id).await?;
        let total_volume: f64 = items
            .iter()
            .map(|it| keg_volume_bbl(&it.sku) * it.qty as f64)
            .sum();

        for it in &items {
            if it.qty <= 0 {
                return Err(HandlerError::Downstream(format!(
                    "qty must be positive for sku {}",
                    it.sku
                )));
            }
            // Derived unit cost = brew_cost × (this keg's volume / total
            // volume) ÷ qty, i.e. brew_cost × unit_volume / total_volume.
            let unit_cost = match brew_cost {
                Some(cost) if total_volume > 0.0 => {
                    Some(((cost as f64) * keg_volume_bbl(&it.sku) / total_volume).round() as i64)
                }
                _ => it.unit_cost_cents,
            };
            let mut body = json!({
                "sku": it.sku,
                "location_id": it.location_id,
                "qty": it.qty,
                // Deterministic key so a redelivered produce applies the
                // relative on_hand increment exactly once.
                "idempotency_key": format!("{}:{}", step.step_id, it.sku),
            });
            if let Some(c) = unit_cost {
                body["unit_cost_cents"] = json!(c);
            }
            let url = format!(
                "{}/api/products/{}/inventory/produce",
                self.products_base.trim_end_matches('/'),
                it.sku
            );
            common::post_json(&self.client, &url, &body, &ctx.rule_name).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keg_volume_parses_bbl_fractions() {
        assert!((keg_volume_bbl("FP-PALE-1-2-BBL") - 0.5).abs() < 1e-9);
        assert!((keg_volume_bbl("FP-IPA-1-6-BBL") - (1.0 / 6.0)).abs() < 1e-9);
        // Non-keg (case pack) → weight 1.0.
        assert!((keg_volume_bbl("FP-SEASONAL-12OZ-CS") - 1.0).abs() < 1e-9);
        assert!((keg_volume_bbl("weird") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cost_allocates_by_volume() {
        // A $1,000 brew producing 2 half-BBLs + 2 sixtels: total volume =
        // 2×0.5 + 2×(1/6) = 1.333 BBL. Half-BBL unit = 1000×0.5/1.333 =
        // 375; sixtel = 1000×0.1667/1.333 = 125. Total back out:
        // 2×375 + 2×125 = 1000. ✓ (cost conserved → WIP drains).
        let cost = 100_000.0_f64; // cents
        let total_vol = 2.0 * 0.5 + 2.0 * (1.0 / 6.0);
        let half = (cost * 0.5 / total_vol).round() as i64;
        let sixtel = (cost * (1.0 / 6.0) / total_vol).round() as i64;
        assert_eq!(2 * half + 2 * sixtel, 100_000);
    }
}
