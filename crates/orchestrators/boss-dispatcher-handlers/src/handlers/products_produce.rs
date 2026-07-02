//! `products.produce` — per-row POST to
//! `/api/products/{sku}/inventory/produce`. Reads `produces_products`
//! from step metadata; tolerates missing/empty arrays so step kinds with
//! optional FG output don't error.
//!
//! ## FG cost basis: drain the WIP that was actually capitalized
//!
//! A brew is a process-costing flow: raw materials are consumed into WIP
//! (DR 1310) at the mash-in step (the shared *joint* cost), packaging
//! materials are consumed into WIP at each packaging step (separable,
//! per-format), and the finished goods produced here drain WIP (CR 1310 /
//! DR 1320). For the books to balance, the produce must drain **exactly
//! what consume capitalized** — otherwise WIP drifts.
//!
//! Two failure modes the naive "re-fetch current avg_cost" basis had, both
//! fixed here:
//!   1. **Double-drain.** A brew mashes once but packages per-format
//!      (`package-half` + `package-sixtel`); summing the whole Job's inputs
//!      and allocating the total across *one* produce step's output drained
//!      the mash once per format. The joint mash is now **split by volume**
//!      across the formats that actually package.
//!   2. **Avg-cost drift.** Re-reading `avg_cost` at packaging time values
//!      the drain differently from the consume-time debit when a restock
//!      lands in between. The joint mash is now drained at its **real
//!      capitalized cost**, read back from the ledger's DR-1310 facts.
//!
//! So the `drain-actual-wip` basis values this produce step's drain as:
//!   - its **own packaging legs** (consumed on this step) at current
//!     `avg_cost`, plus its **own absorbed overhead** (if stamped) at the
//!     stamped amounts — exactly what the sibling
//!     `inventory.parts.consume` side effect will debit moments later,
//!     so they net to zero; plus
//!   - its **share of the joint mash**, `mash × (this format's packaged
//!     volume / Σ volume of every format that packages)`, where `mash` is
//!     the real DR-1310 cost the mash-in legs capitalized — **raw
//!     materials plus any production overhead absorbed at the mash step**
//!     (direct labor, process utilities, production depreciation; see
//!     `overhead_source_ids`) — summed from the ledger. A format that
//!     skips (oversupplied) is excluded from the denominator, so the
//!     packaged formats absorb its mash share.
//!
//! That total is allocated across the produced FG by keg volume and posted
//! as each line's `unit_cost_cents`. COGS then emerges at sale from that
//! real basis (see `commerce.invoice.issue` + the `invoice_issued` ledger
//! rule) — nothing is a percentage-of-price estimate.
//!
//! The basis is **selected by data** (the `cost_basis` arg in
//! `infra/dispatcher/rules.toml`); code provides the named bases. The
//! legacy `current-avg-cost` basis (whole-Job inputs at current avg) stays
//! reachable as a data-selected basis. A failed **or partial** ledger
//! read is NOT silently degraded around: the handler errs, the event
//! NAKs, and the drain retries once the facts land. Draining short would
//! bake the shortfall into the FG cost basis permanently — rebuild
//! replays the recorded produce fact verbatim and never re-runs this
//! computation — so a loud retry (dead-letter if the facts never come)
//! is the only path that conserves WIP.

use super::common::{self, StepEvent};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value as ExprValue;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use serde::Deserialize;
use serde_json::{Value, json};
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
    ledger_base: String,
}

impl ProductsProduce {
    pub fn new(
        products_base: impl Into<String>,
        jobs_base: impl Into<String>,
        inventory_base: impl Into<String>,
        ledger_base: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            products_base: products_base.into(),
            jobs_base: jobs_base.into(),
            inventory_base: inventory_base.into(),
            ledger_base: ledger_base.into(),
        })
    }

    /// GET the Job (with its steps) — both cost bases read its consumed
    /// inputs.
    async fn fetch_job(&self, job_id: &str) -> Result<Value, HandlerError> {
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
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))
    }

    /// `drain-actual-wip` (default). The WIP this produce step should
    /// drain: its own packaging at current `avg_cost` + its own absorbed
    /// overhead (if stamped) + its volume-share of the brew's real
    /// capitalized mash cost (read from the ledger). Returns `None` only
    /// when the job carries no steps to reconstruct from — the caller
    /// then falls back to the current-avg basis. Ledger-read failures
    /// propagate `Err` so the event NAKs and retries: draining short
    /// would bake the shortfall into the FG cost basis permanently,
    /// because rebuild replays the recorded produce fact verbatim and
    /// never re-runs this computation.
    async fn drain_actual_wip_cents(
        &self,
        step: &StepEvent<'_>,
        job: &Value,
    ) -> Result<Option<i64>, HandlerError> {
        let steps = match job.get("steps").and_then(|v| v.as_array()) {
            Some(s) => s.as_slice(),
            None => return Ok(None),
        };

        // Own packaging legs (consumed on THIS produce step) at current
        // avg_cost. Read from the same step-event metadata the sibling
        // parts-consume side effect reads, so this matches exactly what it
        // will debit moments later — the two net to zero.
        let mut own_cost: i64 = 0;
        for (sku, qty) in legs_from_meta_map(step.metadata) {
            own_cost =
                own_cost.saturating_add(self.avg_cost_cents(&sku).await?.saturating_mul(qty));
        }
        // This step's OWN absorbed overhead (e.g. packaging labor stamped
        // on a produce step) — the produce step's to drain, exactly like
        // its own packaging legs. Valued from the same metadata rows the
        // sibling parts-consume side effect absorbs moments later on this
        // same event (it runs after this handler in the rule's do-list),
        // so the two net to zero. The ledger can't be asked here: those
        // facts land after this drain computes.
        for ab in common::overhead_absorbed(step.metadata, step.step_id) {
            own_cost = own_cost.saturating_add(ab.amount_cents);
        }

        // Shared mash: the real DR-1310 cost its consume legs capitalized
        // — raw materials plus any production overhead absorbed at those
        // same mash steps (direct labor / utilities / depreciation
        // drivers). Both are DR-1310 facts; summing both is what drains
        // the *full* WIP a brew capitalized, so the drivers reach FG/COGS
        // instead of stranding in WIP. The two sums are independent
        // read-only aggregates — issue them concurrently.
        //
        // Failure policy: a failed OR partial read (see the matched-count
        // check in `ledger_transferred_sum`) propagates `Err` → NAK →
        // redelivery, converging once the facts land. Both sums run
        // before any side effect, so the retry is clean. The old
        // behavior — draining raw-only when the overhead read failed —
        // permanently understated the FG basis and stranded the absorbed
        // overhead in 1310: rebuild replays recorded facts, it never
        // re-runs this drain, so nothing downstream could heal it.
        let mash_ids = mash_source_ids(steps, step.step_id);
        let overhead_ids = overhead_source_ids(steps, step.step_id);
        let (raw_mash_cost, overhead_cost) = {
            let (raw, overhead) = tokio::join!(
                self.ledger_transferred_sum("inventory_consume", mash_ids),
                self.ledger_transferred_sum("ledger_overhead_absorbed", overhead_ids),
            );
            (raw?, overhead?)
        };
        let mash_cost = raw_mash_cost.saturating_add(overhead_cost);

        // This format's share of the joint mash, spread across every
        // format that actually packages.
        let this_vol = this_step_volume(steps, step.step_id);
        let denom = packaged_volume_denominator(steps);
        let mash_share = mash_share_cents(mash_cost, this_vol, denom);

        Ok(Some(own_cost.saturating_add(mash_share)))
    }

    /// Legacy `current-avg-cost` basis: every consumed input across the
    /// whole Job at its current inventory `avg_cost`. Kept as a named
    /// basis and as the drain-actual fallback. Returns `None` when nothing
    /// resolved (caller then keeps each line's declared fallback cost).
    async fn current_avg_brew_cost(&self, job: &Value) -> Result<Option<i64>, HandlerError> {
        let mut consumed: Vec<(String, i64)> = Vec::new();
        if let Some(steps) = job.get("steps").and_then(|v| v.as_array()) {
            for s in steps {
                if let Some(md) = s.get("metadata").and_then(|v| v.as_object()) {
                    consumed.extend(legs_from_meta_map(md));
                }
            }
        }
        if consumed.is_empty() {
            return Ok(None);
        }
        let mut total: i64 = 0;
        for (sku, qty) in consumed {
            total = total.saturating_add(self.avg_cost_cents(&sku).await?.saturating_mul(qty));
        }
        Ok((total > 0).then_some(total))
    }

    /// Sum the real DR-1310 cost the given facts capitalized into WIP,
    /// via the ledger's read-only facts-sum endpoint. Both the raw
    /// `finance.inventory.transferred` / `inventory_consume` legs (each
    /// `total_cost_cents = qty × avg_cost_at_consume`) and the absorbed
    /// `finance.inventory.transferred` / `ledger_overhead_absorbed`
    /// overhead drivers are summed this way — only the `source_table`
    /// and the source-id shape differ.
    ///
    /// Every requested leg must have landed: the endpoint's `matched`
    /// count is compared against the (deduped) request, and a shortfall
    /// is an error, not a smaller sum. A short match means the step
    /// metadata names facts the ledger doesn't hold yet — typically the
    /// consume/absorb side effect is still in NAK-redelivery — and
    /// summing anyway would silently under-drain WIP into a cost basis
    /// that rebuild then reproduces forever. Err → NAK → retry converges
    /// once the facts land; a fact that never lands dead-letters loudly.
    async fn ledger_transferred_sum(
        &self,
        source_table: &str,
        mut source_ids: Vec<String>,
    ) -> Result<i64, HandlerError> {
        // Unique ids: `source_id = ANY(..)` can't double-count rows, and
        // `matched` (a row count) is compared against the request set.
        source_ids.sort();
        source_ids.dedup();
        if source_ids.is_empty() {
            return Ok(0);
        }
        let url = format!(
            "{}/api/ledger/financial-facts/sum",
            self.ledger_base.trim_end_matches('/')
        );
        let resp = self
            .client
            .post(&url)
            .json(&json!({
                "kind": "finance.inventory.transferred",
                "source_table": source_table,
                "source_ids": source_ids,
                "debit_account": "1310",
            }))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "POST {url} returned {status}: {body}"
            )));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url} not JSON: {e}")))?;
        let matched = v.get("matched").and_then(|x| x.as_i64()).unwrap_or(-1);
        if matched != source_ids.len() as i64 {
            return Err(HandlerError::Downstream(format!(
                "POST {url}: {matched} of {} requested {source_table} facts matched — \
                 refusing to drain short (facts not landed yet, or source-id drift)",
                source_ids.len()
            )));
        }
        Ok(v.get("total_cost_cents")
            .and_then(|x| x.as_i64())
            .unwrap_or(0))
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
        let item: Value = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))?;
        Ok(item
            .get("avg_cost_cents")
            .and_then(|v| v.as_i64())
            .unwrap_or(0))
    }
}

/// Consumed legs `(part_sku, qty)` from a step's metadata object — the
/// `ingredients_consumed` (mash) + `parts_consumed` (packaging) arrays.
/// Non-positive quantities are dropped.
fn legs_from_meta_map(md: &serde_json::Map<String, Value>) -> Vec<(String, i64)> {
    let mut legs = Vec::new();
    for key in ["ingredients_consumed", "parts_consumed"] {
        if let Some(rows) = md.get(key).and_then(|v| v.as_array()) {
            for r in rows {
                if let (Some(sku), Some(qty)) = (
                    r.get("part_sku").and_then(|v| v.as_str()),
                    r.get("qty").and_then(|v| v.as_i64()),
                ) && qty > 0
                {
                    legs.push((sku.to_string(), qty));
                }
            }
        }
    }
    legs
}

/// Whether a step yields finished goods — a non-empty `produces_products`
/// array. This is the *property* that marks a packaging/produce step: its
/// own consumed inputs are separable (that step drains them), so they are
/// not part of the brew's joint mash. Dispatching on this metadata field
/// keeps step-kind names out of core code (they are registry data).
fn produces_products(step: &Value) -> bool {
    step.get("metadata")
        .and_then(|m| m.get("produces_products"))
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
}

/// The brew's **joint** (shared-mash) steps relative to the produce step
/// `this_step_id`: every completed step that yields no finished goods,
/// except this step itself. This one predicate IS the definition of a
/// joint mash leg — `mash_source_ids` and `overhead_source_ids` both
/// derive from it, so raw materials and absorbed overhead always drain
/// over the same step set. Only completed steps count: a pending or
/// skipped consume step has fired no side effects, so it has no facts
/// to drain — and expecting its facts would wedge the drain's
/// matched-count check against rows that never come.
fn joint_mash_steps<'a>(
    steps: &'a [Value],
    this_step_id: &'a str,
) -> impl Iterator<Item = (&'a str, &'a serde_json::Map<String, Value>)> {
    steps.iter().filter_map(move |s| {
        let step_id = s.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if step_id == this_step_id || produces_products(s) {
            return None;
        }
        if s.get("status").and_then(|v| v.as_str()) != Some("completed") {
            return None;
        }
        s.get("metadata")
            .and_then(|v| v.as_object())
            .map(|md| (step_id, md))
    })
}

/// The `financial_facts.source_id`s of the brew's **joint** consume legs,
/// relative to the produce step `this_step_id`: every leg consumed on a
/// joint mash step (see `joint_mash_steps`). A step that produces goods
/// drains its own consumed inputs — this step's packaging is valued
/// separately, and a sibling produce step's packaging is the sibling's
/// to drain. Each consume leg's fact is keyed `"{step_id}:{part_sku}"`
/// (the consume side effect's idempotency key).
fn mash_source_ids(steps: &[Value], this_step_id: &str) -> Vec<String> {
    joint_mash_steps(steps, this_step_id)
        .flat_map(|(step_id, md)| {
            legs_from_meta_map(md)
                .into_iter()
                .map(move |(sku, _qty)| format!("{step_id}:{sku}"))
        })
        .collect()
}

/// The `financial_facts.source_id`s of the production overhead absorbed
/// at the brew's joint mash steps (see `joint_mash_steps`), relative to
/// the produce step `this_step_id`. The ids reconstruct the
/// `overhead_absorbed` drivers the `inventory.parts.consume` side effect
/// capitalized — derived from the SAME parse the absorb side posts from
/// (`common::overhead_absorbed`, aggregation and all) and keyed by
/// `common::overhead_source_id`, under
/// `source_table=ledger_overhead_absorbed`. Summed into the mash cost so
/// the absorbed overhead drains WIP → FG → COGS alongside the raw
/// materials instead of stranding in WIP.
fn overhead_source_ids(steps: &[Value], this_step_id: &str) -> Vec<String> {
    joint_mash_steps(steps, this_step_id)
        .flat_map(|(step_id, md)| {
            common::overhead_absorbed(md, step_id)
                .into_iter()
                .map(move |ab| common::overhead_source_id(step_id, &ab.credit_account))
        })
        .collect()
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

/// BBL a produce step yields = Σ `keg_volume × qty` over its
/// `produces_products`.
fn produce_step_volume(step: &Value) -> f64 {
    step.get("metadata")
        .and_then(|m| m.get("produces_products"))
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    let sku = r.get("sku").and_then(|v| v.as_str()).unwrap_or("");
                    let qty = r.get("qty").and_then(|v| v.as_i64()).unwrap_or(0).max(0);
                    keg_volume_bbl(sku) * qty as f64
                })
                .sum()
        })
        .unwrap_or(0.0)
}

/// The packaged volume of the produce step `this_step_id`.
fn this_step_volume(steps: &[Value], this_step_id: &str) -> f64 {
    steps
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(this_step_id))
        .map(produce_step_volume)
        .unwrap_or(0.0)
}

/// Total packaged BBL the joint mash spreads across: the summed allocated
/// volume of every produce step. `packaging.allocate` writes each format's
/// packaged keg qty onto its produce step — INCLUDING a 0 for a format it
/// skipped — so a skipped format contributes 0 volume and drops out of the
/// denominator, and the packaged formats absorb its mash share into their
/// COGS. Non-producing steps carry no `produces_products` and contribute 0.
///
/// This reads the allocated quantities directly (data), replacing the old
/// coupling to a per-format demand-gate's `outcome` — which broke once the
/// packaging split moved to the single allocation step, and which the
/// lagging-`status` variant of got wrong (under-draining WIP when a skipped
/// format hadn't flipped to `skipped` yet).
fn packaged_volume_denominator(steps: &[Value]) -> f64 {
    steps.iter().map(produce_step_volume).sum()
}

/// This format's share of the joint mash cost: `mash × this_vol /
/// denom_vol`, rounded to cents. Zero when there's no packaged volume
/// (never divides by zero).
fn mash_share_cents(mash_cost: i64, this_vol: f64, denom_vol: f64) -> i64 {
    if denom_vol <= 0.0 {
        return 0;
    }
    ((mash_cost as f64) * this_vol / denom_vol).round() as i64
}

#[async_trait]
impl Handler for ProductsProduce {
    fn name(&self) -> &'static str {
        "products.produce"
    }

    async fn invoke(
        &self,
        args: &[(String, ExprValue)],
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

        // Cost basis is selected by data (rules.toml `cost_basis` arg);
        // code provides the named bases. Default is drain-actual-wip.
        let basis = arg_string(args, "cost_basis").unwrap_or("drain-actual-wip");

        // Fetch the Job once — both bases read its steps' consumed inputs.
        let job = self.fetch_job(step.job_id).await?;

        let cost = match basis {
            "current-avg-cost" => self.current_avg_brew_cost(&job).await?,
            // drain-actual-wip (default): drain exactly what consume
            // capitalized. A failed/partial ledger read propagates Err
            // (NAK → retry once the facts land); the current-avg
            // fallback below covers only the structural no-steps case.
            _ => match self.drain_actual_wip_cents(&step, &job).await? {
                Some(c) => Some(c),
                None => self.current_avg_brew_cost(&job).await?,
            },
        };

        // Allocate the drain across the produced FG by keg volume. `cost`
        // is None only when no inputs resolved (then each line keeps its
        // declared fallback `unit_cost_cents`).
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
            // Derived unit cost = cost × (this keg's volume / total
            // volume) ÷ qty, i.e. cost × unit_volume / total_volume.
            let unit_cost = match cost {
                Some(c) if total_volume > 0.0 => {
                    Some(((c as f64) * keg_volume_bbl(&it.sku) / total_volume).round() as i64)
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

    #[test]
    fn legs_from_meta_map_reads_both_arrays_and_skips_nonpositive() {
        let md = json!({
            "ingredients_consumed": [{ "part_sku": "ING-A", "qty": 5 }],
            "parts_consumed": [
                { "part_sku": "PKG-B", "qty": 2 },
                { "part_sku": "PKG-C", "qty": 0 }
            ]
        });
        let legs = legs_from_meta_map(md.as_object().unwrap());
        assert_eq!(
            legs,
            vec![("ING-A".to_string(), 5), ("PKG-B".to_string(), 2)]
        );
    }

    #[test]
    fn produce_step_volume_sums_keg_volume() {
        let step = json!({
            "kind": "production-produce",
            "metadata": { "produces_products": [
                { "sku": "FP-PALE-1-2-BBL", "qty": 210 }, // 0.5 × 210 = 105
                { "sku": "FP-PALE-1-6-BBL", "qty": 6 }    // 1/6 × 6 = 1
            ]}
        });
        assert!((produce_step_volume(&step) - 106.0).abs() < 1e-6);
    }

    #[test]
    fn denominator_excludes_zero_qty_skipped_formats() {
        // packaging.allocate stamps qty 0 on a format it skips, so that produce
        // step contributes 0 volume — the packaged format absorbs the whole
        // batch's WIP. Read straight from the allocated quantities: no
        // demand-gate lookup, no lagging-`status` race.
        let steps = vec![
            json!({ "id": "pkg-half", "kind": "production-produce",
                    "metadata": { "produces_products": [{ "sku": "FP-PALE-1-2-BBL", "qty": 210 }] } }), // 105
            json!({ "id": "pkg-sixtel", "kind": "production-produce",
                    "metadata": { "produces_products": [{ "sku": "FP-PALE-1-6-BBL", "qty": 0 }] } }), // skipped → 0
        ];
        assert!((packaged_volume_denominator(&steps) - 105.0).abs() < 1e-6);
        // Both packaged → both counted (the demand-driven split).
        let mut both = steps.clone();
        both[1] = json!({ "id": "pkg-sixtel", "kind": "production-produce",
                          "metadata": { "produces_products": [{ "sku": "FP-PALE-1-6-BBL", "qty": 315 }] } }); // 52.5
        assert!((packaged_volume_denominator(&both) - 157.5).abs() < 1e-6); // 105 + 52.5
    }

    #[test]
    fn mash_legs_separated_from_own_and_siblings() {
        let steps = vec![
            json!({ "id": "mash", "kind": "production-consume", "status": "completed", "metadata": {
                "ingredients_consumed": [
                    { "part_sku": "ING-MALT-2ROW-50", "qty": 196 },
                    { "part_sku": "ING-HOPS-CASCADE-44", "qty": 4 }
                ]
            }}),
            json!({ "id": "pkg-half", "kind": "production-produce", "status": "active", "metadata": {
                "parts_consumed": [{ "part_sku": "PKG-KEG-FILL-HALF-BBL", "qty": 210 }],
                "produces_products": [{ "sku": "FP-PALE-1-2-BBL", "qty": 210 }]
            }}),
            json!({ "id": "pkg-sixtel", "kind": "production-produce", "status": "active", "metadata": {
                "parts_consumed": [{ "part_sku": "PKG-KEG-FILL-SIXTEL", "qty": 315 }],
                "produces_products": [{ "sku": "FP-PALE-1-6-BBL", "qty": 315 }]
            }}),
        ];
        // From pkg-half: mash legs are the two mash-in ingredients; the
        // sibling pkg-sixtel's packaging is excluded; pkg-half's own
        // packaging is not a mash leg.
        let mut ids = mash_source_ids(&steps, "pkg-half");
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "mash:ING-HOPS-CASCADE-44".to_string(),
                "mash:ING-MALT-2ROW-50".to_string(),
            ]
        );
    }

    #[test]
    fn overhead_ids_reconstruct_absorption_keys_for_mash_steps_only() {
        let steps = vec![
            json!({ "id": "mash", "kind": "production-consume", "status": "completed", "metadata": {
                "ingredients_consumed": [{ "part_sku": "ING-MALT-2ROW-50", "qty": 196 }],
                "overhead_absorbed": [
                    { "amount_cents": 578_280, "credit_account": "6100" },
                    { "amount_cents": 88_480,  "credit_account": "6300" },
                    { "amount_cents": 135_880, "credit_account": "6900" }
                ]
            }}),
            json!({ "id": "pkg-half", "kind": "production-produce", "status": "active", "metadata": {
                "produces_products": [{ "sku": "FP-PALE-1-2-BBL", "qty": 210 }],
                // A producing step's own absorption is the produce
                // step's to drain (added into own_cost) — never a
                // joint mash leg.
                "overhead_absorbed": [{ "amount_cents": 999, "credit_account": "6100" }]
            }}),
        ];
        // Keys mirror the absorption endpoint's source_id, mash steps only.
        let mut ids = overhead_source_ids(&steps, "pkg-half");
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "overhead-absorbed@mash:6100".to_string(),
                "overhead-absorbed@mash:6300".to_string(),
                "overhead-absorbed@mash:6900".to_string(),
            ]
        );
    }

    #[test]
    fn joint_mash_excludes_steps_that_have_not_completed() {
        // A pending/skipped consume step has fired no side effects, so
        // it has no facts to drain — including it would make the drain
        // expect facts that never land (and wedge the matched-count
        // check). Only completed steps are joint mash legs.
        let steps = vec![
            json!({ "id": "mash", "kind": "production-consume", "status": "completed", "metadata": {
                "ingredients_consumed": [{ "part_sku": "ING-A", "qty": 5 }],
                "overhead_absorbed": [{ "amount_cents": 100, "credit_account": "6100" }]
            }}),
            json!({ "id": "late-adds", "kind": "production-consume", "status": "pending", "metadata": {
                "ingredients_consumed": [{ "part_sku": "ING-B", "qty": 2 }],
                "overhead_absorbed": [{ "amount_cents": 50, "credit_account": "6300" }]
            }}),
            json!({ "id": "pkg", "kind": "production-produce", "status": "active", "metadata": {
                "produces_products": [{ "sku": "FP-PALE-1-2-BBL", "qty": 210 }]
            }}),
        ];
        assert_eq!(mash_source_ids(&steps, "pkg"), vec!["mash:ING-A".to_string()]);
        assert_eq!(
            overhead_source_ids(&steps, "pkg"),
            vec!["overhead-absorbed@mash:6100".to_string()]
        );
    }

    #[test]
    fn mash_share_splits_then_absorbs_on_skip() {
        // Two formats both packaging equal volume → split 50/50.
        assert_eq!(mash_share_cents(1000, 105.0, 210.0), 500);
        // Sibling skipped → denom = this format only → absorbs 100%.
        assert_eq!(mash_share_cents(1000, 105.0, 105.0), 1000);
        // Guard: zero packaged volume → no share (never divides by zero).
        assert_eq!(mash_share_cents(1000, 0.0, 0.0), 0);
    }
}
