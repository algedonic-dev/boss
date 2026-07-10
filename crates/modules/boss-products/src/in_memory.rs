//! In-memory adapter for `ProductsRepository`. Used by HTTP tests
//! and any caller that doesn't want a Postgres dependency.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::port::{InventoryDeltaResult, ProductsError, ProductsRepository};
use crate::types::{Product, ProductInventory};

#[derive(Default)]
pub struct InMemoryProducts {
    products: Mutex<BTreeMap<String, Product>>,
    inventory: Mutex<BTreeMap<(String, String), ProductInventory>>,
}

impl InMemoryProducts {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ProductsRepository for InMemoryProducts {
    async fn list_products(&self, active_only: bool) -> Result<Vec<Product>, ProductsError> {
        let map = self.products.lock().unwrap();
        let mut out: Vec<Product> = map
            .values()
            .filter(|p| !active_only || p.active)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.sku.cmp(&b.sku));
        Ok(out)
    }

    async fn get_product(&self, sku: &str) -> Result<Option<Product>, ProductsError> {
        Ok(self.products.lock().unwrap().get(sku).cloned())
    }

    async fn upsert_product(&self, product: &Product) -> Result<(), ProductsError> {
        self.products
            .lock()
            .unwrap()
            .insert(product.sku.clone(), product.clone());
        Ok(())
    }

    async fn inventory_for(&self, sku: &str) -> Result<Vec<ProductInventory>, ProductsError> {
        let map = self.inventory.lock().unwrap();
        let mut out: Vec<ProductInventory> = map
            .values()
            .filter(|r| r.product_sku == sku)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.location_id.cmp(&b.location_id));
        Ok(out)
    }

    async fn upsert_inventory(&self, row: &ProductInventory) -> Result<(), ProductsError> {
        self.inventory.lock().unwrap().insert(
            (row.product_sku.clone(), row.location_id.clone()),
            row.clone(),
        );
        Ok(())
    }

    async fn record_inventory_je(
        &self,
        _total_cost_cents: i64,
        _debit_account: &str,
        _credit_account: &str,
        _memo: &str,
        _source_table: &str,
        _source_id: &str,
        _happened_on: chrono::NaiveDate,
    ) -> Result<crate::types::JeRecorded, ProductsError> {
        Ok(crate::types::JeRecorded {
            fact_id: uuid::Uuid::new_v4(),
            inserted: true,
            payload: serde_json::Value::Null,
        })
    }

    async fn produce(
        &self,
        sku: &str,
        location_id: &str,
        qty: i32,
        total_cost_cents: Option<i64>,
        _now: chrono::DateTime<chrono::Utc>,
        _source_id: String,
    ) -> Result<InventoryDeltaResult, ProductsError> {
        if qty <= 0 {
            return Err(ProductsError::Invalid(format!(
                "produce qty must be positive, got {qty}"
            )));
        }
        let mut map = self.inventory.lock().unwrap();
        let key = (sku.to_string(), location_id.to_string());
        let row = map.entry(key).or_insert_with(|| ProductInventory {
            product_sku: sku.to_string(),
            location_id: location_id.to_string(),
            on_hand: 0,
            reserved: 0,
            value_cents: 0,
            production_cost_cents: 0,
            updated_at: None,
        });
        // Value-primary: the exact line total lands on the row.
        if let Some(total) = total_cost_cents
            && total > 0
        {
            row.value_cents += total;
        }
        row.on_hand += qty;
        row.production_cost_cents = if row.on_hand > 0 {
            row.value_cents / row.on_hand as i64
        } else {
            0
        };
        // In-memory adapter doesn't carry a GL — tests that need to
        // assert the GL move drive the Postgres adapter directly.
        Ok(InventoryDeltaResult {
            inventory: row.clone(),
            gl_move: None,
        })
    }

    async fn consume(
        &self,
        sku: &str,
        location_id: &str,
        qty: i32,
        _revenue_category: Option<&str>,
        _now: chrono::DateTime<chrono::Utc>,
        _source_id: String,
    ) -> Result<InventoryDeltaResult, ProductsError> {
        if qty <= 0 {
            return Err(ProductsError::Invalid(format!(
                "consume qty must be positive, got {qty}"
            )));
        }
        let mut map = self.inventory.lock().unwrap();
        let key = (sku.to_string(), location_id.to_string());
        match map.get_mut(&key) {
            Some(row) if row.on_hand >= qty => {
                // Proportional drain, final unit takes the remainder —
                // mirrors the Postgres adapter.
                let drained = if row.on_hand == qty {
                    row.value_cents
                } else {
                    (((row.value_cents as i128) * (qty as i128) + (row.on_hand as i128) / 2)
                        / (row.on_hand as i128)) as i64
                };
                row.on_hand -= qty;
                row.value_cents -= drained;
                row.production_cost_cents = if row.on_hand > 0 {
                    row.value_cents / row.on_hand as i64
                } else {
                    0
                };
                Ok(InventoryDeltaResult {
                    inventory: row.clone(),
                    gl_move: None,
                })
            }
            _ => Err(ProductsError::Invalid(format!(
                "consume failed: row missing or on_hand < {qty} for {sku} @ {location_id}"
            ))),
        }
    }
}
