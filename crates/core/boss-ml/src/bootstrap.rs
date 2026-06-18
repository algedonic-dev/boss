//! Bootstrap seed — candidate models upserted on every service
//! startup. Idempotent via the port's upsert_model so a restart
//! doesn't duplicate rows.
//!
//! The catalog itself is data — see
//! `crates/core/boss-ml/seeds/phase_two_models.toml`. Each
//! `[[model]]` block deserializes into an `MlModelSeed` (this
//! module's private struct, same shape as `MlModel` minus runtime
//! timestamps), then `seed_phase_two_candidates` stamps
//! `created_at` / `updated_at` and calls `repo.upsert_model`.
//! Adding a new candidate model is a one-block edit to the TOML
//! plus a restart — no Rust code changes.

use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::port::{MlError, MlRepository};
use crate::types::{InferenceSpec, MlModel, ModelKind, ModelStatus};

const PHASE_TWO_SEEDS_TOML: &str = include_str!("../seeds/phase_two_models.toml");

#[derive(Debug, Deserialize)]
struct PhaseTwoSeeds {
    model: Vec<MlModelSeed>,
}

#[derive(Debug, Clone, Deserialize)]
struct MlModelSeed {
    id: String,
    name: String,
    kind: ModelKind,
    version: String,
    status: ModelStatus,
    #[serde(default)]
    accuracy: Option<f64>,
    #[serde(default)]
    accuracy_metric: Option<String>,
    #[serde(default)]
    training_data_ref: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    inference_spec: Option<InferenceSpec>,
}

impl MlModelSeed {
    fn into_model(self, now: DateTime<Utc>) -> MlModel {
        MlModel {
            id: self.id,
            name: self.name,
            kind: self.kind,
            version: self.version,
            status: self.status,
            accuracy: self.accuracy,
            accuracy_metric: self.accuracy_metric,
            training_data_ref: self.training_data_ref,
            description: self.description,
            inference_spec: self.inference_spec,
            created_at: now,
            updated_at: now,
        }
    }
}

fn phase_two_seeds() -> &'static [MlModelSeed] {
    static CACHE: OnceLock<Vec<MlModelSeed>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let parsed: PhaseTwoSeeds = toml::from_str(PHASE_TWO_SEEDS_TOML)
            .expect("phase_two_models.toml ships with the crate; parse should never fail");
        parsed.model
    })
}

/// Upsert the candidate models. Safe to call on every service
/// startup — the repo's `upsert_model` is idempotent.
pub async fn seed_phase_two_candidates(repo: &dyn MlRepository) -> Result<usize, MlError> {
    let now = Utc::now();
    let mut count = 0;
    for seed in phase_two_seeds() {
        let model = seed.clone().into_model(now);
        repo.upsert_model(&model).await?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryMlRepo;

    #[tokio::test]
    async fn seeds_phase_two_models() {
        let repo = InMemoryMlRepo::new();
        let n = seed_phase_two_candidates(&repo).await.unwrap();
        assert_eq!(n, 9);
        let out = repo.all_model_summaries(None).await.unwrap();
        assert_eq!(out.len(), 9);
        let names: Vec<&str> = out.iter().map(|s| s.model.name.as_str()).collect();
        assert!(names.contains(&"account-churn-risk"));
        assert!(names.contains(&"device-mtbf"));
        assert!(names.contains(&"opportunity-win-probability"));
        assert!(names.contains(&"next-action-contract-expiring"));
        assert!(names.contains(&"next-action-past-due-invoice"));
        assert!(names.contains(&"next-action-missing-primary-contact"));
        assert!(names.contains(&"next-action-high-churn-risk"));
        assert!(names.contains(&"next-action-stalled-service-ticket"));
        assert!(names.contains(&"next-action-preventive-maintenance-due"));
    }

    #[tokio::test]
    async fn seed_is_idempotent() {
        let repo = InMemoryMlRepo::new();
        seed_phase_two_candidates(&repo).await.unwrap();
        seed_phase_two_candidates(&repo).await.unwrap();
        let out = repo.all_model_summaries(None).await.unwrap();
        assert_eq!(out.len(), 9, "second seed should not duplicate rows");
    }

    #[tokio::test]
    async fn next_action_seeds_are_inference_driven() {
        let repo = InMemoryMlRepo::new();
        seed_phase_two_candidates(&repo).await.unwrap();
        for id in [
            "mdl-next-action-contract-expiring-v1",
            "mdl-next-action-past-due-invoice-v1",
            "mdl-next-action-missing-primary-contact-v1",
            "mdl-next-action-high-churn-risk-v1",
            "mdl-next-action-stalled-service-ticket-v1",
            "mdl-next-action-preventive-maintenance-due-v1",
        ] {
            let s = repo
                .model_summary_by_id(id)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("seed {id} missing"));
            assert_eq!(s.model.kind, ModelKind::DeclarativeRule);
            assert_eq!(s.model.status, ModelStatus::Active);
            match &s.model.inference_spec {
                Some(InferenceSpec::DeclarativeRule(spec)) => {
                    assert_eq!(spec.entity_type, "account");
                    assert_eq!(spec.entity_id_column, "account_id");
                    assert_eq!(spec.score_expr, "score");
                }
                other => panic!("{id} expected DeclarativeRule, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn churn_risk_seed_is_inference_driven() {
        let repo = InMemoryMlRepo::new();
        seed_phase_two_candidates(&repo).await.unwrap();
        let summary = repo
            .model_summary_by_id("mdl-account-churn-risk-v1")
            .await
            .unwrap()
            .expect("seed registered");
        assert_eq!(summary.model.kind, ModelKind::HeuristicFormula);
        assert_eq!(summary.model.status, ModelStatus::Active);
        match summary.model.inference_spec {
            Some(InferenceSpec::HeuristicFormula(s)) => {
                assert_eq!(s.plugin_name, "account-churn-risk-composite-v1");
                assert_eq!(s.entity_type, "account");
            }
            other => panic!("expected HeuristicFormula, got {other:?}"),
        }
    }

    #[test]
    fn toml_parses_at_compile_time() {
        // Belt-and-suspenders: the OnceLock parse is lazy, so a
        // malformed TOML would only surface on first seed call. This
        // test forces the parse + asserts the count to fail loud at
        // `cargo test`.
        let seeds = phase_two_seeds();
        assert_eq!(
            seeds.len(),
            9,
            "phase_two_models.toml should have 9 model blocks"
        );
    }
}
