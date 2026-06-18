//! Domain types for the ML platform.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Kind of model — deliberately an open set in the design doc
/// (D-decisions allow new kinds via CHECK constraint migration).
///
/// The storage-only kinds (`RiskScore` … `Ranking`) have their
/// predictions written by code outside boss-ml. The two
/// inference-driven kinds (`DeclarativeRule`, `HeuristicFormula`)
/// are dispatched by the `InferenceDispatcher` and described by
/// `inference_spec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelKind {
    RiskScore,
    Regression,
    Classification,
    AnomalyDetection,
    Ranking,
    DeclarativeRule,
    HeuristicFormula,
}

impl ModelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RiskScore => "risk-score",
            Self::Regression => "regression",
            Self::Classification => "classification",
            Self::AnomalyDetection => "anomaly-detection",
            Self::Ranking => "ranking",
            Self::DeclarativeRule => "declarative-rule",
            Self::HeuristicFormula => "heuristic-formula",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "risk-score" => Some(Self::RiskScore),
            "regression" => Some(Self::Regression),
            "classification" => Some(Self::Classification),
            "anomaly-detection" => Some(Self::AnomalyDetection),
            "ranking" => Some(Self::Ranking),
            "declarative-rule" => Some(Self::DeclarativeRule),
            "heuristic-formula" => Some(Self::HeuristicFormula),
            _ => None,
        }
    }

    /// True for kinds whose predictions are produced by the
    /// `InferenceDispatcher` (declarative-rule + heuristic-formula).
    /// Storage-only kinds return `false` — their predictions land
    /// from external code.
    pub fn is_inference_driven(self) -> bool {
        matches!(self, Self::DeclarativeRule | Self::HeuristicFormula)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelStatus {
    Draft,
    Active,
    Shadow,
    Retired,
}

impl ModelStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Shadow => "shadow",
            Self::Retired => "retired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "active" => Some(Self::Active),
            "shadow" => Some(Self::Shadow),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }
}

/// One registered model. Returned as-is by `GET /api/ml/models` and
/// `GET /api/ml/models/:id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MlModel {
    pub id: String,
    pub name: String,
    pub kind: ModelKind,
    pub version: String,
    pub status: ModelStatus,
    pub accuracy: Option<f64>,
    pub accuracy_metric: Option<String>,
    pub training_data_ref: Option<String>,
    pub description: Option<String>,
    /// Dispatch spec for inference-driven kinds. Required when
    /// `kind.is_inference_driven()`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_spec: Option<InferenceSpec>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Inference dispatch spec. The variant is selected by the model's
/// `kind`; the JSONB column carries one of these shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum InferenceSpec {
    /// SQL + payload template; no plugin code. The dispatcher
    /// runs `query`, reads `score_expr` and the
    /// `entity_id_column` from each row, and renders
    /// `payload_template` against the row's columns.
    DeclarativeRule(DeclarativeRuleSpec),
    /// Plugin-driven scoring. The dispatcher enumerates entities
    /// via `entity_query` (returning a single `entity_id`
    /// column), then calls the named plugin for each.
    HeuristicFormula(HeuristicFormulaSpec),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclarativeRuleSpec {
    pub query: String,
    pub score_expr: String,
    pub entity_type: String,
    pub entity_id_column: String,
    /// Optional column whose value is folded into the
    /// generated prediction id. Lets a single rule fire
    /// multiple times per (entity, day) — e.g. one
    /// `contract-expiring` prediction per agreement, even
    /// when several agreements belong to the same account.
    /// When unset, the dispatcher keys predictions on
    /// `(model, entity, day)` so reruns within a day are a
    /// no-op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_key_column: Option<String>,
    #[serde(default)]
    pub payload_template: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeuristicFormulaSpec {
    pub plugin_name: String,
    pub entity_type: String,
    pub entity_query: String,
}

/// Model summary used by the CTO panel — includes derived
/// `predictions_24h` and `latest_prediction_at` so the panel can
/// render in one fetch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MlModelSummary {
    #[serde(flatten)]
    pub model: MlModel,
    pub predictions_24h: i64,
    pub latest_prediction_at: Option<DateTime<Utc>>,
}

/// A stored prediction. Append-only — never updated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MlPrediction {
    pub id: String,
    pub model_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub score: f64,
    pub payload: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

/// POST body for `/api/ml/predictions`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePredictionInput {
    pub id: String,
    pub model_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub score: f64,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarative_rule_spec_roundtrip() {
        let spec = InferenceSpec::DeclarativeRule(DeclarativeRuleSpec {
            query: "SELECT id FROM accounts".into(),
            score_expr: "score".into(),
            entity_type: "account".into(),
            entity_id_column: "id".into(),
            row_key_column: None,
            payload_template: serde_json::json!({"rule": "test"}),
        });
        let s = serde_json::to_string(&spec).unwrap();
        let parsed: InferenceSpec = serde_json::from_str(&s).unwrap();
        assert_eq!(spec, parsed);
        assert!(s.contains("\"kind\":\"declarative-rule\""));
    }

    #[test]
    fn heuristic_formula_spec_roundtrip() {
        let spec = InferenceSpec::HeuristicFormula(HeuristicFormulaSpec {
            plugin_name: "account-churn-risk-composite-v1".into(),
            entity_type: "account".into(),
            entity_query: "SELECT id FROM accounts".into(),
        });
        let s = serde_json::to_string(&spec).unwrap();
        let parsed: InferenceSpec = serde_json::from_str(&s).unwrap();
        assert_eq!(spec, parsed);
        assert!(s.contains("\"kind\":\"heuristic-formula\""));
    }

    #[test]
    fn inference_driven_kinds_flagged() {
        assert!(ModelKind::DeclarativeRule.is_inference_driven());
        assert!(ModelKind::HeuristicFormula.is_inference_driven());
        assert!(!ModelKind::RiskScore.is_inference_driven());
        assert!(!ModelKind::Regression.is_inference_driven());
    }

    #[test]
    fn modelkind_parse_roundtrip_includes_phase2() {
        for kind in [
            ModelKind::RiskScore,
            ModelKind::Regression,
            ModelKind::Classification,
            ModelKind::AnomalyDetection,
            ModelKind::Ranking,
            ModelKind::DeclarativeRule,
            ModelKind::HeuristicFormula,
        ] {
            assert_eq!(ModelKind::parse(kind.as_str()), Some(kind));
        }
    }
}
