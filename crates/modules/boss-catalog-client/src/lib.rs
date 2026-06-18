//! HTTP client port for reaching the `boss-catalog` service.
//!
//! Currently exposes one question: "what's the service-relevant
//! slice of a system model?" for the device-insights projection
//! (operations-needs session 3, E4). Callers need the failure-mode
//! list and the spare-parts BOM; they don't need every AssetModel
//! field, so the summary type is a narrow projection rather than a
//! re-export of the full catalog shape.

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};
use serde::{Deserialize, Serialize};

/// Service-name marker for the shared [`HttpClientError`]. Keeps the
/// `Display` text reading `"catalog service unreachable: …"`.
#[derive(Debug)]
pub struct Catalog;
impl ServiceLabel for Catalog {
    const NAME: &'static str = "catalog";
}

/// Transport error for the Catalog client. Alias of the shared
/// [`HttpClientError`] so existing constructors and matches keep
/// compiling.
pub type CatalogClientError = HttpClientError<Catalog>;

/// Service-oriented slice of a `AssetModel` — only the fields the
/// device-insights view renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogModelSummary {
    pub sku: String,
    pub name: String,
    pub manufacturer: String,
    pub failure_modes: Vec<FailureMode>,
    pub spare_parts: Vec<SparePart>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureMode {
    pub code: String,
    pub name: String,
    pub frequency: f32,
    pub typical_fix: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SparePart {
    pub part_sku: String,
    pub name: String,
    pub lead_time_days: u16,
    pub high_usage: bool,
}

#[async_trait]
pub trait CatalogClient: Send + Sync {
    /// Fetch the service-relevant summary of a system model. Returns
    /// `None` for unknown SKUs so callers can render "model not in
    /// catalog" instead of a 404.
    async fn model_summary_by_sku(
        &self,
        sku: &str,
    ) -> Result<Option<CatalogModelSummary>, CatalogClientError>;
}

pub struct ReqwestCatalogClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestCatalogClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl CatalogClient for ReqwestCatalogClient {
    async fn model_summary_by_sku(
        &self,
        sku: &str,
    ) -> Result<Option<CatalogModelSummary>, CatalogClientError> {
        let url = format!("{}/api/catalog/models/{sku}", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| CatalogClientError::Unreachable(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(CatalogClientError::UnexpectedStatus(resp.status().as_u16()));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| CatalogClientError::MalformedBody(e.to_string()))?;
        Ok(Some(
            project_model_summary(&body).map_err(CatalogClientError::MalformedBody)?,
        ))
    }
}

/// Pure extractor: pick the relevant fields out of the full
/// AssetModel JSON. Extracted so tests can pin the projection shape
/// without running a reqwest stack.
pub fn project_model_summary(body: &serde_json::Value) -> Result<CatalogModelSummary, String> {
    let sku = body
        .get("sku")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing sku".to_string())?
        .to_string();
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let manufacturer = body
        .get("manufacturer")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let failure_modes: Vec<FailureMode> = body
        .get("service")
        .and_then(|s| s.get("common_failure_modes"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(FailureMode {
                        code: m.get("code")?.as_str()?.to_string(),
                        name: m.get("name")?.as_str()?.to_string(),
                        frequency: m.get("frequency").and_then(|f| f.as_f64()).unwrap_or(0.0)
                            as f32,
                        typical_fix: m
                            .get("typical_fix")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let spare_parts: Vec<SparePart> = body
        .get("spare_parts")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    Some(SparePart {
                        part_sku: p.get("part_sku")?.as_str()?.to_string(),
                        name: p
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        lead_time_days: p
                            .get("lead_time_days")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u16,
                        high_usage: p
                            .get("high_usage")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(CatalogModelSummary {
        sku,
        name,
        manufacturer,
        failure_modes,
        spare_parts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projects_failure_modes_and_spare_parts_from_full_model() {
        let full = json!({
            "sku": "LUM-3D",
            "name": "Lumicell 3D",
            "manufacturer": "Lumicell",
            "service": {
                "common_failure_modes": [
                    { "code": "HEAT", "name": "Heat sink fail", "frequency": 0.12, "typical_fix": "Replace sink" },
                    { "code": "CAL",  "name": "Calibration drift", "frequency": 0.08, "typical_fix": "Recalibrate" }
                ]
            },
            "spare_parts": [
                { "part_sku": "PART-SINK", "name": "Heat Sink", "lead_time_days": 7, "high_usage": true },
                { "part_sku": "PART-CAL",  "name": "Cal Cell",  "lead_time_days": 14, "high_usage": false }
            ]
        });
        let s = project_model_summary(&full).unwrap();
        assert_eq!(s.sku, "LUM-3D");
        assert_eq!(s.failure_modes.len(), 2);
        assert_eq!(s.failure_modes[0].code, "HEAT");
        assert_eq!(s.spare_parts.len(), 2);
        assert!(s.spare_parts[0].high_usage);
    }

    #[test]
    fn missing_service_block_yields_empty_failure_modes() {
        let full = json!({
            "sku": "SKU-A",
            "name": "Name A",
            "manufacturer": "Acme",
            "spare_parts": []
        });
        let s = project_model_summary(&full).unwrap();
        assert!(s.failure_modes.is_empty());
    }

    #[test]
    fn malformed_sku_returns_err() {
        let full = json!({ "name": "n", "manufacturer": "m" });
        assert!(project_model_summary(&full).is_err());
    }
}
