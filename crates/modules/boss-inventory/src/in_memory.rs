//! In-memory adapter for `InventoryRepository` — the test double (no
//! mocks; a real implementation of the port, minus durability, the
//! ledger's journal and the jobs store). Held to Postgres by
//! `tests/the_adapters_agree_on_the_inventory_store_pg.rs` (backlog
//! be459ab9): it moves stock with the same arithmetic, records the same
//! facts, answers the same canonical fact ids, and keeps the same proof
//! of application that makes a redelivered receive or consume a no-op.
//! Until that suite it was a stub — an item upsert stored nothing, a
//! consume or receive moved no stock, and a JE answered a fresh random
//! fact id every time.
//!
//! What it cannot hold: a chart of accounts (an unknown GL account is
//! refused by Postgres alone) and the jobs store's open restock steps
//! (`inbound_reserved_for_part` answers 0).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::publisher::EventStamp;

use crate::port::{
    InventoryError, InventoryRepository, nul_key, on_hand_overflow, refuse_bad_invoice,
    refuse_bad_item, refuse_bad_po, refuse_bad_vendor, refuse_negative_limit, refuse_non_positive,
    refuse_nul, refuse_past_i32, unplaced_order, unregistered_vendor,
};
use crate::types::{
    ApAging, ApAgingBucket, ConsumeApplied, InventoryItem, JeRecorded, PoStatus, PurchaseOrder,
    ReceiveApplied, Vendor, VendorInvoice, bill_approved_payload, bill_paid_payload,
};

#[derive(Default)]
struct State {
    /// Keyed by SKU: iterates in byte order, as the port lists.
    items: BTreeMap<String, InventoryItem>,
    purchase_orders: BTreeMap<String, PurchaseOrder>,
    vendors: BTreeMap<String, Vendor>,
    /// Vendor ids that hold a Subject identity row — every vendor ever
    /// created (a delete keeps the identity, as Postgres's `subjects`
    /// does). A purchase order may name only these.
    vendor_subjects: BTreeSet<String>,
    vendor_invoices: BTreeMap<String, VendorInvoice>,
    /// The `financial_facts` natural keys `(kind, source_table,
    /// source_id)` written — Postgres's unique index, and the proof of
    /// application its receive / consume guard reads.
    facts: BTreeSet<(String, String, String)>,
    recorded: Vec<Event>,
}

impl State {
    fn record(&mut self, stamp: &EventStamp, kind: &str, payload: serde_json::Value) {
        self.recorded.push(stamp.event(kind, payload));
    }

    fn has_fact(&self, kind: &str, source_table: &str, source_id: &str) -> bool {
        self.facts
            .contains(&(kind.into(), source_table.into(), source_id.into()))
    }

    /// Insert a fact's key; `true` when this call inserted it.
    fn insert_fact(&mut self, kind: &str, source_table: &str, source_id: &str) -> bool {
        self.facts
            .insert((kind.into(), source_table.into(), source_id.into()))
    }
}

pub struct InMemoryInventory {
    state: Mutex<State>,
}

/// The row as the table keeps it: the unit cost is the generated column
/// `value / on_hand` (0 with nothing on hand), whatever was sent.
fn settle(mut item: InventoryItem) -> InventoryItem {
    item.avg_cost_cents = if item.on_hand > 0 {
        item.value_cents / item.on_hand as i64
    } else {
        0
    };
    item
}

/// The order as the tables answer it: lines in byte order of SKU.
fn assembled(mut po: PurchaseOrder) -> PurchaseOrder {
    po.lines.sort_by(|a, b| a.part_sku.cmp(&b.part_sku));
    po
}

impl InMemoryInventory {
    pub fn new(items: Vec<InventoryItem>, purchase_orders: Vec<PurchaseOrder>) -> Self {
        Self::with_vendors(items, purchase_orders, Vec::new())
    }

    /// A store seeded as if each row had been written through the port —
    /// seeded vendors hold their Subject identity.
    pub fn with_vendors(
        items: Vec<InventoryItem>,
        purchase_orders: Vec<PurchaseOrder>,
        vendors: Vec<Vendor>,
    ) -> Self {
        let state = State {
            items: items
                .into_iter()
                .map(|i| (i.part_sku.clone(), settle(i)))
                .collect(),
            purchase_orders: purchase_orders
                .into_iter()
                .map(|p| (p.id.clone(), assembled(p)))
                .collect(),
            vendor_subjects: vendors.iter().map(|v| v.id.clone()).collect(),
            vendors: vendors.into_iter().map(|v| (v.id.clone(), v)).collect(),
            ..State::default()
        };
        Self {
            state: Mutex::new(state),
        }
    }

    /// Events recorded by the write paths, in call order — the
    /// in-memory analogue of the Pg adapter's in-tx outbox write.
    pub fn recorded_events(&self) -> Vec<Event> {
        self.state
            .lock()
            .map(|s| s.recorded.clone())
            .unwrap_or_default()
    }

    fn state(&self) -> Result<MutexGuard<'_, State>, InventoryError> {
        self.state
            .lock()
            .map_err(|_| InventoryError::Storage("in-memory inventory lock poisoned".into()))
    }
}

#[async_trait]
impl InventoryRepository for InMemoryInventory {
    async fn all_items(&self) -> Result<Vec<InventoryItem>, InventoryError> {
        Ok(self.state()?.items.values().cloned().collect())
    }

    async fn item_by_sku(&self, part_sku: &str) -> Result<Option<InventoryItem>, InventoryError> {
        if nul_key(part_sku) {
            return Ok(None);
        }
        Ok(self.state()?.items.get(part_sku).cloned())
    }

    async fn upsert_item_at(
        &self,
        item: &InventoryItem,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &EventStamp,
    ) -> Result<(), InventoryError> {
        refuse_bad_item(item)?;
        let mut state = self.state()?;
        state
            .items
            .insert(item.part_sku.clone(), settle(item.clone()));
        // The item as sent — the last-write-wins rebuild source.
        state.record(
            stamp,
            crate::events::ITEM_UPSERTED,
            serde_json::to_value(item).unwrap_or_default(),
        );
        Ok(())
    }

    async fn all_purchase_orders(&self) -> Result<Vec<PurchaseOrder>, InventoryError> {
        let mut orders: Vec<PurchaseOrder> =
            self.state()?.purchase_orders.values().cloned().collect();
        // Latest placement first, an unplaced order (Postgres's DESC puts
        // NULL first) leading; the map already orders ids by byte, and
        // the sort is stable, so a tie stays in byte order of id.
        orders.sort_by(|a, b| match (a.placed_on, b.placed_on) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => y.cmp(&x),
        });
        Ok(orders)
    }

    async fn purchase_order_by_id(
        &self,
        id: &str,
    ) -> Result<Option<PurchaseOrder>, InventoryError> {
        if nul_key(id) {
            return Ok(None);
        }
        Ok(self.state()?.purchase_orders.get(id).cloned())
    }

    async fn consume_part_at(
        &self,
        part_sku: &str,
        qty: u32,
        now: chrono::DateTime<chrono::Utc>,
        source_id: &str,
        stamp: &EventStamp,
    ) -> Result<ConsumeApplied, InventoryError> {
        refuse_nul(&[("part_sku", part_sku), ("source_id", source_id)])?;
        let mut state = self.state()?;
        // The delivery's source_id is the guard: a valued consume's
        // transfer fact or a valueless one's inert marker proves it
        // applied, so a redelivery answers the current row and records
        // nothing (backlog 55f69172: the valueless kind went unguarded).
        if state.has_fact(
            "finance.inventory.transferred",
            "inventory_consume",
            source_id,
        ) || state.has_fact("finance.inventory.consumed", "inventory_consume", source_id)
        {
            let item = state
                .items
                .get(part_sku)
                .cloned()
                .ok_or_else(|| InventoryError::NotFound(part_sku.to_string()))?;
            return Ok(ConsumeApplied {
                item,
                fact_payload: None,
            });
        }
        let before = state
            .items
            .get(part_sku)
            .cloned()
            .ok_or_else(|| InventoryError::NotFound(part_sku.to_string()))?;
        if before.on_hand < qty {
            return Err(InventoryError::InsufficientStock(
                part_sku.to_string(),
                before.on_hand,
                qty,
            ));
        }
        // The drain Postgres computes: round-half-up(value × qty /
        // on_hand), the last unit taking whatever value remains.
        let drained_cents = if before.on_hand == qty {
            before.value_cents
        } else {
            (((before.value_cents as i128) * (qty as i128) + (before.on_hand as i128) / 2)
                / (before.on_hand as i128)) as i64
        };
        let item = settle(InventoryItem {
            on_hand: before.on_hand - qty,
            value_cents: before.value_cents - drained_cents,
            ..before
        });
        state.items.insert(part_sku.to_string(), item.clone());
        let fact_payload = (drained_cents > 0).then(|| {
            serde_json::json!({
                "total_cost_cents": drained_cents,
                "debit_account": "1310",
                "credit_account": "1300",
                "memo": format!(
                    "Production — consumed {qty} × {part_sku} (raw → WIP, value drain)",
                ),
                "part_sku": part_sku,
                "qty": qty,
                "source_id": source_id,
                "consumed_on": now.date_naive(),
            })
        });
        let marker_payload = fact_payload.is_none().then(|| {
            serde_json::json!({
                "part_sku": part_sku,
                "qty": qty,
                "source_id": source_id,
                "consumed_on": now.date_naive(),
            })
        });
        if fact_payload.is_some() {
            state.insert_fact(
                "finance.inventory.transferred",
                "inventory_consume",
                source_id,
            );
        } else {
            state.insert_fact("finance.inventory.consumed", "inventory_consume", source_id);
        }
        state.record(
            stamp,
            crate::events::ITEM_CONSUMED,
            serde_json::to_value(&item).unwrap_or_default(),
        );
        if let Some(payload) = &fact_payload {
            state.record(stamp, crate::events::INVENTORY_TRANSFERRED, payload.clone());
        }
        if let Some(payload) = marker_payload {
            state.record(stamp, crate::events::ITEM_CONSUME_RECORDED, payload);
        }
        Ok(ConsumeApplied { item, fact_payload })
    }

    async fn inbound_reserved_for_part(&self, _part_sku: &str) -> Result<i64, InventoryError> {
        // The double holds no jobs store, so no open restock step can
        // name the part; tests that exercise the auto-restock trigger
        // drive the postgres adapter.
        Ok(0)
    }

    async fn open_po_exists_for_part(&self, part_sku: &str) -> Result<bool, InventoryError> {
        Ok(self.state()?.purchase_orders.values().any(|po| {
            !matches!(
                po.status.as_str(),
                PoStatus::RECEIVED | PoStatus::CLOSED | "cancelled"
            ) && po.lines.iter().any(|l| l.part_sku == part_sku)
        }))
    }

    async fn primary_vendor_for_part(
        &self,
        part_sku: &str,
    ) -> Result<Option<String>, InventoryError> {
        let state = self.state()?;
        // The latest-placed order naming a vendor with a line for the
        // part (NULLS LAST), a tie to the lowest id: the map iterates ids
        // in byte order and `max_by` keeps the LAST maximum, so iterate
        // in reverse.
        let latest = state
            .purchase_orders
            .values()
            .rev()
            .filter(|po| po.lines.iter().any(|l| l.part_sku == part_sku))
            .filter_map(|po| po.vendor.clone().map(|v| (po.placed_on, v)))
            .max_by(|a, b| match (a.0, b.0) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (Some(x), Some(y)) => x.cmp(&y),
            });
        if let Some((_, vendor)) = latest {
            return Ok(Some(vendor));
        }
        // No order history: the lowest-id vendor serving the part's
        // category.
        let category = state
            .items
            .get(part_sku)
            .and_then(|i| i.vendor_category.clone());
        Ok(category.and_then(|c| {
            state
                .vendors
                .values()
                .find(|v| v.category.as_deref() == Some(c.as_str()))
                .map(|v| v.id.clone())
        }))
    }

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
    ) -> Result<JeRecorded, InventoryError> {
        refuse_non_positive(total_cost_cents)?;
        refuse_nul(&[
            ("debit_account", debit_account),
            ("credit_account", credit_account),
            ("memo", memo),
            ("source_table", source_table),
            ("source_id", source_id),
        ])?;
        let payload = serde_json::json!({
            "total_cost_cents": total_cost_cents,
            "debit_account": debit_account,
            "credit_account": credit_account,
            "memo": memo,
            "happened_on": happened_on.to_string(),
            "source_table": source_table,
            "source_id": source_id,
        });
        let mut state = self.state()?;
        let inserted = state.insert_fact("finance.inventory.transferred", source_table, source_id);
        if inserted {
            state.record(
                stamp,
                crate::events::LEDGER_INVENTORY_TRANSFERRED,
                payload.clone(),
            );
        }
        Ok(JeRecorded {
            fact_id: boss_ledger::deterministic_fact_id(
                "finance.inventory.transferred",
                source_table,
                source_id,
            ),
            inserted,
            payload,
        })
    }

    async fn record_overhead_absorbed(
        &self,
        total_cost_cents: i64,
        debit_account: &str,
        credit_account: &str,
        memo: &str,
        source_id: &str,
        happened_on: chrono::NaiveDate,
        stamp: &EventStamp,
    ) -> Result<(uuid::Uuid, bool), InventoryError> {
        refuse_non_positive(total_cost_cents)?;
        refuse_nul(&[
            ("debit_account", debit_account),
            ("credit_account", credit_account),
            ("memo", memo),
            ("source_id", source_id),
        ])?;
        let payload = serde_json::json!({
            "total_cost_cents": total_cost_cents,
            "debit_account": debit_account,
            "credit_account": credit_account,
            "memo": memo,
            "happened_on": happened_on,
            "source_id": source_id,
        });
        let mut state = self.state()?;
        let inserted = state.insert_fact(
            "finance.inventory.transferred",
            "ledger_overhead_absorbed",
            source_id,
        );
        if inserted {
            state.record(stamp, crate::events::INVENTORY_OVERHEAD_ABSORBED, payload);
        }
        Ok((
            boss_ledger::deterministic_fact_id(
                "finance.inventory.transferred",
                "ledger_overhead_absorbed",
                source_id,
            ),
            inserted,
        ))
    }

    async fn receive_part_at(
        &self,
        part_sku: &str,
        qty: u32,
        unit_cost_cents: Option<i64>,
        now: chrono::DateTime<chrono::Utc>,
        source_id: &str,
        stamp: &EventStamp,
    ) -> Result<ReceiveApplied, InventoryError> {
        refuse_nul(&[("part_sku", part_sku), ("source_id", source_id)])?;
        refuse_past_i32(&[("qty", qty)])?;
        let mut state = self.state()?;
        let current = state
            .items
            .get(part_sku)
            .cloned()
            .ok_or_else(|| InventoryError::NotFound(part_sku.to_string()));
        // The receipt fact is the proof of application: a redelivered
        // receive answers the current row and records nothing.
        if state.has_fact("finance.inventory.received", "inventory_receipt", source_id) {
            return Ok(ReceiveApplied {
                item: current?,
                receipt_payload: None,
            });
        }
        let before = current?;
        // The column holds an i32: past it is refused `Invalid`, as on
        // Postgres (until backlog 55f69172 the double counted in u32 and
        // accepted what Postgres refused).
        if i64::from(before.on_hand) + i64::from(qty) > i64::from(i32::MAX) {
            return Err(on_hand_overflow(part_sku, before.on_hand, qty));
        }
        // Value-primary: a positive unit cost adds the exact line total;
        // none (or zero or less) moves units only.
        let added = match unit_cost_cents {
            Some(unit) if unit > 0 => unit.checked_mul(qty as i64),
            _ => Some(0),
        };
        // Past the columns' range Postgres refuses with its overflow
        // error; so does the double, rather than wrap or panic.
        let (Some(on_hand), Some(value_cents)) = (
            before.on_hand.checked_add(qty),
            added.and_then(|a| before.value_cents.checked_add(a)),
        ) else {
            return Err(InventoryError::Storage(format!(
                "receiving {qty} of {part_sku} overflows the row"
            )));
        };
        let item = settle(InventoryItem {
            on_hand,
            value_cents,
            ..before
        });
        state.items.insert(part_sku.to_string(), item.clone());
        let payload = serde_json::json!({
            "part_sku": part_sku,
            "qty": qty,
            "unit_cost_cents": unit_cost_cents,
            "received_on": now.date_naive(),
            "source_id": source_id,
        });
        state.insert_fact("finance.inventory.received", "inventory_receipt", source_id);
        state.record(
            stamp,
            crate::events::ITEM_UPSERTED,
            serde_json::to_value(&item).unwrap_or_default(),
        );
        state.record(stamp, crate::events::ITEM_RECEIVED, payload.clone());
        Ok(ReceiveApplied {
            item,
            receipt_payload: Some(payload),
        })
    }

    async fn create_purchase_order_at(
        &self,
        po: &PurchaseOrder,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &EventStamp,
    ) -> Result<(), InventoryError> {
        refuse_bad_po(po)?;
        let mut state = self.state()?;
        if let Some(vendor) = po.vendor.as_deref().filter(|v| !v.is_empty())
            && !state.vendor_subjects.contains(vendor)
        {
            return Err(unregistered_vendor(vendor));
        }
        // A re-post moves the vendor, status, expected and received
        // dates; it keeps the first placement date and every line held,
        // adding lines for new SKUs only (Postgres's ON CONFLICT DO
        // UPDATE / DO NOTHING).
        let stored = match state.purchase_orders.get(&po.id) {
            Some(held) => {
                let mut lines = held.lines.clone();
                for line in &po.lines {
                    if !lines.iter().any(|l| l.part_sku == line.part_sku) {
                        lines.push(line.clone());
                    }
                }
                PurchaseOrder {
                    placed_on: held.placed_on,
                    lines,
                    ..po.clone()
                }
            }
            None => {
                // A line repeated within one post keeps its first.
                let mut lines: Vec<_> = Vec::new();
                for line in &po.lines {
                    if !lines
                        .iter()
                        .any(|l: &crate::types::PurchaseOrderLine| l.part_sku == line.part_sku)
                    {
                        lines.push(line.clone());
                    }
                }
                PurchaseOrder {
                    lines,
                    ..po.clone()
                }
            }
        };
        state
            .purchase_orders
            .insert(po.id.clone(), assembled(stored));
        // The caller-intended order, as sent.
        state.record(
            stamp,
            crate::events::PO_UPSERTED,
            serde_json::to_value(po).unwrap_or_default(),
        );
        Ok(())
    }

    async fn update_po_status(
        &self,
        id: &str,
        status: &str,
        stamp: &EventStamp,
    ) -> Result<(), InventoryError> {
        refuse_nul(&[("id", id), ("status", status)])?;
        let mut state = self.state()?;
        let order = state
            .purchase_orders
            .get_mut(id)
            .ok_or_else(|| InventoryError::NotFound(id.to_string()))?;
        order.status = PoStatus::new(status);
        let flipped = order.clone();
        state.record(
            stamp,
            crate::events::PO_UPSERTED,
            serde_json::to_value(&flipped).unwrap_or_default(),
        );
        state.record(
            stamp,
            crate::events::PO_STATUS_CHANGED,
            serde_json::json!({ "id": id, "new_status": status }),
        );
        Ok(())
    }

    async fn all_vendors(&self) -> Result<Vec<Vendor>, InventoryError> {
        let mut vendors: Vec<Vendor> = self.state()?.vendors.values().cloned().collect();
        // Byte order of name, nameless last; the map orders ids by byte
        // and the sort is stable, so a tie stays in byte order of id.
        vendors.sort_by(|a, b| match (&a.name, &b.name) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        Ok(vendors)
    }

    async fn create_vendor_at(
        &self,
        vendor: &Vendor,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &EventStamp,
    ) -> Result<String, InventoryError> {
        refuse_bad_vendor(vendor)?;
        let mut state = self.state()?;
        if state.vendors.contains_key(&vendor.id) {
            return Err(InventoryError::Conflict(format!(
                "vendor {} already exists",
                vendor.id
            )));
        }
        state.vendors.insert(vendor.id.clone(), vendor.clone());
        state.vendor_subjects.insert(vendor.id.clone());
        state.record(
            stamp,
            crate::events::VENDOR_CREATED,
            serde_json::to_value(vendor).unwrap_or_default(),
        );
        Ok(vendor.id.clone())
    }

    async fn update_vendor(
        &self,
        id: &str,
        vendor: &Vendor,
        stamp: &EventStamp,
    ) -> Result<(), InventoryError> {
        refuse_nul(&[("id", id)])?;
        // The row is the one the path names: the body's id is not
        // written (until backlog be459ab9 the double rewrote it).
        let row = Vendor {
            id: id.to_string(),
            ..vendor.clone()
        };
        refuse_bad_vendor(&row)?;
        let mut state = self.state()?;
        let existing = state
            .vendors
            .get_mut(id)
            .ok_or_else(|| InventoryError::NotFound(format!("vendor {id}")))?;
        *existing = row;
        state.record(
            stamp,
            crate::events::VENDOR_UPDATED,
            serde_json::to_value(vendor).unwrap_or_default(),
        );
        Ok(())
    }

    async fn delete_vendor(&self, id: &str, stamp: &EventStamp) -> Result<(), InventoryError> {
        refuse_nul(&[("id", id)])?;
        let mut state = self.state()?;
        if state.vendors.remove(id).is_none() {
            return Err(InventoryError::NotFound(format!("vendor {id}")));
        }
        state.record(
            stamp,
            crate::events::VENDOR_DELETED,
            serde_json::json!({ "id": id, "deleted_at": stamp.timestamp }),
        );
        Ok(())
    }

    async fn upsert_vendor_invoice_at(
        &self,
        invoice: &VendorInvoice,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &EventStamp,
    ) -> Result<(), InventoryError> {
        refuse_bad_invoice(invoice)?;
        let mut state = self.state()?;
        if !state.purchase_orders.contains_key(&invoice.po_id) {
            return Err(unplaced_order(&invoice.po_id));
        }
        // The table keeps no lines: the bill's breakdown lives on its
        // approval fact, so a read answers none.
        state.vendor_invoices.insert(
            invoice.id.clone(),
            VendorInvoice {
                lines: Vec::new(),
                ..invoice.clone()
            },
        );
        state.record(
            stamp,
            crate::events::VENDOR_INVOICE_UPSERTED,
            serde_json::to_value(invoice).unwrap_or_default(),
        );
        // Each transition records once: when its fact first inserts.
        if let Some(approved_on) = invoice.approved_on
            && state.insert_fact("finance.bill.approved", "vendor_invoices", &invoice.id)
        {
            state.record(
                stamp,
                crate::events::VENDOR_INVOICE_APPROVED,
                bill_approved_payload(invoice, approved_on),
            );
        }
        if let Some(paid_on) = invoice.paid_on
            && state.insert_fact("finance.bill.paid", "vendor_invoices", &invoice.id)
        {
            state.record(
                stamp,
                crate::events::VENDOR_INVOICE_PAID,
                bill_paid_payload(invoice, paid_on),
            );
        }
        Ok(())
    }

    async fn all_vendor_invoices(
        &self,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<VendorInvoice>, InventoryError> {
        refuse_negative_limit(limit)?;
        let mut filtered: Vec<VendorInvoice> = self
            .state()?
            .vendor_invoices
            .values()
            .filter(|v| status.is_none_or(|s| v.status.as_str() == s))
            .cloned()
            .collect();
        // Latest received first; the map orders ids by byte and the sort
        // is stable, so a tie stays in byte order of id.
        filtered.sort_by_key(|i| std::cmp::Reverse(i.received_on));
        filtered.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(filtered)
    }

    async fn vendor_invoice_by_id(
        &self,
        id: &str,
    ) -> Result<Option<VendorInvoice>, InventoryError> {
        Ok(self.state()?.vendor_invoices.get(id).cloned())
    }

    async fn ap_aging(&self, today: chrono::NaiveDate) -> Result<ApAging, InventoryError> {
        let state = self.state()?;
        let mut buckets: std::collections::HashMap<&'static str, (i64, i64)> =
            std::collections::HashMap::new();
        let mut total_outstanding: i64 = 0;
        let mut total_count: i64 = 0;
        for v in state.vendor_invoices.values() {
            if v.status.is_paid() {
                continue;
            }
            let days = (today - v.received_on).num_days();
            let label = bucket_label(days);
            let entry = buckets.entry(label).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += v.amount_cents;
            total_outstanding += v.amount_cents;
            total_count += 1;
        }
        Ok(ApAging {
            buckets: canonical_buckets(&buckets),
            total_outstanding_cents: total_outstanding,
            total_invoice_count: total_count,
            currency: "USD".to_string(),
        })
    }
}

/// AP-aging bucket thresholds. Mirrors the AR aging buckets so
/// callers can use the same layout for both sides of the ledger.
pub(crate) fn bucket_label(days_since_received: i64) -> &'static str {
    if days_since_received <= 0 {
        "current"
    } else if days_since_received <= 30 {
        "1-30"
    } else if days_since_received <= 60 {
        "31-60"
    } else if days_since_received <= 90 {
        "61-90"
    } else {
        "90+"
    }
}

/// Emit the five canonical buckets in order even when some are empty,
/// so the frontend always sees the same shape.
pub(crate) fn canonical_buckets(
    map: &std::collections::HashMap<&'static str, (i64, i64)>,
) -> Vec<ApAgingBucket> {
    ["current", "1-30", "31-60", "61-90", "90+"]
        .iter()
        .map(|label| {
            let (count, total_cents) = map.get(*label).copied().unwrap_or((0, 0));
            ApAgingBucket {
                label: label.to_string(),
                count,
                total_cents,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn test_item(sku: &str) -> InventoryItem {
        InventoryItem {
            part_sku: sku.to_string(),
            bin: "A-01".to_string(),
            on_hand: 50,
            allocated: 10,
            reorder_point: 20,
            reorder_qty: 100,
            trailing_90d_usage: 30,
            value_cents: 0,
            avg_cost_cents: 0,
            vendor_price_cents: None,
            vendor_category: None,
        }
    }

    fn test_po(id: &str) -> PurchaseOrder {
        PurchaseOrder {
            id: id.to_string(),
            vendor: Some("Acme Parts Co".to_string()),
            status: PoStatus::new(PoStatus::SUBMITTED),
            placed_on: Some(chrono::NaiveDate::from_ymd_opt(2025, 3, 1).unwrap()),
            expected_on: Some(chrono::NaiveDate::from_ymd_opt(2025, 3, 15).unwrap()),
            received_on: None,
            lines: vec![PurchaseOrderLine {
                part_sku: "PART-001".to_string(),
                qty: 25,
                unit_cost_cents: 15_000,
                currency: "USD".to_string(),
            }],
        }
    }

    fn test_repo() -> InMemoryInventory {
        InMemoryInventory::new(
            vec![test_item("PART-001"), test_item("PART-002")],
            vec![test_po("PO-001"), test_po("PO-002"), test_po("PO-003")],
        )
    }

    #[tokio::test]
    async fn all_items_returns_all() {
        let repo = test_repo();
        assert_eq!(repo.all_items().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn item_by_sku_found() {
        let repo = test_repo();
        let item = repo.item_by_sku("PART-001").await.unwrap();
        assert!(item.is_some());
        assert_eq!(item.unwrap().part_sku, "PART-001");
    }

    #[tokio::test]
    async fn item_by_sku_not_found() {
        let repo = test_repo();
        assert!(repo.item_by_sku("PART-999").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn all_purchase_orders_returns_all() {
        let repo = test_repo();
        assert_eq!(repo.all_purchase_orders().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn purchase_order_by_id_found() {
        let repo = test_repo();
        let po = repo.purchase_order_by_id("PO-002").await.unwrap();
        assert!(po.is_some());
        assert_eq!(po.unwrap().id, "PO-002");
    }

    #[tokio::test]
    async fn purchase_order_by_id_not_found() {
        let repo = test_repo();
        assert!(repo.purchase_order_by_id("PO-999").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn empty_repo() {
        let repo = InMemoryInventory::new(vec![], vec![]);
        assert!(repo.all_items().await.unwrap().is_empty());
        assert!(repo.all_purchase_orders().await.unwrap().is_empty());
        assert!(repo.item_by_sku("PART-001").await.unwrap().is_none());
        assert!(repo.purchase_order_by_id("PO-001").await.unwrap().is_none());
    }
}
