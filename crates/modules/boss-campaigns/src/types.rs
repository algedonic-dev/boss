//! Domain types for marketing campaigns.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// One marketing motion — a tap launch, a seasonal release, a promo.
/// Deliberately thin: the audit found campaigns referenced by
/// tap-launch Jobs and marketing-asset links with no home; this row
/// is that home. Attributes accrue when the marketing flows need
/// them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Campaign {
    pub id: String,
    pub name: String,
    /// Tenant-defined lifecycle ('active', 'ended', …). Free-text;
    /// the Class registry validates per-tenant when a taxonomy lands.
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub starts_on: Option<NaiveDate>,
    #[serde(default)]
    pub ends_on: Option<NaiveDate>,
    /// Free-form: channel, budget_cents, beer sku, …
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

fn default_status() -> String {
    "active".to_string()
}

fn default_metadata() -> serde_json::Value {
    serde_json::json!({})
}
