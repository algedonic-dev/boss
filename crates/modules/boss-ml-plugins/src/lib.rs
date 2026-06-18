//! `boss-ml-plugins` — canonical inference plugins for boss-ml's
//! dispatcher.
//!
//! Each plugin implements `boss_ml::InferencePlugin`. The
//! `boss-ml-api` binary registers them at startup so the
//! corresponding model rows in `ml_models` (kind =
//! `heuristic-formula`, `inference_spec.plugin_name`) can be
//! evaluated by `infer_one` / `infer_batch`.

pub mod account_churn_risk_v1;

pub use account_churn_risk_v1::AccountChurnRiskV1;
