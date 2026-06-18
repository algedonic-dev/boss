//! Domain types for finished products + per-location inventory.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Catalog row for a finished-product SKU. One row per
/// (product, package_unit) combination — the SKU itself encodes
/// both. Sibling to `parts` (raw inputs catalog) but tracks output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Product {
    pub sku: String,
    pub name: String,
    /// Tenant-defined family ('beer', 'cider', 'refurb-device').
    /// Free-text; the Class registry validates per-tenant.
    pub product_kind: String,
    /// Tenant-defined unit ('1/2-bbl-keg', '12oz-case', 'unit').
    pub package_unit: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Free-form metadata: abv, ibu, style, msrp_cents, ...
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    #[serde(default = "default_active")]
    pub active: bool,
}

fn default_metadata() -> serde_json::Value {
    serde_json::json!({})
}

fn default_active() -> bool {
    true
}

/// Per-location on-hand row. Mirrors `inventory_items` but keyed
/// on (sku, location) so finished-goods movement (brewhouse →
/// taproom → distributor) is visible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductInventory {
    pub product_sku: String,
    pub location_id: String,
    pub on_hand: i32,
    #[serde(default)]
    pub reserved: i32,
    /// Per-unit production cost in cents. Set on `produce` from
    /// the JobKind's authored `unit_cost_cents`; read on `consume`
    /// to size the `finance.cogs.recognized` JE. Weighted moving
    /// average — see `port::produce` for the formula.
    #[serde(default)]
    pub production_cost_cents: i64,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

/// Read-side detail shape for `GET /api/products/:sku` — the
/// catalog row plus an on-hand-by-location rollup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductDetail {
    #[serde(flatten)]
    pub product: Product,
    pub inventory: Vec<ProductInventory>,
    pub total_on_hand: i32,
}
