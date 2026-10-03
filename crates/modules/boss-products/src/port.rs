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

/// Refuse text Postgres cannot store: a NUL byte in any of `fields`
/// (TEXT rejects one). Both adapters call this before writing, so the
/// refusal is `Invalid` naming the field on each — until the
/// adapters-agree suite (backlog be459ab9) Postgres answered with its
/// encoding error as `Storage` (a 500) and the double stored the byte.
pub fn refuse_nul(fields: &[(&str, &str)]) -> Result<(), ProductsError> {
    match fields.iter().find(|(_, v)| v.contains('\0')) {
        Some((f, _)) => Err(ProductsError::Invalid(format!(
            "{f} holds a NUL byte, which cannot be stored"
        ))),
        None => Ok(()),
    }
}

/// `refuse_nul` over a catalog row, its metadata searched whole (JSONB
/// rejects a NUL in any string or key).
pub fn refuse_nul_in_product(p: &Product) -> Result<(), ProductsError> {
    fn json_holds_nul(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::String(s) => s.contains('\0'),
            serde_json::Value::Array(a) => a.iter().any(json_holds_nul),
            serde_json::Value::Object(o) => {
                o.iter().any(|(k, v)| k.contains('\0') || json_holds_nul(v))
            }
            _ => false,
        }
    }
    refuse_nul(&[
        ("sku", &p.sku),
        ("name", &p.name),
        ("product_kind", &p.product_kind),
        ("package_unit", &p.package_unit),
        ("description", p.description.as_deref().unwrap_or("")),
    ])?;
    if json_holds_nul(&p.metadata) {
        return Err(ProductsError::Invalid(
            "metadata holds a NUL byte, which cannot be stored".into(),
        ));
    }
    Ok(())
}

/// The refusal of an inventory write naming a SKU nobody registered —
/// one wording for both adapters (backlog be459ab9: Postgres answered
/// its foreign-key error as `Storage`, a 500, and the double stored the
/// row).
pub fn unregistered(sku: &str) -> ProductsError {
    ProductsError::NotFound(format!("product {sku} is not registered"))
}

/// Every port method is held to one statement across both adapters by
/// `tests/the_adapters_agree_on_the_products_store_pg.rs` (backlog
/// be459ab9). Common to every method: a NUL byte in a written text
/// argument or field is refused `Invalid` naming it, and a read keyed
/// by one is a miss; lists are in BYTE order (never the database's
/// locale, backlog 2987fb2d); a stored instant is kept to the
/// microsecond.
#[async_trait]
pub trait ProductsRepository: Send + Sync {
    /// All catalog rows, in byte order of SKU. `active_only=true`
    /// filters out retired SKUs.
    async fn list_products(&self, active_only: bool) -> Result<Vec<Product>, ProductsError>;

    /// One catalog row; `None` for a SKU nobody registered.
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

    /// Per-location rows for one SKU, in byte order of location.
    async fn inventory_for(&self, sku: &str) -> Result<Vec<ProductInventory>, ProductsError>;

    /// Upsert one (sku, location) row. Production / sale side-effect
    /// handlers call this with delta-applied counts; the table holds
    /// absolute state, last-write-wins. The stored row's
    /// `production_cost_cents` is derived (what was sent is ignored)
    /// and its `updated_at` is the stamp's instant. A SKU nobody
    /// registered is refused `NotFound` (so is a produce naming one).
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
    /// no-op (`inserted: false`). Returns the canonical fact_id
    /// (`boss_ledger::deterministic_fact_id`). A total of zero or
    /// fewer cents is refused `Invalid`.
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
    /// A costed produce replayed with the same `source_id` (a
    /// redelivered step effect) answers the current row unchanged, no
    /// GL move, and records nothing — its fact is the proof of
    /// application; the same holds for a costed `consume`.
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
