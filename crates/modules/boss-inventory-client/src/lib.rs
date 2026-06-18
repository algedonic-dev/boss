//! HTTP client port for reaching the `boss-inventory` service.
//!
//! Currently exposes one question: "given a list of part SKUs, what
//! are their stock levels?" for the device-insights projection's
//! "likely-failure parts on-hand" section (operations-needs session
//! 3, E4). The caller (assets) joins this against the device model's
//! spare-parts BOM returned by the catalog client.

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};
use serde::{Deserialize, Serialize};

/// Service-name marker for the shared [`HttpClientError`]. Keeps the
/// `Display` text reading `"inventory service unreachable: …"`.
#[derive(Debug)]
pub struct Inventory;
impl ServiceLabel for Inventory {
    const NAME: &'static str = "inventory";
}

/// Transport error for the Inventory client. Alias of the shared
/// [`HttpClientError`] so existing constructors and matches keep
/// compiling.
pub type InventoryClientError = HttpClientError<Inventory>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartStockLevel {
    pub part_sku: String,
    pub on_hand: u32,
    pub allocated: u32,
    pub available: u32,
    pub reorder_point: u32,
    pub below_reorder: bool,
}

#[async_trait]
pub trait InventoryClient: Send + Sync {
    /// Stock levels for the requested SKUs. SKUs absent from inventory
    /// are omitted — callers detect "part not stocked" by diffing the
    /// requested list against the returned list.
    async fn parts_stock_by_skus(
        &self,
        skus: &[String],
    ) -> Result<Vec<PartStockLevel>, InventoryClientError>;
}

pub struct ReqwestInventoryClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestInventoryClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl InventoryClient for ReqwestInventoryClient {
    async fn parts_stock_by_skus(
        &self,
        skus: &[String],
    ) -> Result<Vec<PartStockLevel>, InventoryClientError> {
        if skus.is_empty() {
            return Ok(Vec::new());
        }
        // v1: pull the full items list (small — ~500-2000 SKUs) and
        // filter client-side. If the list grows past ~10k, add a
        // `/api/inventory/items?sku_in=...` endpoint and switch.
        let url = format!("{}/api/inventory/items", self.base_url);
        let items: Vec<serde_json::Value> = http_client::get_json(&self.http, &url).await?;

        let wanted: std::collections::HashSet<&str> = skus.iter().map(|s| s.as_str()).collect();
        Ok(filter_and_project(&items, &wanted))
    }
}

/// Pure filter + project: extract the `PartStockLevel` shape for any
/// `InventoryItem` JSON row whose SKU is in the requested set.
/// Extracted so tests can pin the projection without reqwest.
pub fn filter_and_project(
    items: &[serde_json::Value],
    wanted: &std::collections::HashSet<&str>,
) -> Vec<PartStockLevel> {
    items
        .iter()
        .filter_map(|item| {
            let sku = item.get("part_sku").and_then(|v| v.as_str())?;
            if !wanted.contains(sku) {
                return None;
            }
            let on_hand = item.get("on_hand").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let allocated = item.get("allocated").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let reorder_point = item
                .get("reorder_point")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let available = on_hand.saturating_sub(allocated);
            Some(PartStockLevel {
                part_sku: sku.to_string(),
                on_hand,
                allocated,
                available,
                reorder_point,
                below_reorder: available <= reorder_point,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn items() -> Vec<serde_json::Value> {
        vec![
            json!({ "part_sku": "PART-A", "on_hand": 50, "allocated": 10, "reorder_point": 20 }),
            json!({ "part_sku": "PART-B", "on_hand": 5,  "allocated": 2,  "reorder_point": 10 }),
            json!({ "part_sku": "PART-C", "on_hand": 0,  "allocated": 0,  "reorder_point": 5 }),
        ]
    }

    #[test]
    fn filters_to_requested_skus_only() {
        let wanted: std::collections::HashSet<&str> = ["PART-A", "PART-C"].into_iter().collect();
        let rows = filter_and_project(&items(), &wanted);
        let skus: Vec<&str> = rows.iter().map(|r| r.part_sku.as_str()).collect();
        assert_eq!(skus, vec!["PART-A", "PART-C"]);
    }

    #[test]
    fn computes_available_and_below_reorder_flag() {
        let wanted: std::collections::HashSet<&str> =
            ["PART-A", "PART-B", "PART-C"].into_iter().collect();
        let rows = filter_and_project(&items(), &wanted);
        let by: std::collections::HashMap<_, _> =
            rows.iter().map(|r| (r.part_sku.as_str(), r)).collect();
        assert_eq!(by["PART-A"].available, 40);
        assert!(!by["PART-A"].below_reorder);
        assert_eq!(by["PART-B"].available, 3);
        assert!(by["PART-B"].below_reorder);
        assert_eq!(by["PART-C"].available, 0);
        assert!(by["PART-C"].below_reorder);
    }

    #[test]
    fn empty_wanted_set_yields_empty_result() {
        let wanted: std::collections::HashSet<&str> = std::collections::HashSet::new();
        assert!(filter_and_project(&items(), &wanted).is_empty());
    }
}
