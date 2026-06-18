//! Port (trait) for the products catalog + per-location inventory.
//! Adapters: PgProducts (postgres) + InMemoryProducts (tests).

use async_trait::async_trait;

use crate::types::{Product, ProductInventory};

/// GL leg an inventory delta produced — `None` when the call had
/// no cost basis (zero-cost row, missing `unit_cost_cents`, etc.).
/// The HTTP handler emits a NATS event whose payload IS this value,
/// so the `gl_fact_projection_rules` row reproduces the same
/// `(source_table, source_id)` financial_facts row on rebuild.
#[derive(Debug, Clone)]
pub struct GlMove {
    pub source_id: String,
    pub happened_on: chrono::NaiveDate,
    /// Full passthrough payload — already contains `source_id`
    /// and `happened_on` at the keys the projection rule pointers
    /// read from (`/source_id`, `/happened_on`).
    pub payload: serde_json::Value,
}

/// Result of a `produce` or `consume` call: the new inventory row
/// plus the optional GL move the adapter wrote inside the same tx.
#[derive(Debug, Clone)]
pub struct InventoryDeltaResult {
    pub inventory: ProductInventory,
    pub gl_move: Option<GlMove>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProductsError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

#[async_trait]
pub trait ProductsRepository: Send + Sync {
    /// All catalog rows. `active_only=true` filters out retired SKUs.
    async fn list_products(&self, active_only: bool) -> Result<Vec<Product>, ProductsError>;

    /// One catalog row. Returns NotFound if the SKU isn't registered.
    async fn get_product(&self, sku: &str) -> Result<Option<Product>, ProductsError>;

    /// Upsert by SKU (idempotent on the natural key). Used by the
    /// brewery seed loader and the future authoring HTTP path.
    async fn upsert_product(&self, product: &Product) -> Result<(), ProductsError>;

    /// Per-location rows for one SKU.
    async fn inventory_for(&self, sku: &str) -> Result<Vec<ProductInventory>, ProductsError>;

    /// Upsert one (sku, location) row. Production / sale side-effect
    /// handlers call this with delta-applied counts; the table holds
    /// absolute state, last-write-wins.
    async fn upsert_inventory(&self, row: &ProductInventory) -> Result<(), ProductsError>;

    /// Atomic opening-balance / adjustment JE for FG inventory
    /// changes that don't already pair with a produce / consume
    /// fact. Used by `PUT /api/products/{sku}/inventory` (seed-
    /// side opening balance, DR 1320 / CR 3000 sized at
    /// qty × production_cost_cents) and the symmetric
    /// brewery_data_seed external call. Sibling to
    /// `InventoryRepository::record_inventory_je`; identical
    /// shape so cross-adapter callers stay consistent.
    /// Idempotent on the `(kind, source_table, source_id)`
    /// unique key, so the same opening row re-applied is a
    /// no-op. Returns the canonical fact_id.
    async fn record_inventory_je(
        &self,
        total_cost_cents: i64,
        debit_account: &str,
        credit_account: &str,
        memo: &str,
        source_table: &str,
        source_id: &str,
        happened_on: chrono::NaiveDate,
    ) -> Result<uuid::Uuid, ProductsError>;

    /// Increment on_hand for (sku, location) by `qty`. Inserts the
    /// row if missing (starting from `on_hand = qty`). Used by
    /// production-side handlers (morning-brew packaging step).
    /// Returns the new absolute on_hand so the caller can echo it
    /// in audit_log.
    ///
    /// When `unit_cost_cents` is `Some(_)`, the adapter folds it
    /// into the row's weighted moving-average `production_cost_cents`:
    ///   new_avg = (old_avg × old_on_hand + unit_cost × qty)
    ///             / (old_on_hand + qty)
    /// `None` leaves the cost basis unchanged — used by callers
    /// that don't yet carry cost data. Model B's WIP→FG cost
    /// transfer relies on `unit_cost_cents` being present so the
    /// FG row's cost basis stays current.
    async fn produce(
        &self,
        sku: &str,
        location_id: &str,
        qty: i32,
        unit_cost_cents: Option<i64>,
        now: chrono::DateTime<chrono::Utc>,
        source_id: String,
    ) -> Result<InventoryDeltaResult, ProductsError>;

    /// Decrement on_hand for (sku, location) by `qty`. Errors if
    /// the row doesn't exist or `on_hand < qty` — finished-product
    /// inventory should never go negative; if it would, the sale
    /// step is being walked out of order. Returns the new absolute
    /// on_hand. `production_cost_cents` on the returned row is the
    /// per-unit cost basis the caller uses to size the
    /// `finance.cogs.recognized` JE.
    /// Decrement on_hand for `(sku, location)` by `qty` and emit
    /// the matching COGS recognition. `revenue_category` (optional)
    /// is propagated to the `finance.cogs.recognized` payload so
    /// per-category gross margin rolls up exactly; `None` preserves
    /// the prior pro-rated rollup behavior.
    async fn consume(
        &self,
        sku: &str,
        location_id: &str,
        qty: i32,
        revenue_category: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
        source_id: String,
    ) -> Result<InventoryDeltaResult, ProductsError>;
}
