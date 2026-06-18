//! In-memory adapter for `ShippingRepository`.

use async_trait::async_trait;

use crate::port::{ShippingError, ShippingRepository};
use crate::summary::summarise_shipments;
use crate::types::{Shipment, ShipmentDirection};

pub struct InMemoryShipping {
    shipments: std::sync::RwLock<Vec<Shipment>>,
}

impl InMemoryShipping {
    pub fn new(shipments: Vec<Shipment>) -> Self {
        Self {
            shipments: std::sync::RwLock::new(shipments),
        }
    }
}

#[async_trait]
impl ShippingRepository for InMemoryShipping {
    async fn all_shipments(&self) -> Result<Vec<Shipment>, ShippingError> {
        Ok(self.shipments.read().unwrap().clone())
    }

    async fn list_shipments(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<Shipment>, i64), ShippingError> {
        let shipments = self.shipments.read().unwrap();
        let filtered: Vec<&Shipment> = match account_id {
            Some(cid) => shipments
                .iter()
                .filter(|s| s.account_id.as_deref() == Some(cid))
                .collect(),
            None => shipments.iter().collect(),
        };
        let total = filtered.len() as i64;
        let start = (offset as usize).min(filtered.len());
        let end = (start + limit as usize).min(filtered.len());
        Ok((
            filtered[start..end].iter().map(|&s| s.clone()).collect(),
            total,
        ))
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
    ) -> Result<String, ShippingError> {
        let mut shipments = self.shipments.write().unwrap();
        if shipments.iter().any(|s| s.id == shipment.id) {
            return Err(ShippingError::Conflict(format!(
                "shipment {} already exists",
                shipment.id
            )));
        }
        let id = shipment.id.clone();
        shipments.push(shipment.clone());
        Ok(id)
    }

    async fn update_shipment_at(
        &self,
        id: &str,
        shipment: &Shipment,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), ShippingError> {
        let mut shipments = self.shipments.write().unwrap();
        let pos = shipments
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| ShippingError::NotFound(id.to_string()))?;
        shipments[pos] = shipment.clone();
        Ok(())
    }

    async fn delete_shipment(&self, id: &str) -> Result<(), ShippingError> {
        let mut shipments = self.shipments.write().unwrap();
        let pos = shipments
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| ShippingError::NotFound(id.to_string()))?;
        shipments.remove(pos);
        Ok(())
    }

    async fn record_tracking_scan(
        &self,
        _shipment_id: &str,
        _status: &str,
        _occurred_on: chrono::NaiveDate,
        _stage_index: Option<i16>,
    ) -> Result<(), ShippingError> {
        // In-memory backend: tracking events are not persisted.
        // Tests that exercise the live flow use the Postgres path.
        Ok(())
    }

    async fn status_summary(
        &self,
        direction: ShipmentDirection,
        today: chrono::NaiveDate,
        recent_limit: i64,
    ) -> Result<boss_shipping_client::OutboundShipmentSummary, ShippingError> {
        let shipments = self.shipments.read().unwrap();
        let mut summary = summarise_shipments(&shipments, direction, today);
        // The pure `summarise_shipments` hardcodes the preview cap at
        // STATUS_SUMMARY_RECENT_LIMIT (10). If the caller wanted less,
        // honour that here. The port's limit is an upper bound, not a
        // target — we don't pad up.
        if (recent_limit as usize) < summary.recent.len() {
            summary.recent.truncate(recent_limit as usize);
        }
        Ok(summary)
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
