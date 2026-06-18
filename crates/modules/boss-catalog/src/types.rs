//! Device catalog domain types.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

fn default_currency() -> String {
    "USD".to_string()
}

// ---------------------------------------------------------------------------
// Identity & classification
// ---------------------------------------------------------------------------

/// Stable catalog key, e.g. `"Boss-HAL-3D-2024"`.
pub type Sku = String;

/// High-level device category. Free-text wrapper around a kebab-case
/// string; tenants extend via the Class registry under
/// `(subject_kind='asset', member_attribute='category')`. The
/// catalog row stores the code; validation happens at the API
/// boundary against the active Class set per
/// docs/design/class-registry.md.
///
/// Wire shape is the bare string (`"fractional-co2"`, `"diode"`,
/// `"router"`, etc.) — the wrapper exists for type-distinction in
/// Rust but serializes transparently.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceCategory(pub String);

impl DeviceCategory {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DeviceCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for DeviceCategory {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DeviceCategory {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tenant-defined extras
// ---------------------------------------------------------------------------
//
// Typed-vs-extras boundary: kind-specific specs
// (networking-equipment port counts, brewing-vessel capacity,
// printer ppm, vehicle dimensions, …) live in a flat `extras`
// JSON blob hung off AssetModel (one flat blob, no tenant
// namespacing). The platform stays neutral; tenants validate
// against their own JSON schema in `asset_model_extras_schema`,
// reintroducing structured types alongside that validator schema if
// they want them.

// ---------------------------------------------------------------------------
// Physical & power
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Physical {
    pub width_cm: f32,
    pub depth_cm: f32,
    pub height_cm: f32,
    pub weight_kg: f32,
    /// e.g. "208V, 20A, single-phase".
    pub power_requirements: String,
}

// ---------------------------------------------------------------------------
// Regulatory
// ---------------------------------------------------------------------------

/// Generic regulatory metadata. Tenants in regulated industries map
/// their own clearance ids + device classes onto these fields; the
/// type stays neutral.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regulatory {
    /// Opaque clearance / approval identifier issued by whichever
    /// regulator the tenant operates under (e.g. an FDA 510(k)
    /// number, a CE-mark notified-body id, an FCC ID).
    pub clearance_id: Option<String>,
    pub clearance_date: Option<NaiveDate>,
    /// Tenant-defined device class (regulated industries usually
    /// have a 1/2/3 risk tier; the schema stays neutral).
    pub regulator_device_class: u8,
}

// ---------------------------------------------------------------------------
// Commerce
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Commerce {
    pub list_price_new_cents: i64,
    pub typical_refurb_price_cents: Option<i64>,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub lead_time_days: Option<u16>,
    /// 1-3 sentence marketing description.
    pub tagline: String,
    /// Longer paragraph for detail pages.
    pub description: String,
    /// Tenant-defined use cases this device covers (informs
    /// search/filter). Free-text by design — a future Class-registry
    /// row can validate values per tenant without forcing every
    /// tenant to fork the closed enum that lived here previously.
    pub use_cases: Vec<String>,
    /// Hero image reference — resolved by the UI.
    pub hero_image: Option<String>,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureMode {
    pub code: String,
    pub name: String,
    /// How frequently this shows up across the installed base (0.0-1.0).
    pub frequency: f32,
    pub typical_fix: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceProfile {
    /// Typical hours for a preventive-maintenance visit.
    pub preventive_maintenance_hours: f32,
    /// Months between recommended PMs.
    pub preventive_maintenance_interval_months: u8,
    /// Months between required calibrations.
    pub calibration_interval_months: u8,
    /// Minimum skill level required (1 = apprentice, 5 = senior specialist).
    pub required_skill_level: u8,
    /// Whether a depot service bay is required (vs. field-serviceable).
    pub depot_required: bool,
    pub common_failure_modes: Vec<FailureMode>,
    pub pm_checklist: Vec<String>,
}

// ---------------------------------------------------------------------------
// Parts & consumables
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SparePart {
    pub part_sku: String,
    pub name: String,
    pub description: String,
    pub unit_price_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    /// Days to acquire if not in stock.
    pub lead_time_days: u16,
    /// Commonly replaced (for stocking guidance)?
    pub high_usage: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Consumable {
    pub part_sku: String,
    pub name: String,
    pub description: String,
    pub unit_price_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    /// Estimated treatments per unit.
    pub treatments_per_unit: Option<u32>,
}

/// Flat row shape for `GET /api/catalog/parts`. Mirrors the
/// `parts` table 1:1 so the SPA can populate inventory rows
/// even when no asset_model references the part (the brewery
/// case — ingredients and packaging are first-class but not
/// satellites of a system model).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartCatalogRow {
    pub part_sku: String,
    pub name: String,
    pub description: String,
    pub unit_price_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub lead_time_days: u16,
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

/// Catalog document kind (manual, spec sheet, cert, training video, …).
/// Free-text wrapper around a kebab-case string; tenants extend via the
/// Class registry under `(subject_kind='asset',
/// member_attribute='document-kind')`. The catalog row stores the code;
/// validation happens at the API boundary against the active Class set
/// per docs/design/class-registry.md. Serializes transparently to the
/// bare string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocumentKind(pub String);

impl DocumentKind {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DocumentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for DocumentKind {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DocumentKind {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub kind: DocumentKind,
    pub title: String,
    pub url: String,
    pub version: Option<String>,
    pub published: Option<NaiveDate>,
    /// 'internal' = techs only; 'customer' = shared with accounts.
    pub audience: DocumentAudience,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentAudience {
    Internal,
    Customer,
    Public,
}

/// Generic entity-keyed document — the `documents` table is keyed
/// (entity_kind, entity_id) so the same shape covers vendor
/// datasheets, ingredient COA scans, account agreements, etc.
/// Distinct from `Document` (which models the system-catalog
/// satellite, keyed only by SKU + with a closed-enum `kind`).
/// Wire-shape matches the SPA's `KBDocument` consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityDocument {
    pub id: String,
    pub doc_type: String,
    pub title: String,
    pub url: Option<String>,
    pub version: Option<String>,
    pub audience: String,
    pub uploaded_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ---------------------------------------------------------------------------
// The assembled record
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetModel {
    pub sku: Sku,
    pub name: String,
    pub manufacturer: String,
    pub model_year: u16,
    pub category: DeviceCategory,
    /// Tenant-defined kind-specific specs. Empty `{}` for tenants
    /// who don't publish extra detail. Validated against the
    /// per-category JSON-schema row in `asset_model_extras_schema`
    /// when one exists; pure pass-through otherwise.
    #[serde(default)]
    pub extras: serde_json::Value,
    pub physical: Physical,
    pub regulatory: Regulatory,
    pub commerce: Commerce,
    pub service: ServiceProfile,
    pub spare_parts: Vec<SparePart>,
    pub consumables: Vec<Consumable>,
    pub documents: Vec<Document>,
    /// End-of-support date, if announced.
    pub end_of_support: Option<NaiveDate>,
    pub current_firmware: Option<String>,
}
