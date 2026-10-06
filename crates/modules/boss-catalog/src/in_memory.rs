//! In-memory adapter for `KbRepository`.
//!
//! Useful for tests and as a seed-data fallback. It answers what
//! `PgKb` answers — the adapters-agree suite
//! (`tests/the_adapters_agree_on_the_equipment_kb_pg.rs`, backlog
//! be459ab9) holds the two to one statement of the port.

use std::collections::BTreeMap;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::port::{KbError, KbRepository};
use crate::types::{AssetModel, PartCatalogRow};
use crate::validate::{refuse_moved_sku, refuse_unstorable};

/// The double's tables: each model as written, and the ONE parts row
/// every model naming a part shares.
///
/// WHY a parts map (backlog be459ab9, found by the adapters-agree
/// suite, 2026-10-01): Postgres keeps a part in the shared `parts`
/// table, so the last model to write a part SKU names it for every
/// model, and the row outlives a deleted model — the port's `all_parts`
/// is "independent of any linkage". The double kept a private copy per
/// model and derived `all_parts` from the live models, so a part
/// vanished with its model and two models disagreed on its name.
#[derive(Default)]
struct Tables {
    /// Keyed by SKU, so the list is in byte order of SKU as Postgres's
    /// `ORDER BY sku COLLATE "C"`.
    models: BTreeMap<String, AssetModel>,
    parts: BTreeMap<String, PartCatalogRow>,
}

impl Tables {
    /// Write a model's parts the way `postgres::insert_satellites`
    /// does: a spare part writes every field; a consumable writes its
    /// name, description, price and currency, takes a lead time of 7
    /// days when it is the first to write the part, and leaves the
    /// lead time alone otherwise.
    fn write_parts(&mut self, m: &AssetModel) {
        for p in &m.spare_parts {
            self.parts.insert(
                p.part_sku.clone(),
                PartCatalogRow {
                    part_sku: p.part_sku.clone(),
                    name: p.name.clone(),
                    description: p.description.clone(),
                    unit_price_cents: p.unit_price_cents,
                    currency: p.currency.clone(),
                    lead_time_days: p.lead_time_days,
                },
            );
        }
        for c in &m.consumables {
            let lead_time_days = self.parts.get(&c.part_sku).map_or(7, |p| p.lead_time_days);
            self.parts.insert(
                c.part_sku.clone(),
                PartCatalogRow {
                    part_sku: c.part_sku.clone(),
                    name: c.name.clone(),
                    description: c.description.clone(),
                    unit_price_cents: c.unit_price_cents,
                    currency: c.currency.clone(),
                    lead_time_days,
                },
            );
        }
    }

    /// A model as `PgKb::assemble` answers it: use cases, failure modes,
    /// spare parts and consumables in byte order, each part read from
    /// its shared row; the checklist and documents in the order sent.
    fn read(&self, m: &AssetModel) -> AssetModel {
        let mut out = m.clone();
        out.commerce.use_cases.sort();
        out.service
            .common_failure_modes
            .sort_by(|a, b| a.code.cmp(&b.code));
        for p in &mut out.spare_parts {
            if let Some(row) = self.parts.get(&p.part_sku) {
                p.name = row.name.clone();
                p.description = row.description.clone();
                p.unit_price_cents = row.unit_price_cents;
                p.currency = row.currency.clone();
                p.lead_time_days = row.lead_time_days;
            }
        }
        out.spare_parts.sort_by(|a, b| a.part_sku.cmp(&b.part_sku));
        for c in &mut out.consumables {
            if let Some(row) = self.parts.get(&c.part_sku) {
                c.name = row.name.clone();
                c.description = row.description.clone();
                c.unit_price_cents = row.unit_price_cents;
                c.currency = row.currency.clone();
            }
        }
        out.consumables.sort_by(|a, b| a.part_sku.cmp(&b.part_sku));
        out
    }
}

pub struct InMemoryKb {
    tables: RwLock<Tables>,
    recorded: std::sync::Mutex<Vec<boss_core::event::Event>>,
}

fn poisoned<T>(_: T) -> KbError {
    KbError::Storage("in-memory kb lock poisoned".into())
}

impl InMemoryKb {
    pub fn new(models: Vec<AssetModel>) -> Self {
        let mut tables = Tables::default();
        for m in models {
            tables.write_parts(&m);
            tables.models.insert(m.sku.clone(), m);
        }
        Self {
            tables: RwLock::new(tables),
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
impl KbRepository for InMemoryKb {
    async fn all_models(&self) -> Result<Vec<AssetModel>, KbError> {
        let t = self.tables.read().map_err(poisoned)?;
        Ok(t.models.values().map(|m| t.read(m)).collect())
    }

    async fn model_by_sku(&self, sku: &str) -> Result<Option<AssetModel>, KbError> {
        let t = self.tables.read().map_err(poisoned)?;
        Ok(t.models.get(sku).map(|m| t.read(m)))
    }

    async fn create_model_at(
        &self,
        model: &AssetModel,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<String, KbError> {
        refuse_unstorable(model)?;
        {
            let mut t = self.tables.write().map_err(poisoned)?;
            if t.models.contains_key(&model.sku) {
                return Err(KbError::Conflict(format!(
                    "SKU {} already exists",
                    model.sku
                )));
            }
            t.write_parts(model);
            t.models.insert(model.sku.clone(), model.clone());
        }
        self.record(stamp.event(
            crate::events::MODEL_CREATED,
            serde_json::to_value(model).unwrap_or_default(),
        ));
        Ok(model.sku.clone())
    }

    async fn update_model_at(
        &self,
        sku: &str,
        model: &AssetModel,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), KbError> {
        refuse_moved_sku(sku, model)?;
        refuse_unstorable(model)?;
        {
            let mut t = self.tables.write().map_err(poisoned)?;
            if !t.models.contains_key(sku) {
                return Err(KbError::NotFound(sku.to_string()));
            }
            t.write_parts(model);
            t.models.insert(sku.to_string(), model.clone());
        }
        self.record(stamp.event(
            crate::events::MODEL_UPDATED,
            serde_json::to_value(model).unwrap_or_default(),
        ));
        Ok(())
    }

    async fn delete_model_at(
        &self,
        sku: &str,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), KbError> {
        {
            let mut t = self.tables.write().map_err(poisoned)?;
            // The model's satellites go with it; its parts rows stay,
            // as they do in Postgres.
            if t.models.remove(sku).is_none() {
                return Err(KbError::NotFound(sku.to_string()));
            }
        }
        self.record(stamp.event(
            crate::events::MODEL_DELETED,
            serde_json::json!({ "sku": sku, "deleted_at": now }),
        ));
        Ok(())
    }

    async fn documents_for(
        &self,
        _entity_kind: &str,
        _entity_id: &str,
    ) -> Result<Vec<crate::types::EntityDocument>, KbError> {
        // In-memory adapter has no `documents` table (no port method
        // writes one); tests that exercise document surfacing drive the
        // postgres adapter.
        Ok(Vec::new())
    }

    async fn all_parts(&self) -> Result<Vec<PartCatalogRow>, KbError> {
        // Every parts row, in byte order of part SKU. The double holds
        // no `inventory_items` table, so it has none of the stock-only
        // stub rows Postgres unions in from boss-inventory.
        let t = self.tables.read().map_err(poisoned)?;
        Ok(t.parts.values().cloned().collect())
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

    // Backlog e9ff7ccb: the refusals past each column's width ride the
    // adapters-agree suite; this pins the ceiling itself as storable.
    #[tokio::test]
    async fn create_accepts_the_smallint_ceiling() {
        let catalog = InMemoryKb::new(vec![]);
        let mut model = test_model("SKU-EDGE");
        model.commerce.lead_time_days = Some(32_767);
        catalog.create_model(&model).await.unwrap();
    }
}
