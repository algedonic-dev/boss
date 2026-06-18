//! In-memory adapter for `KbRepository`.
//!
//! Useful for tests and as a seed-data fallback.

use std::sync::RwLock;

use async_trait::async_trait;

use crate::port::{KbError, KbRepository};
use crate::types::AssetModel;

pub struct InMemoryKb {
    models: RwLock<Vec<AssetModel>>,
}

impl InMemoryKb {
    pub fn new(models: Vec<AssetModel>) -> Self {
        Self {
            models: RwLock::new(models),
        }
    }
}

#[async_trait]
impl KbRepository for InMemoryKb {
    async fn all_models(&self) -> Result<Vec<AssetModel>, KbError> {
        Ok(self.models.read().unwrap().clone())
    }

    async fn model_by_sku(&self, sku: &str) -> Result<Option<AssetModel>, KbError> {
        Ok(self
            .models
            .read()
            .unwrap()
            .iter()
            .find(|m| m.sku == sku)
            .cloned())
    }

    async fn create_model_at(
        &self,
        model: &AssetModel,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, KbError> {
        let mut models = self.models.write().unwrap();
        if models.iter().any(|m| m.sku == model.sku) {
            return Err(KbError::Conflict(format!(
                "SKU {} already exists",
                model.sku
            )));
        }
        let sku = model.sku.clone();
        models.push(model.clone());
        Ok(sku)
    }

    async fn update_model_at(
        &self,
        sku: &str,
        model: &AssetModel,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), KbError> {
        let mut models = self.models.write().unwrap();
        let pos = models
            .iter()
            .position(|m| m.sku == sku)
            .ok_or_else(|| KbError::NotFound(sku.to_string()))?;
        models[pos] = model.clone();
        Ok(())
    }

    async fn delete_model(&self, sku: &str) -> Result<(), KbError> {
        let mut models = self.models.write().unwrap();
        let pos = models
            .iter()
            .position(|m| m.sku == sku)
            .ok_or_else(|| KbError::NotFound(sku.to_string()))?;
        models.remove(pos);
        Ok(())
    }

    async fn documents_for(
        &self,
        _entity_kind: &str,
        _entity_id: &str,
    ) -> Result<Vec<crate::types::EntityDocument>, KbError> {
        // In-memory adapter has no `documents` table; tests that
        // exercise document surfacing drive the postgres adapter.
        Ok(Vec::new())
    }

    async fn all_parts(&self) -> Result<Vec<crate::types::PartCatalogRow>, KbError> {
        // The in-memory adapter doesn't carry a separate parts
        // store — derive from the spare_parts/consumables of
        // every registered model. Tests that need richer parts
        // coverage can wrap this with a custom adapter.
        use crate::types::PartCatalogRow;
        use std::collections::HashMap;
        let models = self.models.read().unwrap();
        let mut by_sku: HashMap<String, PartCatalogRow> = HashMap::new();
        for m in models.iter() {
            for p in &m.spare_parts {
                by_sku.entry(p.part_sku.clone()).or_insert(PartCatalogRow {
                    part_sku: p.part_sku.clone(),
                    name: p.name.clone(),
                    description: p.description.clone(),
                    unit_price_cents: p.unit_price_cents,
                    currency: p.currency.clone(),
                    lead_time_days: p.lead_time_days,
                });
            }
            for c in &m.consumables {
                by_sku.entry(c.part_sku.clone()).or_insert(PartCatalogRow {
                    part_sku: c.part_sku.clone(),
                    name: c.name.clone(),
                    description: c.description.clone(),
                    unit_price_cents: c.unit_price_cents,
                    currency: c.currency.clone(),
                    lead_time_days: 7,
                });
            }
        }
        let mut out: Vec<PartCatalogRow> = by_sku.into_values().collect();
        out.sort_by(|a, b| a.part_sku.cmp(&b.part_sku));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn test_model(sku: &str) -> AssetModel {
        AssetModel {
            sku: sku.to_string(),
            name: "Test Device".to_string(),
            manufacturer: "TestCo".to_string(),
            model_year: 2024,
            category: DeviceCategory::new("router"),
            extras: serde_json::json!({"port_count": 24}),
            physical: Physical {
                width_cm: 50.0,
                depth_cm: 50.0,
                height_cm: 100.0,
                weight_kg: 80.0,
                power_requirements: "120V".to_string(),
            },
            regulatory: Regulatory {
                clearance_id: None,
                clearance_date: None,
                regulator_device_class: 2,
            },
            commerce: Commerce {
                list_price_new_cents: 5_000_000,
                typical_refurb_price_cents: None,
                currency: "USD".to_string(),
                lead_time_days: None,
                tagline: "A test device".to_string(),
                description: "For testing only".to_string(),
                use_cases: vec![],
                hero_image: None,
            },
            service: ServiceProfile {
                preventive_maintenance_hours: 2.0,
                preventive_maintenance_interval_months: 6,
                calibration_interval_months: 12,
                required_skill_level: 3,
                depot_required: false,
                common_failure_modes: vec![],
                pm_checklist: vec![],
            },
            spare_parts: vec![],
            consumables: vec![],
            documents: vec![],
            end_of_support: None,
            current_firmware: None,
        }
    }

    #[tokio::test]
    async fn all_models_returns_all() {
        let catalog = InMemoryKb::new(vec![test_model("SKU-1"), test_model("SKU-2")]);
        let models = catalog.all_models().await.unwrap();
        assert_eq!(models.len(), 2);
    }

    #[tokio::test]
    async fn model_by_sku_found() {
        let catalog = InMemoryKb::new(vec![test_model("SKU-1"), test_model("SKU-2")]);
        let model = catalog.model_by_sku("SKU-2").await.unwrap();
        assert!(model.is_some());
        assert_eq!(model.unwrap().sku, "SKU-2");
    }

    #[tokio::test]
    async fn model_by_sku_not_found() {
        let catalog = InMemoryKb::new(vec![test_model("SKU-1")]);
        let model = catalog.model_by_sku("NOPE").await.unwrap();
        assert!(model.is_none());
    }

    #[tokio::test]
    async fn empty_catalog() {
        let catalog = InMemoryKb::new(vec![]);
        assert!(catalog.all_models().await.unwrap().is_empty());
        assert!(catalog.model_by_sku("X").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn create_model_adds_to_catalog() {
        let catalog = InMemoryKb::new(vec![]);
        let model = test_model("NEW-1");
        let sku = catalog.create_model(&model).await.unwrap();
        assert_eq!(sku, "NEW-1");
        assert_eq!(catalog.all_models().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn create_duplicate_sku_fails() {
        let catalog = InMemoryKb::new(vec![test_model("SKU-1")]);
        let result = catalog.create_model(&test_model("SKU-1")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn update_model_replaces() {
        let catalog = InMemoryKb::new(vec![test_model("SKU-1")]);
        let mut updated = test_model("SKU-1");
        updated.name = "Updated Name".to_string();
        catalog.update_model("SKU-1", &updated).await.unwrap();
        let fetched = catalog.model_by_sku("SKU-1").await.unwrap().unwrap();
        assert_eq!(fetched.name, "Updated Name");
    }

    #[tokio::test]
    async fn update_nonexistent_fails() {
        let catalog = InMemoryKb::new(vec![]);
        let result = catalog.update_model("NOPE", &test_model("NOPE")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_model_removes() {
        let catalog = InMemoryKb::new(vec![test_model("SKU-1"), test_model("SKU-2")]);
        catalog.delete_model("SKU-1").await.unwrap();
        assert_eq!(catalog.all_models().await.unwrap().len(), 1);
        assert!(catalog.model_by_sku("SKU-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_nonexistent_fails() {
        let catalog = InMemoryKb::new(vec![]);
        let result = catalog.delete_model("NOPE").await;
        assert!(result.is_err());
    }
}
