//! Port (trait) for the products catalog + per-location inventory.
//! Adapters: PgProducts (postgres) + InMemoryProducts (tests).

use async_trait::async_trait;
use boss_core::publisher::EventStamp;

use crate::types::{Product, ProductInventory};

/// GL leg an inventory delta produced — `None` when the call had
/// no cost basis (zero-cost row, missing `total_cost_cents`, etc.).
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
    /// OUTBOX (phase 2): records `products.product.upserted` in the
    /// same transaction as the row.
    async fn upsert_product(
        &self,
        product: &Product,
        stamp: &EventStamp,
    ) -> Result<(), ProductsError>;

    /// Per-location rows for one SKU.
    async fn inventory_for(&self, sku: &str) -> Result<Vec<ProductInventory>, ProductsError>;

    /// Upsert one (sku, location) row. Production / sale side-effect
    /// handlers call this with delta-applied counts; the table holds
    /// absolute state, last-write-wins.
    /// OUTBOX (phase 2): records `products.inventory.upserted` in
    /// the same transaction as the row.
    async fn upsert_inventory(
        &self,
        row: &ProductInventory,
        stamp: &EventStamp,
    ) -> Result<(), ProductsError>;

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
    /// OUTBOX (phase 2): when THIS call inserts the fact, the
    /// matching `ledger.inventory.transferred` event records in the
    /// same transaction — the emit-once-on-`inserted` contract the
    /// HTTP handler used to enforce is structural now.
    #[allow(clippy::too_many_arguments)]
    async fn record_inventory_je(
        &self,
        total_cost_cents: i64,
        debit_account: &str,
        credit_account: &str,
        memo: &str,
        source_table: &str,
        source_id: &str,
        happened_on: chrono::NaiveDate,
        stamp: &EventStamp,
    ) -> Result<crate::types::JeRecorded, ProductsError>;

    /// Increment on_hand for (sku, location) by `qty`. Inserts the
    /// row if missing (starting from `on_hand = qty`). Used by
    /// production-side handlers (morning-brew packaging step).
    /// Returns the new absolute on_hand so the caller can echo it
    /// in audit_log.
    ///
    /// When `total_cost_cents` is `Some(_)`, the adapter adds the
    /// EXACT line total onto the row's conserved `value_cents` and
    /// posts the same number as the WIP→FG transfer — the caller
    /// (the produce handler) allocated largest-remainder shares, and
    /// posting them un-rounded is what makes 1310 drain to zero
    /// (PR 6a). `None` leaves value unchanged — callers that don't
    /// carry cost data move units only. The display
    /// `production_cost_cents` is derived (value / on_hand).
    /// OUTBOX (phase 2): records `products.inventory.upserted`
    /// (post-delta row) and, when a GL move happened,
    /// `products.produced` (the fact payload verbatim) in the same
    /// transaction as the delta.
    #[allow(clippy::too_many_arguments)]
    async fn produce(
        &self,
        sku: &str,
        location_id: &str,
        qty: i32,
        total_cost_cents: Option<i64>,
        now: chrono::DateTime<chrono::Utc>,
        source_id: String,
        stamp: &EventStamp,
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
    /// OUTBOX (phase 2): symmetric to `produce` —
    /// `products.inventory.upserted` + (on a GL move)
    /// `products.consumed`, in the delta's transaction.
    #[allow(clippy::too_many_arguments)]
    async fn consume(
        &self,
        sku: &str,
        location_id: &str,
        qty: i32,
        revenue_category: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
        source_id: String,
        stamp: &EventStamp,
    ) -> Result<InventoryDeltaResult, ProductsError>;
}
