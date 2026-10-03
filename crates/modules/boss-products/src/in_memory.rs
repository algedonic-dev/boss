//! In-memory adapter for `ProductsRepository` — the test double (no
//! mocks; a real implementation of the port, minus durability and the
//! ledger's journal). Held to Postgres by
//! `tests/the_adapters_agree_on_the_products_store_pg.rs` (backlog
//! be459ab9): it records the same facts, answers the same GL moves and
//! the same canonical fact ids, and keeps the same proof of application
//! that makes a redelivered produce or consume a no-op.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::publisher::EventStamp;

use crate::delta;
use crate::port::{
    InventoryDeltaResult, ProductsError, ProductsRepository, refuse_nul, refuse_nul_in_product,
    unregistered,
};
use crate::types::{JeRecorded, Product, ProductInventory};

#[derive(Default)]
struct State {
    products: BTreeMap<String, Product>,
    inventory: BTreeMap<(String, String), ProductInventory>,
    /// The `financial_facts` natural keys `(kind, source_table,
    /// source_id)` written — Postgres's unique index, and the proof of
    /// application its produce / consume guard reads.
    facts: BTreeSet<(String, String, String)>,
    recorded: Vec<Event>,
}

#[derive(Default)]
pub struct InMemoryProducts {
    state: Mutex<State>,
}

impl InMemoryProducts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Events the write paths recorded — test visibility (the in-memory
    /// analogue of the Pg adapter's in-tx outbox write).
    pub fn recorded_events(&self) -> Vec<Event> {
        self.state
            .lock()
            .map(|s| s.recorded.clone())
            .unwrap_or_default()
    }

    fn state(&self) -> Result<MutexGuard<'_, State>, ProductsError> {
        self.state
            .lock()
            .map_err(|_| ProductsError::Storage("in-memory products lock poisoned".into()))
    }
}

fn fact_key(kind: &str, source_table: &str, source_id: &str) -> (String, String, String) {
    (kind.into(), source_table.into(), source_id.into())
}

fn record(state: &mut State, stamp: &EventStamp, kind: &str, payload: serde_json::Value) {
    state.recorded.push(stamp.event(kind, payload));
}

fn record_row(state: &mut State, stamp: &EventStamp, row: &ProductInventory) {
    record(
        state,
        stamp,
        crate::events::PRODUCT_INVENTORY_UPSERTED,
        delta::inventory_upserted(row),
    );
}

/// The row as the table keeps it: per-unit cost derived, `updated_at`
/// the write's instant to the microsecond.
fn settle(row: &mut ProductInventory, stamp: &EventStamp) {
    row.production_cost_cents = delta::unit_cost(row.on_hand, row.value_cents);
    row.updated_at = Some(delta::as_stored(stamp.timestamp));
}

#[async_trait]
impl ProductsRepository for InMemoryProducts {
    async fn list_products(&self, active_only: bool) -> Result<Vec<Product>, ProductsError> {
        // BTreeMap<String, _> iterates in byte order of SKU.
        Ok(self
            .state()?
            .products
            .values()
            .filter(|p| !active_only || p.active)
            .cloned()
            .collect())
    }

    async fn get_product(&self, sku: &str) -> Result<Option<Product>, ProductsError> {
        Ok(self.state()?.products.get(sku).cloned())
    }

    async fn upsert_product(
        &self,
        product: &Product,
        stamp: &EventStamp,
    ) -> Result<(), ProductsError> {
        refuse_nul_in_product(product)?;
        let mut state = self.state()?;
        state.products.insert(product.sku.clone(), product.clone());
        record(
            &mut state,
            stamp,
            crate::events::PRODUCT_UPSERTED,
            serde_json::to_value(product).unwrap_or_default(),
        );
        Ok(())
    }

    async fn inventory_for(&self, sku: &str) -> Result<Vec<ProductInventory>, ProductsError> {
        // Keyed (sku, location): one SKU's rows iterate in byte order
        // of location.
        Ok(self
            .state()?
            .inventory
            .values()
            .filter(|r| r.product_sku == sku)
            .cloned()
            .collect())
    }

    async fn upsert_inventory(
        &self,
        row: &ProductInventory,
        stamp: &EventStamp,
    ) -> Result<(), ProductsError> {
        refuse_nul(&[
            ("product_sku", &row.product_sku),
            ("location_id", &row.location_id),
        ])?;
        let mut state = self.state()?;
        if !state.products.contains_key(&row.product_sku) {
            return Err(unregistered(&row.product_sku));
        }
        let mut stored = row.clone();
        settle(&mut stored, stamp);
        // The fact carries the row as stored, as Postgres's does
        // (backlog 797b6168).
        record_row(&mut state, stamp, &stored);
        state
            .inventory
            .insert((row.product_sku.clone(), row.location_id.clone()), stored);
        Ok(())
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
    ) -> Result<JeRecorded, ProductsError> {
        if total_cost_cents <= 0 {
            return Err(ProductsError::Invalid(
                "total_cost_cents must be positive".to_string(),
            ));
        }
        refuse_nul(&[
            ("debit_account", debit_account),
            ("credit_account", credit_account),
            ("memo", memo),
            ("source_table", source_table),
            ("source_id", source_id),
        ])?;
        let payload = delta::je_payload(
            total_cost_cents,
            debit_account,
            credit_account,
            memo,
            source_table,
            source_id,
            happened_on,
        );
        let mut state = self.state()?;
        let inserted = state
            .facts
            .insert(fact_key(delta::TRANSFER_FACT, source_table, source_id));
        if inserted {
            record(
                &mut state,
                stamp,
                crate::events::LEDGER_INVENTORY_TRANSFERRED,
                payload.clone(),
            );
        }
        Ok(JeRecorded {
            fact_id: boss_ledger::deterministic_fact_id(
                delta::TRANSFER_FACT,
                source_table,
                source_id,
            ),
            inserted,
            payload,
        })
    }

    async fn produce(
        &self,
        sku: &str,
        location_id: &str,
        qty: i32,
        total_cost_cents: Option<i64>,
        now: chrono::DateTime<chrono::Utc>,
        source_id: String,
        stamp: &EventStamp,
    ) -> Result<InventoryDeltaResult, ProductsError> {
        if qty <= 0 {
            return Err(ProductsError::Invalid(format!(
                "produce qty must be positive, got {qty}"
            )));
        }
        refuse_nul(&[
            ("sku", sku),
            ("location_id", location_id),
            ("source_id", &source_id),
        ])?;
        let mut state = self.state()?;
        let key = (sku.to_string(), location_id.to_string());
        let proof = fact_key(delta::TRANSFER_FACT, delta::PRODUCE_SOURCE, &source_id);
        // Idempotency guard, as Postgres's: the fact is the proof of
        // application.
        if state.facts.contains(&proof)
            && let Some(row) = state.inventory.get(&key)
        {
            return Ok(InventoryDeltaResult {
                inventory: row.clone(),
                gl_move: None,
            });
        }
        if !state.products.contains_key(sku) {
            return Err(unregistered(sku));
        }
        let costed = total_cost_cents.filter(|t| *t > 0);
        let mut row = state
            .inventory
            .get(&key)
            .cloned()
            .unwrap_or_else(|| ProductInventory {
                product_sku: sku.to_string(),
                location_id: location_id.to_string(),
                on_hand: 0,
                reserved: 0,
                value_cents: 0,
                production_cost_cents: 0,
                updated_at: None,
            });
        // Value-primary: the exact line total lands on the row.
        row.on_hand += qty;
        row.value_cents += costed.unwrap_or(0);
        settle(&mut row, stamp);
        state.inventory.insert(key, row.clone());
        let gl_move =
            costed.map(|total| delta::produced_move(sku, location_id, qty, total, now, source_id));
        if gl_move.is_some() {
            state.facts.insert(proof);
        }
        record_row(&mut state, stamp, &row);
        if let Some(gl) = &gl_move {
            record(
                &mut state,
                stamp,
                crate::events::PRODUCT_PRODUCED,
                gl.payload.clone(),
            );
        }
        Ok(InventoryDeltaResult {
            inventory: row,
            gl_move,
        })
    }

    async fn consume(
        &self,
        sku: &str,
        location_id: &str,
        qty: i32,
        revenue_category: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
        source_id: String,
        stamp: &EventStamp,
    ) -> Result<InventoryDeltaResult, ProductsError> {
        if qty <= 0 {
            return Err(ProductsError::Invalid(format!(
                "consume qty must be positive, got {qty}"
            )));
        }
        refuse_nul(&[
            ("sku", sku),
            ("location_id", location_id),
            ("source_id", &source_id),
            ("revenue_category", revenue_category.unwrap_or("")),
        ])?;
        let mut state = self.state()?;
        let key = (sku.to_string(), location_id.to_string());
        let proof = fact_key(delta::COGS_FACT, delta::CONSUME_SOURCE, &source_id);
        if state.facts.contains(&proof)
            && let Some(row) = state.inventory.get(&key)
        {
            return Ok(InventoryDeltaResult {
                inventory: row.clone(),
                gl_move: None,
            });
        }
        let mut row = match state.inventory.get(&key) {
            Some(row) if row.on_hand >= qty => row.clone(),
            _ => {
                return Err(ProductsError::Invalid(format!(
                    "consume failed: row missing or on_hand < {qty} for {sku} @ {location_id}"
                )));
            }
        };
        let drained = delta::drained_cents(row.on_hand, row.value_cents, qty);
        row.on_hand -= qty;
        row.value_cents -= drained;
        settle(&mut row, stamp);
        state.inventory.insert(key, row.clone());
        // A zero drain (a row seeded without a cost basis) moves no GL.
        let gl_move = (drained > 0).then(|| {
            delta::consumed_move(
                sku,
                location_id,
                qty,
                drained,
                revenue_category,
                now,
                source_id,
            )
        });
        if gl_move.is_some() {
            state.facts.insert(proof);
        }
        record_row(&mut state, stamp, &row);
        if let Some(gl) = &gl_move {
            record(
                &mut state,
                stamp,
                crate::events::PRODUCT_CONSUMED,
                gl.payload.clone(),
            );
        }
        Ok(InventoryDeltaResult {
            inventory: row,
            gl_move,
        })
    }
}
