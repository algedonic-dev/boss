//! In-memory adapter for `InventoryRepository`.

use async_trait::async_trait;

use std::sync::RwLock;

use crate::port::{InventoryError, InventoryRepository};
use crate::types::{
    ApAging, ApAgingBucket, ConsumeApplied, InventoryItem, JeRecorded, PurchaseOrder,
    ReceiveApplied, Vendor, VendorInvoice, VendorInvoiceStatus,
};

pub struct InMemoryInventory {
    items: Vec<InventoryItem>,
    purchase_orders: RwLock<Vec<PurchaseOrder>>,
    vendors: RwLock<Vec<Vendor>>,
    vendor_invoices: RwLock<Vec<VendorInvoice>>,
}

impl InMemoryInventory {
    pub fn new(items: Vec<InventoryItem>, purchase_orders: Vec<PurchaseOrder>) -> Self {
        Self {
            items,
            purchase_orders: RwLock::new(purchase_orders),
            vendors: RwLock::new(Vec::new()),
            vendor_invoices: RwLock::new(Vec::new()),
        }
    }

    pub fn with_vendors(
        items: Vec<InventoryItem>,
        purchase_orders: Vec<PurchaseOrder>,
        vendors: Vec<Vendor>,
    ) -> Self {
        Self {
            items,
            purchase_orders: RwLock::new(purchase_orders),
            vendors: RwLock::new(vendors),
            vendor_invoices: RwLock::new(Vec::new()),
        }
    }
}

#[async_trait]
impl InventoryRepository for InMemoryInventory {
    async fn all_items(&self) -> Result<Vec<InventoryItem>, InventoryError> {
        Ok(self.items.clone())
    }

    async fn item_by_sku(&self, part_sku: &str) -> Result<Option<InventoryItem>, InventoryError> {
        Ok(self.items.iter().find(|i| i.part_sku == part_sku).cloned())
    }

    async fn upsert_item_at(
        &self,
        _item: &InventoryItem,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), InventoryError> {
        // In-memory tests seed items via the constructor; upsert is a
        // no-op here because `self.items` is not locked.
        Ok(())
    }

    async fn all_purchase_orders(&self) -> Result<Vec<PurchaseOrder>, InventoryError> {
        let pos = self
            .purchase_orders
            .read()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(pos.clone())
    }

    async fn purchase_order_by_id(
        &self,
        id: &str,
    ) -> Result<Option<PurchaseOrder>, InventoryError> {
        let pos = self
            .purchase_orders
            .read()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(pos.iter().find(|po| po.id == id).cloned())
    }

    async fn consume_part_at(
        &self,
        part_sku: &str,
        _qty: u32,
        _now: chrono::DateTime<chrono::Utc>,
        _source_id: &str,
    ) -> Result<ConsumeApplied, InventoryError> {
        // In-memory: no mutation support, just return the item (no
        // fact written → no payload to emit).
        let item = self
            .item_by_sku(part_sku)
            .await?
            .ok_or_else(|| InventoryError::NotFound(part_sku.to_string()))?;
        Ok(ConsumeApplied {
            item,
            fact_payload: None,
        })
    }
    async fn inbound_reserved_for_part(&self, _part_sku: &str) -> Result<i64, InventoryError> {
        // In-memory adapter has no Job/step state to project across;
        // tests that exercise the auto-restock trigger drive the
        // postgres adapter.
        Ok(0)
    }
    async fn open_po_exists_for_part(&self, _part_sku: &str) -> Result<bool, InventoryError> {
        Ok(false)
    }
    async fn primary_vendor_for_part(
        &self,
        _part_sku: &str,
    ) -> Result<Option<String>, InventoryError> {
        Ok(None)
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
    ) -> Result<JeRecorded, InventoryError> {
        Ok(JeRecorded {
            fact_id: uuid::Uuid::new_v4(),
            inserted: true,
            payload: serde_json::Value::Null,
        })
    }

    async fn record_overhead_absorbed(
        &self,
        _total_cost_cents: i64,
        _debit_account: &str,
        _credit_account: &str,
        _memo: &str,
        _source_id: &str,
        _happened_on: chrono::NaiveDate,
    ) -> Result<(uuid::Uuid, bool), InventoryError> {
        // No ledger / financial_facts in the in-memory adapter — tests
        // that exercise burden absorption use the postgres adapter.
        // Returning a fresh UUID keeps the trait contract intact.
        Ok((uuid::Uuid::new_v4(), true))
    }

    async fn receive_part_at(
        &self,
        part_sku: &str,
        _qty: u32,
        _unit_cost_cents: Option<i64>,
        _now: chrono::DateTime<chrono::Utc>,
        _source_id: &str,
    ) -> Result<ReceiveApplied, InventoryError> {
        // In-memory: no mutation support, just return the item (no
        // fact written → no payload → caller emits nothing).
        let item = self
            .item_by_sku(part_sku)
            .await?
            .ok_or_else(|| InventoryError::NotFound(part_sku.to_string()))?;
        Ok(ReceiveApplied {
            item,
            receipt_payload: None,
        })
    }

    async fn create_purchase_order_at(
        &self,
        po: &PurchaseOrder,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), InventoryError> {
        let mut pos = self
            .purchase_orders
            .write()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        if pos.iter().any(|existing| existing.id == po.id) {
            return Ok(());
        }
        pos.push(po.clone());
        Ok(())
    }

    async fn update_po_status(&self, _id: &str, _status: &str) -> Result<(), InventoryError> {
        Ok(())
    }

    async fn all_vendors(&self) -> Result<Vec<Vendor>, InventoryError> {
        let vendors = self
            .vendors
            .read()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(vendors.clone())
    }

    async fn create_vendor_at(
        &self,
        vendor: &Vendor,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, InventoryError> {
        let mut vendors = self
            .vendors
            .write()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        if vendors.iter().any(|v| v.id == vendor.id) {
            return Err(InventoryError::Conflict(format!(
                "vendor {} already exists",
                vendor.id
            )));
        }
        vendors.push(vendor.clone());
        Ok(vendor.id.clone())
    }

    async fn update_vendor(&self, id: &str, vendor: &Vendor) -> Result<(), InventoryError> {
        let mut vendors = self
            .vendors
            .write()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        let existing = vendors
            .iter_mut()
            .find(|v| v.id == id)
            .ok_or_else(|| InventoryError::NotFound(format!("vendor {id}")))?;
        *existing = vendor.clone();
        Ok(())
    }

    async fn delete_vendor(&self, id: &str) -> Result<(), InventoryError> {
        let mut vendors = self
            .vendors
            .write()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        let len_before = vendors.len();
        vendors.retain(|v| v.id != id);
        if vendors.len() == len_before {
            return Err(InventoryError::NotFound(format!("vendor {id}")));
        }
        Ok(())
    }

    async fn upsert_vendor_invoice_at(
        &self,
        invoice: &VendorInvoice,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), InventoryError> {
        let mut rows = self
            .vendor_invoices
            .write()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        if let Some(existing) = rows.iter_mut().find(|v| v.id == invoice.id) {
            *existing = invoice.clone();
        } else {
            rows.push(invoice.clone());
        }
        Ok(())
    }

    async fn all_vendor_invoices(
        &self,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<VendorInvoice>, InventoryError> {
        let rows = self
            .vendor_invoices
            .read()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        let mut filtered: Vec<VendorInvoice> = rows
            .iter()
            .filter(|v| match status {
                Some(s) => v.status.as_str() == s,
                None => true,
            })
            .cloned()
            .collect();
        filtered.sort_by_key(|i| std::cmp::Reverse(i.received_on));
        filtered.truncate(limit.max(0) as usize);
        Ok(filtered)
    }

    async fn vendor_invoice_by_id(
        &self,
        id: &str,
    ) -> Result<Option<VendorInvoice>, InventoryError> {
        let rows = self
            .vendor_invoices
            .read()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(rows.iter().find(|v| v.id == id).cloned())
    }

    async fn ap_aging(&self, today: chrono::NaiveDate) -> Result<ApAging, InventoryError> {
        let rows = self
            .vendor_invoices
            .read()
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        let mut buckets: std::collections::HashMap<&'static str, (i64, i64)> =
            std::collections::HashMap::new();
        let mut total_outstanding: i64 = 0;
        let mut total_count: i64 = 0;
        for v in rows.iter() {
            if matches!(v.status, VendorInvoiceStatus::Paid) {
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
            status: PoStatus::Submitted,
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
