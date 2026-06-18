//! Domain types for shipment tracking.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShipmentDirection {
    Inbound,
    Outbound,
}

/// Where a shipment sits in its carrier lifecycle. Free-text wrapper
/// around a kebab-case string; the five platform stages are seeded as
/// Class rows under `(subject_kind='shipment', member_attribute='status')`
/// and a tenant extends the lifecycle by adding a row, not forking core.
/// The shipping API validates an incoming status against the active
/// Class set at the create/update boundary (fail-loud → 400). Serializes
/// transparently to the bare string; the `shipments.status` column
/// stores it directly. See docs/design/class-registry.md.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShipmentStatus(pub String);

impl ShipmentStatus {
    pub const LABEL_CREATED: &'static str = "label-created";
    pub const PICKED_UP: &'static str = "picked-up";
    pub const IN_TRANSIT: &'static str = "in-transit";
    pub const DELIVERED: &'static str = "delivered";
    pub const EXCEPTION: &'static str = "exception";

    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when the shipment has reached the carrier's delivered
    /// terminal. Drives the in-flight vs delivered split in summaries.
    pub fn is_delivered(&self) -> bool {
        self.0 == Self::DELIVERED
    }
}

impl std::fmt::Display for ShipmentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ShipmentStatus {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ShipmentStatus {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Shipment carrier (FedEx, UPS, freight forwarder, local pickup, …).
/// Free-text wrapper around a kebab-case string; tenants extend via
/// the Class registry under `(subject_kind='shipment')`. The shipments
/// row stores the code; validation happens at the shipping API
/// boundary against the active Class set per
/// docs/design/class-registry.md. Serializes transparently to the
/// bare string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Carrier(pub String);

impl Carrier {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Carrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Carrier {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Carrier {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// A SKU + qty pair carried on a shipment. Distinct from
/// [`Shipment::asset_ids`] (which references identity-bearing
/// used-device-shop systems): line items are non-identity-bearing
/// commodities — finished-product kegs, replacement parts, raw
/// ingredients in a vendor inbound — recorded as
/// SKU + quantity, not per-unit Subjects.
///
/// Populated by the shipping.create side effect from the JobKind
/// shipment-step's `line_items` metadata. The
/// inventory.parts.consume handler reads the same metadata to
/// decrement stock when the shipment step transitions to done,
/// so the projection here and the inventory delta stay in sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShipmentLineItem {
    /// Always a finished-product SKU (e.g. `FP-PALE-1-2-BBL`).
    /// Raw-parts consumption uses a separate `ingredients_consumed`
    /// / `parts_consumed` array authored with `part_sku`.
    pub sku: String,
    pub qty: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_price_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shipment {
    pub id: String,
    pub direction: ShipmentDirection,
    pub status: ShipmentStatus,
    /// Carrier code, validated against the Class registry under
    /// `(subject_kind='shipment')` when present. Identity-first: a
    /// shipment can be created without a carrier (label not yet
    /// purchased / forwarder not yet chosen) and enriched later. The
    /// API gate validates only a `Some` value; `None` passes through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<Carrier>,
    pub tracking_number: Option<String>,
    pub origin: String,
    pub destination: String,
    pub asset_ids: Vec<String>,
    /// SKU-level contents of the shipment. Always serialized
    /// (even when empty) so the SPA + inventory consume handler
    /// can rely on the field's presence rather than the schema
    /// having to special-case "system shipments" vs
    /// "line-item shipments." Authoring order preserved.
    #[serde(default)]
    pub line_items: Vec<ShipmentLineItem>,
    pub po_id: Option<String>,
    pub order_id: Option<String>,
    pub account_id: Option<String>,
    pub created_on: NaiveDate,
    pub shipped_on: Option<NaiveDate>,
    pub estimated_delivery: Option<NaiveDate>,
    pub delivered_on: Option<NaiveDate>,
}

impl Shipment {
    /// Each system carried on this shipment is a [`Part::Subject`] —
    /// systems have their own identity, events, and Jobs, so the
    /// shipment's Parts reference them by kind+id rather than
    /// inlining their state.
    pub fn parts(&self) -> Vec<boss_core::primitives::Part> {
        self.asset_ids
            .iter()
            .map(|sid| boss_core::primitives::Part::Subject {
                subject_kind: "system".to_string(),
                id: sid.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod part_conversion_tests {
    use super::*;
    use boss_core::primitives::Part;

    #[test]
    fn shipment_systems_become_subject_parts() {
        let s = Shipment {
            id: "SHP-2026-0042".into(),
            direction: ShipmentDirection::Outbound,
            status: ShipmentStatus::IN_TRANSIT.into(),
            carrier: Some(Carrier::new("fedex")),
            tracking_number: Some("1Z999AA10123456784".into()),
            origin: "warehouse".into(),
            destination: "account-00042".into(),
            asset_ids: vec!["SYS-0001".into(), "SYS-0002".into()],
            line_items: Vec::new(),
            po_id: None,
            order_id: None,
            account_id: Some("account-00042".into()),
            created_on: chrono::NaiveDate::from_ymd_opt(2026, 4, 23).unwrap(),
            shipped_on: Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 24).unwrap()),
            estimated_delivery: None,
            delivered_on: None,
        };
        let parts = s.parts();
        assert_eq!(parts.len(), 2);
        match &parts[0] {
            Part::Subject { subject_kind, id } => {
                assert_eq!(subject_kind, "system");
                assert_eq!(id, "SYS-0001");
            }
            Part::Attribute { .. } => panic!("shipment parts should be Subject, not Attribute"),
        }
    }

    #[test]
    fn shipment_with_no_systems_has_no_parts() {
        let s = Shipment {
            id: "SHP-EMPTY".into(),
            direction: ShipmentDirection::Outbound,
            status: ShipmentStatus::LABEL_CREATED.into(),
            carrier: Some(Carrier::new("ups")),
            tracking_number: None,
            origin: "warehouse".into(),
            destination: "account-0".into(),
            asset_ids: Vec::new(),
            line_items: Vec::new(),
            po_id: None,
            order_id: None,
            account_id: None,
            created_on: chrono::NaiveDate::from_ymd_opt(2026, 4, 24).unwrap(),
            shipped_on: None,
            estimated_delivery: None,
            delivered_on: None,
        };
        assert!(s.parts().is_empty());
    }
}
