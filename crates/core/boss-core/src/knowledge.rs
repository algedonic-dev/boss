//! Knowledge Base pattern — shared trait for entity intelligence.
//!
//! Every major noun in BOSS (assets, accounts, providers) accumulates
//! knowledge through the same pattern:
//!
//! 1. **Profile** — slow-changing reference data (edited by humans)
//! 2. **Facts** — append-only events from operational work (Jobs, Steps)
//! 3. **Aggregations** — computed patterns from facts (rebuilt periodically)
//! 4. **Documents** — associated files (manuals, contracts, certifications)
//!
//! Domain crates implement this trait for their entity types.
//! The frontend renders KB views polymorphically using the same four-section
//! layout regardless of entity kind.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::define_id;

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

define_id!(DocumentId);

/// A document associated with any KB entity. Shared across all KBs (D4).
/// Stored in a single `documents` table, filtered by entity_kind + entity_id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    #[serde(default)]
    pub id: DocumentId,
    /// Which KB this belongs to: "asset", "account", "provider"
    pub entity_kind: String,
    /// The entity's identifier within its KB
    pub entity_id: String,
    /// Document type: "manual", "spec-sheet", "contract", "proposal",
    /// "certificate", "photo", "meeting-notes", etc.
    pub doc_type: String,
    pub title: String,
    pub url: Option<String>,
    pub version: Option<String>,
    /// Who can see this: "internal", "customer", "public"
    #[serde(default = "default_audience")]
    pub audience: String,
    pub uploaded_by: Option<String>,
    pub uploaded_at: Option<DateTime<Utc>>,
}

fn default_audience() -> String {
    "internal".to_string()
}

/// A fact about an entity — one thing that happened, derived from a
/// Job Step completion, an event, or a manual entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    #[serde(default)]
    pub id: String,
    /// Which KB entity this fact is about
    pub entity_kind: String,
    pub entity_id: String,
    /// What happened: "repair", "sale", "communication", "delivery", etc.
    pub fact_kind: String,
    /// When it happened
    pub occurred_at: NaiveDate,
    /// Who caused it (employee, system, external)
    pub actor_id: Option<String>,
    /// The Job this fact came from (if any)
    pub job_id: Option<String>,
    /// The Step this fact came from (if any)
    pub step_id: Option<String>,
    /// Structured data about what happened (type-specific)
    pub payload: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_serde_round_trip() {
        let doc = Document {
            id: DocumentId::new(),
            entity_kind: "asset".into(),
            entity_id: "SYS-00010001".into(),
            doc_type: "service-manual".into(),
            title: "Palomar Icon Service Manual v3.2".into(),
            url: Some("https://docs.boss.com/manuals/icon-3.2.pdf".into()),
            version: Some("3.2".into()),
            audience: "internal".into(),
            uploaded_by: Some("emp-042".into()),
            uploaded_at: None,
        };
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn fact_serde_round_trip() {
        let fact = Fact {
            id: "fact-001".into(),
            entity_kind: "account".into(),
            entity_id: "PRC-001".into(),
            fact_kind: "sale".into(),
            occurred_at: NaiveDate::from_ymd_opt(2026, 4, 17).unwrap(),
            actor_id: Some("emp-012".into()),
            job_id: Some("job-uuid-here".into()),
            step_id: Some("step-uuid-here".into()),
            payload: serde_json::json!({
                "value_cents": 4_500_000,
                "currency": "USD",
                "system_ids": ["SYS-00010001"],
            }),
        };
        let json = serde_json::to_string(&fact).unwrap();
        let back: Fact = serde_json::from_str(&json).unwrap();
        assert_eq!(fact, back);
    }

    #[test]
    fn document_default_audience() {
        let json =
            r#"{"entity_kind":"asset","entity_id":"SYS-001","doc_type":"manual","title":"Test"}"#;
        let doc: Document = serde_json::from_str(json).unwrap();
        assert_eq!(doc.audience, "internal");
    }
}
