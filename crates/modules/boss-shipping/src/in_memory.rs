//! In-memory adapter for `ShippingRepository`.

use async_trait::async_trait;

use std::collections::HashSet;

use crate::port::{
    ShippingError, ShippingRepository, validate_bound, validate_shipment, validate_update,
};
use crate::summary::summarise_shipments_limited;
use crate::types::{Shipment, ShipmentDirection, ShipmentStatus};

/// The proof a scan was applied: `(shipment_id, status, occurred_on)`,
/// the key of `shipment_tracking_events`' UNIQUE constraint.
type ScanKey = (String, String, chrono::NaiveDate);

pub struct InMemoryShipping {
    shipments: std::sync::RwLock<Vec<Shipment>>,
    scans: std::sync::Mutex<HashSet<ScanKey>>,
    recorded: std::sync::Mutex<Vec<boss_core::event::Event>>,
}

/// A shipment as the store keeps it: asset ids in byte order, each once
/// — what Postgres's `shipment_assets` junction (keyed on the pair,
/// read `ORDER BY asset_id COLLATE "C"`) answers.
fn kept(s: &Shipment) -> Shipment {
    let mut asset_ids = s.asset_ids.clone();
    asset_ids.sort();
    asset_ids.dedup();
    Shipment {
        asset_ids,
        ..s.clone()
    }
}

/// Newest `created_on` first, then id in byte order — the list order
/// both adapters answer (backlog be459ab9, found by the adapters-agree
/// suite: this double answered insertion order, Postgres left ties of
/// one day to the planner).
fn in_list_order(mut v: Vec<Shipment>) -> Vec<Shipment> {
    v.sort_by(|a, b| b.created_on.cmp(&a.created_on).then(a.id.cmp(&b.id)));
    v
}

impl InMemoryShipping {
    pub fn new(shipments: Vec<Shipment>) -> Self {
        Self {
            shipments: std::sync::RwLock::new(shipments.iter().map(kept).collect()),
            scans: std::sync::Mutex::new(HashSet::new()),
            recorded: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Events the outbox paths recorded — test visibility (the
    /// in-memory analogue of the Pg adapter's in-tx recording).
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().map(|v| v.clone()).unwrap_or_default()
    }

    fn record(&self, event: boss_core::event::Event) {
        if let Ok(mut v) = self.recorded.lock() {
            v.push(event);
        }
    }
}

#[async_trait]
impl ShippingRepository for InMemoryShipping {
    async fn all_shipments(&self) -> Result<Vec<Shipment>, ShippingError> {
        Ok(in_list_order(self.shipments.read().unwrap().clone()))
    }

    async fn list_shipments(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<Shipment>, i64), ShippingError> {
        validate_bound("limit", limit)?;
        validate_bound("offset", offset)?;
        let filtered: Vec<Shipment> = in_list_order(
            self.shipments
                .read()
                .unwrap()
                .iter()
                .filter(|s| account_id.is_none() || s.account_id.as_deref() == account_id)
                .cloned()
                .collect(),
        );
        let total = filtered.len() as i64;
        let page = filtered
            .into_iter()
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .collect();
        Ok((page, total))
    }

    async fn shipment_by_id(&self, id: &str) -> Result<Option<Shipment>, ShippingError> {
        Ok(self
            .shipments
            .read()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned())
    }

    async fn create_shipment_at(
        &self,
        shipment: &Shipment,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<String, ShippingError> {
        validate_shipment(shipment)?;
        {
            let mut shipments = self.shipments.write().unwrap();
            if shipments.iter().any(|s| s.id == shipment.id) {
                return Err(ShippingError::Conflict(format!(
                    "shipment {} already exists",
                    shipment.id
                )));
            }
            shipments.push(kept(shipment));
        }
        self.record(stamp.event(
            crate::events::SHIPMENT_CREATED,
            serde_json::to_value(shipment).unwrap_or_default(),
        ));
        Ok(shipment.id.clone())
    }

    async fn update_shipment_at(
        &self,
        id: &str,
        shipment: &Shipment,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), ShippingError> {
        validate_update(id, shipment)?;
        {
            let mut shipments = self.shipments.write().unwrap();
            let pos = shipments
                .iter()
                .position(|s| s.id == id)
                .ok_or_else(|| ShippingError::NotFound(id.to_string()))?;
            shipments[pos] = kept(shipment);
        }
        self.record(stamp.event(
            crate::events::SHIPMENT_UPDATED,
            serde_json::to_value(shipment).unwrap_or_default(),
        ));
        Ok(())
    }

    async fn delete_shipment_at(
        &self,
        id: &str,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), ShippingError> {
        {
            let mut shipments = self.shipments.write().unwrap();
            let pos = shipments
                .iter()
                .position(|s| s.id == id)
                .ok_or_else(|| ShippingError::NotFound(id.to_string()))?;
            shipments.remove(pos);
        }
        // The scans go with the shipment, as `ON DELETE CASCADE` takes
        // them in Postgres.
        self.scans.lock().unwrap().retain(|(sid, _, _)| sid != id);
        self.record(stamp.event(
            crate::events::SHIPMENT_DELETED,
            serde_json::json!({ "id": id, "deleted_at": now }),
        ));
        Ok(())
    }

    /// Until 2026-10-01 a stub answering `Ok` for any shipment, rolling
    /// nothing up and recording nothing (backlog be459ab9, found by the
    /// adapters-agree suite). It now keeps the proof, the rollup and the
    /// fact Postgres keeps, in the same order of judgement.
    async fn record_tracking_scan(
        &self,
        shipment_id: &str,
        status: &str,
        occurred_on: chrono::NaiveDate,
        stage_index: Option<i16>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), ShippingError> {
        if status.contains('\0') {
            return Err(ShippingError::Invalid(
                "status carries a NUL byte, which cannot be stored".into(),
            ));
        }
        {
            let mut shipments = self.shipments.write().unwrap();
            let row = shipments
                .iter_mut()
                .find(|s| s.id == shipment_id)
                .ok_or_else(|| ShippingError::NotFound(shipment_id.to_string()))?;
            let key = (shipment_id.to_string(), status.to_string(), occurred_on);
            // The proof of application: a replayed scan is a full no-op.
            if !self.scans.lock().unwrap().insert(key) {
                return Ok(());
            }
            if matches!(
                status,
                ShipmentStatus::IN_TRANSIT | ShipmentStatus::DELIVERED
            ) {
                row.status = ShipmentStatus::new(status);
                row.shipped_on = row.shipped_on.or(Some(occurred_on));
                if status == ShipmentStatus::DELIVERED {
                    row.delivered_on = row.delivered_on.or(Some(occurred_on));
                }
            }
        }
        self.record(stamp.event(
            crate::events::TRACKING_RECORDED,
            serde_json::json!({
                "shipment_id": shipment_id,
                "status": status,
                "occurred_on": occurred_on,
                "stage_index": stage_index,
            }),
        ));
        Ok(())
    }

    async fn status_summary(
        &self,
        direction: ShipmentDirection,
        today: chrono::NaiveDate,
        recent_limit: i64,
    ) -> Result<boss_shipping_client::OutboundShipmentSummary, ShippingError> {
        validate_bound("recent_limit", recent_limit)?;
        let shipments = self.shipments.read().unwrap();
        // The caller's limit, not a hard ten: until 2026-10-01 this
        // double capped the preview at STATUS_SUMMARY_RECENT_LIMIT
        // whatever was asked, while Postgres honoured the limit
        // (backlog be459ab9, found by the adapters-agree suite).
        Ok(summarise_shipments_limited(
            &shipments,
            direction,
            today,
            usize::try_from(recent_limit).unwrap_or(usize::MAX),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn test_shipment(id: &str) -> Shipment {
        Shipment {
            id: id.to_string(),
            direction: ShipmentDirection::Outbound,
            status: ShipmentStatus::IN_TRANSIT.into(),
            carrier: Some(Carrier::new("fedex")),
            tracking_number: Some("1Z999AA10123456784".to_string()),
            origin: "HQ Warehouse".to_string(),
            destination: "Account Alpha".to_string(),
            asset_ids: vec!["SN-001".to_string(), "SN-002".to_string()],
            line_items: Vec::new(),
            po_id: Some("PO-100".to_string()),
            order_id: Some("ORD-200".to_string()),
            account_id: Some("account-001".to_string()),
            created_on: chrono::NaiveDate::from_ymd_opt(2025, 6, 1).unwrap(),
            shipped_on: Some(chrono::NaiveDate::from_ymd_opt(2025, 6, 2).unwrap()),
            estimated_delivery: Some(chrono::NaiveDate::from_ymd_opt(2025, 6, 5).unwrap()),
            delivered_on: None,
        }
    }

    fn test_repo() -> InMemoryShipping {
        InMemoryShipping::new(vec![
            test_shipment("ship-001"),
            test_shipment("ship-002"),
            test_shipment("ship-003"),
        ])
    }

    #[tokio::test]
    async fn all_shipments_returns_all() {
        let repo = test_repo();
        assert_eq!(repo.all_shipments().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn shipment_by_id_found() {
        let repo = test_repo();
        let ship = repo.shipment_by_id("ship-001").await.unwrap();
        assert!(ship.is_some());
        assert_eq!(ship.unwrap().id, "ship-001");
    }

    #[tokio::test]
    async fn shipment_by_id_not_found() {
        let repo = test_repo();
        assert!(repo.shipment_by_id("ship-999").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn create_shipment_adds() {
        let repo = InMemoryShipping::new(vec![]);
        let ship = test_shipment("NEW-1");
        let id = repo.create_shipment(&ship).await.unwrap();
        assert_eq!(id, "NEW-1");
        assert_eq!(repo.all_shipments().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn create_duplicate_fails() {
        let repo = InMemoryShipping::new(vec![test_shipment("ship-001")]);
        let result = repo.create_shipment(&test_shipment("ship-001")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shipment_carries_line_items_roundtrip() {
        let repo = InMemoryShipping::new(vec![]);
        let mut ship = test_shipment("WK-001");
        ship.line_items = vec![
            crate::types::ShipmentLineItem {
                sku: "FP-PALE-1-2-BBL".into(),
                qty: 12,
                unit_price_cents: Some(13500),
                description: Some("Pale Ale half-barrel keg".into()),
            },
            crate::types::ShipmentLineItem {
                sku: "FP-IPA-1-6-BBL".into(),
                qty: 8,
                unit_price_cents: Some(5500),
                description: None,
            },
        ];
        repo.create_shipment(&ship).await.unwrap();
        let fetched = repo.shipment_by_id("WK-001").await.unwrap().unwrap();
        assert_eq!(fetched.line_items.len(), 2);
        assert_eq!(fetched.line_items[0].sku, "FP-PALE-1-2-BBL");
        assert_eq!(fetched.line_items[0].qty, 12);
        assert_eq!(fetched.line_items[1].sku, "FP-IPA-1-6-BBL");
    }

    #[tokio::test]
    async fn update_shipment_replaces() {
        let repo = InMemoryShipping::new(vec![test_shipment("ship-001")]);
        let mut updated = test_shipment("ship-001");
        updated.origin = "New Origin".to_string();
        repo.update_shipment("ship-001", &updated).await.unwrap();
        let fetched = repo.shipment_by_id("ship-001").await.unwrap().unwrap();
        assert_eq!(fetched.origin, "New Origin");
    }

    #[tokio::test]
    async fn update_nonexistent_fails() {
        let repo = InMemoryShipping::new(vec![]);
        let result = repo.update_shipment("NOPE", &test_shipment("NOPE")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_shipment_removes() {
        let repo =
            InMemoryShipping::new(vec![test_shipment("ship-001"), test_shipment("ship-002")]);
        repo.delete_shipment("ship-001").await.unwrap();
        assert_eq!(repo.all_shipments().await.unwrap().len(), 1);
        assert!(repo.shipment_by_id("ship-001").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_nonexistent_fails() {
        let repo = InMemoryShipping::new(vec![]);
        let result = repo.delete_shipment("NOPE").await;
        assert!(result.is_err());
    }
}
