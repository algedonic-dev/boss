//! Domain types for the Marketing Asset KB.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Marketing-asset kind (photo, video, deck, one-pager, brief-body, …).
/// Free-text wrapper around a kebab-case string; tenants extend via the
/// Class registry under `subject_kind='marketing-asset'` (the code is the
/// kind string). The `marketing_assets` row stores the code; validation
/// happens at the catalog API boundary against the active Class set per
/// docs/design/class-registry.md. Serializes transparently to the bare
/// string. Lifted from the closed `AssetKind` enum + the
/// `marketing_assets.kind` CHECK, mirroring the `DocumentKind` lift in
/// this crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetKind(pub String);

impl AssetKind {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AssetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for AssetKind {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AssetKind {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketingAsset {
    pub id: String,
    pub title: String,
    /// Identity-first: an asset can exist before it's classified, so
    /// `kind` is nullable. When present, it's validated against the
    /// Class registry (`subject_kind='marketing-asset'`) at the API
    /// boundary; `None` means "not classified yet" and skips the gate.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub file_url: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub linked_device_skus: Vec<String>,
    #[serde(default)]
    pub linked_account_ids: Vec<String>,
    #[serde(default)]
    pub linked_campaign_ids: Vec<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub brand_reviewed_by: Option<String>,
    #[serde(default)]
    pub brand_reviewed_at: Option<DateTime<Utc>>,
    /// Points at the asset this one replaces. `None` when this is
    /// the original (or when a new version hasn't superseded it yet).
    #[serde(default)]
    pub supersedes_id: Option<String>,
    #[serde(default)]
    pub retired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewMarketingAsset {
    pub id: String,
    pub title: String,
    /// Identity-first: omit to create an unclassified asset and set
    /// the kind later. When present, validated against the Class
    /// registry (`subject_kind='marketing-asset'`) before commit.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub file_url: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub linked_device_skus: Vec<String>,
    #[serde(default)]
    pub linked_account_ids: Vec<String>,
    #[serde(default)]
    pub linked_campaign_ids: Vec<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub brand_reviewed_by: Option<String>,
    #[serde(default)]
    pub brand_reviewed_at: Option<DateTime<Utc>>,
    /// When creating a new version of an existing asset, point at the
    /// prior version's id. Callers that want the "supersede in place"
    /// behavior should use the `POST /:id/supersede` endpoint instead
    /// of constructing this manually.
    #[serde(default)]
    pub supersedes_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateMarketingAsset {
    pub title: Option<String>,
    pub description: Option<String>,
    pub file_url: Option<String>,
    pub tags: Option<Vec<String>>,
    pub linked_device_skus: Option<Vec<String>>,
    pub linked_account_ids: Option<Vec<String>>,
    pub linked_campaign_ids: Option<Vec<String>>,
    pub owner_id: Option<String>,
    pub brand_reviewed_by: Option<String>,
    pub brand_reviewed_at: Option<DateTime<Utc>>,
}
