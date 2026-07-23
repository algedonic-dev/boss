//! Port (trait) for inventory storage.

use async_trait::async_trait;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, NaiveDate, Utc};

use crate::types::{
    ApAging, ConsumeApplied, InventoryItem, JeRecorded, PurchaseOrder, ReceiveApplied, Vendor,
    VendorInvoice,
};

#[derive(Debug, thiserror::Error)]
pub enum InventoryError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("insufficient stock: {0} has {1} on hand, need {2}")]
    InsufficientStock(String, u32, u32),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// A caller-supplied GL account code that the chart doesn't hold —
    /// deterministic request-data error (a seed/authoring typo), not a
    /// storage failure. The HTTP layer maps this to 422 so it reads as
    /// a client error, distinct from real 5xx storage trouble.
    #[error("invalid account: {0}")]
    InvalidAccount(String),
}

/// Persistence port for inventory tables.
///
/// **Timestamp threading.** Mutation methods come in two flavors:
/// a convenience overload that stamps `Utc::now()` server-side, and
/// an `_at` variant that takes an explicit timestamp. Handlers that
/// emit a domain event for the same mutation should use the `_at`
/// form so the projection write and the audit_log event share one
/// timestamp — required for the audit_log → projection rebuild path
/// to reproduce `created_at` exactly. See
/// `docs/design/projection-rebuilders.md`.
#[async_trait]
pub trait InventoryRepository: Send + Sync {
    async fn all_items(&self) -> Result<Vec<InventoryItem>, InventoryError>;
    async fn item_by_sku(&self, part_sku: &str) -> Result<Option<InventoryItem>, InventoryError>;
    /// Upsert an inventory item row. Used by replay seeding and by
    /// the Warehouse workbench when a new part SKU is stocked for the
    /// first time.
    async fn upsert_item(&self, item: &InventoryItem) -> Result<(), InventoryError> {
        let now = Utc::now();
        let stamp = EventStamp::new(
            "inventory",
            boss_core::actor::ActorId::Automation("platform".into()),
            now,
        );
        self.upsert_item_at(item, now, &stamp).await
    }
    /// Records `inventory.item.upserted` (the upserted row — the
    /// last-write-wins rebuild source) in the same transaction as the
    /// upsert — outbox phase 2.
    async fn upsert_item_at(
        &self,
        item: &InventoryItem,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), InventoryError>;
    async fn all_purchase_orders(&self) -> Result<Vec<PurchaseOrder>, InventoryError>;
    async fn purchase_order_by_id(&self, id: &str)
    -> Result<Option<PurchaseOrder>, InventoryError>;
    async fn consume_part(
        &self,
        part_sku: &str,
        qty: u32,
    ) -> Result<ConsumeApplied, InventoryError> {
        let source_id = format!("{}@{}", part_sku, uuid::Uuid::new_v4());
        let now = Utc::now();
        let stamp = EventStamp::new(
            "inventory",
            boss_core::actor::ActorId::Automation("platform".into()),
            now,
        );
        self.consume_part_at(part_sku, qty, now, &source_id, &stamp)
            .await
    }
    /// `source_id` must be unique per consume call — it's the
    /// `(kind, source_table, source_id)` key for the
    /// `finance.inventory.transferred` financial_fact. It must NOT be
    /// time-based: under sim-time threading `now` collapses to a
    /// date-at-midnight, so a time-keyed id would collide for every
    /// consume of the same SKU on the same sim_day and silently drop
    /// all but the first fact. The HTTP handler generates a
    /// UUID-keyed source_id and uses the SAME value in the matching
    /// audit_log event payload so bundle round-trip stays idempotent.
    ///
    /// OUTBOX (phase 2): records `inventory.item.consumed` (post-
    /// consume row state) + `inventory.transferred` (the exact in-tx
    /// fact payload, value-draining consumes only) in the SAME
    /// transaction as the decrement. The idempotency guard sits ahead
    /// of both, so a redelivered consume records nothing.
    async fn consume_part_at(
        &self,
        part_sku: &str,
        qty: u32,
        now: DateTime<Utc>,
        source_id: &str,
        stamp: &EventStamp,
    ) -> Result<ConsumeApplied, InventoryError>;

    /// Sum of `expected_qty - received_qty` across every open
    /// ingredient-restock Job's not-yet-completed receiving step
    /// where the line targets `part_sku`. The "inbound reservation"
    /// projection — supply that's already on its way. Used by the
    /// auto-restock trigger so it doesn't open redundant Jobs when
    /// the cumulative inbound + on-hand already clears
    /// reorder_point. Symmetric to `inventory_items.allocated`,
    /// which is the outgoing-reservation counterpart.
    async fn inbound_reserved_for_part(&self, part_sku: &str) -> Result<i64, InventoryError>;

    /// True iff there is any open PO with a line for this part_sku.
    /// Used by the dispatcher's canonical reorder-threshold rule
    /// (`when = "... AND NOT open_po_exists(part_sku)"`) to suppress
    /// duplicate restock spawns. Open = status NOT IN
    /// ('received', 'closed', 'cancelled').
    async fn open_po_exists_for_part(&self, part_sku: &str) -> Result<bool, InventoryError>;

    /// The vendor most recently associated with this part_sku via a
    /// PO line. Falls back to None when the SKU has never been
    /// ordered. Used by the dispatcher's reorder rule's
    /// `vendor_for(part_sku)` helper to set the spawned Job's
    /// Subject.
    async fn primary_vendor_for_part(
        &self,
        part_sku: &str,
    ) -> Result<Option<String>, InventoryError>;

    /// Record one production-overhead driver being capitalized into WIP at
    /// production-consume time. Three writes in one tx (outbox phase 2):
    ///   1. `finance.inventory.transferred` financial_fact +
    ///      journal entry (DR `debit_account` / CR `credit_account`).
    ///   2. The matching `inventory.overhead.absorbed` audit event,
    ///      recorded in the SAME transaction — gated on this call
    ///      having inserted the fact, so an idempotent replay records
    ///      nothing.
    ///
    /// `source_id` is the production-consume step id (typical) or a
    /// timestamp fallback; the `(kind, source_table, source_id)`
    /// unique index makes the insert idempotent across rebuild
    /// replays.
    async fn record_overhead_absorbed(
        &self,
        total_cost_cents: i64,
        debit_account: &str,
        credit_account: &str,
        memo: &str,
        source_id: &str,
        happened_on: NaiveDate,
        stamp: &EventStamp,
    ) -> Result<(uuid::Uuid, bool), InventoryError>;

    /// Atomic opening-balance / adjustment JE for inventory
    /// changes that don't already pair with a consume / receive
    /// fact. Used by `batch_upsert_items` (seed-side opening
    /// balance, DR 1300 / CR 3000 sized at qty × avg_cost) and
    /// the PUT inventory endpoints (manual adjustment, same
    /// shape). The `source_table` lets the caller distinguish
    /// `brewery_seed_opening_balance` (idempotent re-runs) from
    /// `inventory_adjustment` (time-stamped, fires every PUT).
    /// OUTBOX (phase 2): when THIS call inserts the fact, the matching
    /// `ledger.inventory.transferred` event records in the same
    /// transaction — the emit-once-on-`inserted` contract is
    /// structural (mirrors `ProductsRepository::record_inventory_je`).
    async fn record_inventory_je(
        &self,
        total_cost_cents: i64,
        debit_account: &str,
        credit_account: &str,
        memo: &str,
        source_table: &str,
        source_id: &str,
        happened_on: NaiveDate,
        stamp: &EventStamp,
    ) -> Result<JeRecorded, InventoryError>;
    /// Receive a part — increments `on_hand` by `qty`. Used by
    /// the `receiving` StepType's side effect when a goods-receipt
    /// step completes against a PO. Returns the post-receive row
    /// state so the API + bridge can emit the canonical
    /// `inventory.item.upserted` event with the new on_hand.
    ///
    /// When `unit_cost_cents` is `Some(_)`, the adapter folds it
    /// into the part's weighted moving average:
    ///   new_avg = (old_avg × old_on_hand + unit_cost × qty)
    ///             / (old_on_hand + qty)
    /// `None` leaves `avg_cost_cents` unchanged — used by callers
    /// that don't carry cost data (e.g., a manual replenishment
    /// without a PO).
    ///
    /// `source_id` is the idempotency key. `on_hand += qty` is a
    /// relative mutation, so a redelivered `step.done.receiving`
    /// (at-least-once JetStream delivery) would double-increment.
    /// The adapter writes a `finance.inventory.received` proof-fact
    /// keyed `(kind, source_table="inventory_receipt", source_id)` in
    /// the same tx as the increment; if a fact with this key already
    /// exists the receive committed on a prior delivery, so it skips
    /// the increment and returns the current row unchanged. That fact
    /// is a DEDUP + AUDIT marker ONLY — it drives NO GL journal line
    /// (the DR-1300 rides the idempotent bill-approval path). The
    /// fallback id the HTTP layer supplies must be RANDOM, never
    /// time-based (see `consume_part_at`).
    async fn receive_part(
        &self,
        part_sku: &str,
        qty: u32,
        unit_cost_cents: Option<i64>,
        source_id: &str,
    ) -> Result<ReceiveApplied, InventoryError> {
        let now = Utc::now();
        let stamp = EventStamp::new(
            "inventory",
            boss_core::actor::ActorId::Automation("platform".into()),
            now,
        );
        self.receive_part_at(part_sku, qty, unit_cost_cents, now, source_id, &stamp)
            .await
    }
    /// OUTBOX (phase 2): records `inventory.item.upserted` (post-
    /// receive row state) + `inventory.item.received` (the exact
    /// proof-fact payload) in the SAME transaction as the increment.
    /// The idempotency guard sits ahead of both, so a redelivered
    /// receive records nothing.
    async fn receive_part_at(
        &self,
        part_sku: &str,
        qty: u32,
        unit_cost_cents: Option<i64>,
        now: DateTime<Utc>,
        source_id: &str,
        stamp: &EventStamp,
    ) -> Result<ReceiveApplied, InventoryError>;
    async fn create_purchase_order(&self, po: &PurchaseOrder) -> Result<(), InventoryError> {
        let now = Utc::now();
        let stamp = EventStamp::new(
            "inventory",
            boss_core::actor::ActorId::Automation("platform".into()),
            now,
        );
        self.create_purchase_order_at(po, now, &stamp).await
    }
    /// OUTBOX (phase 2): header + lines + subject identity +
    /// `inventory.purchase_order.upserted` (the caller-intended PO
    /// state) commit or abort in ONE transaction.
    async fn create_purchase_order_at(
        &self,
        po: &PurchaseOrder,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), InventoryError>;
    /// Records `inventory.purchase_order.upserted` (post-update row
    /// state, read back in-tx) + the `inventory.po.status_changed`
    /// marker in the same transaction as the flip — outbox phase 2.
    async fn update_po_status(
        &self,
        id: &str,
        status: &str,
        stamp: &EventStamp,
    ) -> Result<(), InventoryError>;

    async fn all_vendors(&self) -> Result<Vec<Vendor>, InventoryError>;
    /// Convenience overload: stamps `Utc::now()` + a platform-
    /// automation event stamp. Handlers use `create_vendor_at`.
    async fn create_vendor(&self, vendor: &Vendor) -> Result<String, InventoryError> {
        let now = Utc::now();
        let stamp = EventStamp::new(
            "inventory",
            boss_core::actor::ActorId::Automation("platform".into()),
            now,
        );
        self.create_vendor_at(vendor, now, &stamp).await
    }
    /// OUTBOX (transactional-audit-log phase 2): records
    /// `inventory.vendor.created` (full row state) — enriched via
    /// `stamp` — in the SAME transaction as the row + the subject
    /// identity write-through, so the event and the state commit or
    /// abort together. Callers no longer publish this kind
    /// post-commit; boss-event-relay moves it to audit_log + NATS.
    async fn create_vendor_at(
        &self,
        vendor: &Vendor,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<String, InventoryError>;
    /// Records `inventory.vendor.updated` (full post-update row
    /// state) in the same transaction as the UPDATE — outbox phase 2.
    /// Also refreshes the subject identity name write-through, so a
    /// vendor rename keeps `subjects.name` current (mirrors
    /// `ProductsRepository::upsert_product`).
    async fn update_vendor(
        &self,
        id: &str,
        vendor: &Vendor,
        stamp: &EventStamp,
    ) -> Result<(), InventoryError>;
    /// Records `inventory.vendor.deleted` in the same transaction as
    /// the DELETE — outbox phase 2. The subjects identity row stays
    /// (identity is event-sourced; deletion is a fact, not an erasure).
    async fn delete_vendor(&self, id: &str, stamp: &EventStamp) -> Result<(), InventoryError>;

    /// Three-way match: upsert a vendor invoice keyed by `id`. If
    /// the caller already ran the match logic, they pass the status,
    /// discrepancy_cents, and discrepancy_kind. Otherwise the invoice
    /// lands as `status=received` for later reconciliation.
    async fn upsert_vendor_invoice(&self, invoice: &VendorInvoice) -> Result<(), InventoryError> {
        let now = Utc::now();
        let stamp = EventStamp::new(
            "inventory",
            boss_core::actor::ActorId::Automation("platform".into()),
            now,
        );
        self.upsert_vendor_invoice_at(invoice, now, &stamp).await
    }
    /// OUTBOX (phase 2): records `inventory.vendor_invoice.upserted`
    /// (the full row — the last-write-wins rebuild source) plus the
    /// `approved` / `paid` transition events in the SAME transaction
    /// as the upsert + the transition facts. Transition events gate on
    /// their fact actually inserting, so a re-upsert of an already-
    /// approved/paid invoice appends no duplicate transition event.
    async fn upsert_vendor_invoice_at(
        &self,
        invoice: &VendorInvoice,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), InventoryError>;

    /// List vendor invoices, optionally filtered by status.
    async fn all_vendor_invoices(
        &self,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<VendorInvoice>, InventoryError>;

    /// Fetch one vendor invoice by id (or `None`). Guards the
    /// vendor-posts-invoice path against downgrading an already-approved
    /// invoice back to `received`.
    async fn vendor_invoice_by_id(&self, id: &str)
    -> Result<Option<VendorInvoice>, InventoryError>;

    /// AP aging: unpaid vendor invoices bucketed by days since
    /// `received_on`. Mirrors the AR aging shape so finance surfaces
    /// can render both sides with one layout. Ages from received_on;
    /// aging-from-due_date would need a vendor join + payment-terms
    /// parsing.
    ///
    /// `today` anchors the aging buckets ("days since received_on").
    /// The HTTP handler sources it from ClockClient so sim-time
    /// invoices bucket correctly instead of all landing in "90+".
    async fn ap_aging(&self, today: chrono::NaiveDate) -> Result<ApAging, InventoryError>;
}
