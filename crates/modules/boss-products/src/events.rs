//! Domain event subjects for products operations.
//!
//! Pattern matches the boss-jobs / boss-inventory state-event split:
//! state events carry full row state for the audit_log → projection
//! rebuild path; transition events fire on lifecycle changes for
//! downstream consumers + fact projection.

/// Full Product row state on create or update. Idempotent —
/// re-emission of the same SKU UPSERTs the catalog row.
pub const PRODUCT_UPSERTED: &str = "products.product.upserted";

/// Full ProductInventory row state on (sku, location) upsert.
/// Production side-effects + sale consume side-effects emit this.
pub const PRODUCT_INVENTORY_UPSERTED: &str = "products.inventory.upserted";

/// Auditable WIP→FG cost transfer (DR 1320 / CR 1310). Emitted
/// only when `produce` runs with a positive unit cost. Payload
/// carries `source_id` + `happened_on` so the
/// `gl_fact_projection_rules` row projects it back to a
/// `finance.inventory.transferred` row with the same canonical
/// `(source_table, source_id)` as the live insert — bundle
/// export + rebuild reproduces the GL identically.
pub const PRODUCT_PRODUCED: &str = "products.produced";

/// Auditable FG→COGS recognition (DR 5100 / CR 1320). Emitted
/// only when `consume` runs against an FG row with a positive
/// running cost basis. Symmetric to [`PRODUCT_PRODUCED`] — same
/// projection-rule mechanism, lands as `finance.cogs.recognized`.
pub const PRODUCT_CONSUMED: &str = "products.consumed";

/// Manual/atomic inventory value movement — the SAME kind the ledger
/// movement endpoint emits (same projection rule on rebuild). Emitted
/// by put_inventory when its atomic FG opening JE actually inserts
/// the fact (payload verbatim).
pub const LEDGER_INVENTORY_TRANSFERRED: &str = "ledger.inventory.transferred";
