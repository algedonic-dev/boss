//! Inference runtime — `InferencePlugin` trait, `InferContext` /
//! `InferOutput` data types, and the `InferenceDispatcher` that runs
//! declarative-rule and heuristic-formula models.
//!
//! Declarative-rule models run their SQL directly; heuristic-formula
//! models call out to a registered `InferencePlugin`. A model whose
//! kind has no executor wired yields `InferError::Unimplemented`.

use serde::Serialize;

#[cfg(feature = "postgres")]
use std::collections::HashMap;
#[cfg(feature = "postgres")]
use std::sync::Arc;

#[cfg(feature = "postgres")]
use async_trait::async_trait;
#[cfg(feature = "postgres")]
use chrono::{DateTime, Utc};

#[cfg(feature = "postgres")]
use sqlx::PgPool;

use crate::port::MlError;
#[cfg(feature = "postgres")]
use crate::port::MlRepository;
#[cfg(feature = "postgres")]
use crate::types::{InferenceSpec, MlModel, MlPrediction, ModelKind};

/// Context handed to an `InferencePlugin` for each call. Holds
/// the resources the plugin can use to query domain data and
/// stamp predictions.
#[cfg(feature = "postgres")]
pub struct InferContext<'a> {
    pub pool: &'a PgPool,
    pub now: DateTime<Utc>,
}

/// What a plugin returns for a single entity.
#[derive(Debug, Clone, Serialize)]
pub struct InferOutput {
    pub score: f64,
    pub payload: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum InferError {
    #[error("plugin failure: {0}")]
    Plugin(String),
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not implemented: {0}")]
    Unimplemented(&'static str),
}

impl From<MlError> for InferError {
    fn from(e: MlError) -> Self {
        match e {
            MlError::Storage(s) => InferError::Storage(s),
            MlError::NotFound(s) => InferError::NotFound(s),
            MlError::BadRequest(s) => InferError::BadRequest(s),
        }
    }
}

/// Tenant- or core-supplied scoring code for `heuristic-formula`
/// models. Each plugin implements this trait and registers itself
/// against the dispatcher at boot.
#[cfg(feature = "postgres")]
#[async_trait]
pub trait InferencePlugin: Send + Sync {
    /// Stable name — must match the `plugin_name` field in the
    /// model's `inference_spec`.
    fn name(&self) -> &'static str;

    async fn infer(
        &self,
        entity_type: &str,
        entity_id: &str,
        ctx: &InferContext<'_>,
    ) -> Result<InferOutput, InferError>;
}

/// Summary returned by `infer_batch`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BatchInferReport {
    pub model_id: String,
    pub written: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Inference dispatcher. Holds the registered plugin set + a handle
/// to the repository, and routes each model to its executor.
#[cfg(feature = "postgres")]
pub struct InferenceDispatcher {
    pool: PgPool,
    repo: Arc<dyn MlRepository>,
    plugins: HashMap<&'static str, Arc<dyn InferencePlugin>>,
    /// Sim/wall-aware clock — every "today" lookup in declarative
    /// rules + plugin paths routes through this so sim-mode
    /// inferences see sim-today, not wallclock. (A direct
    /// `Utc::now()` or a hardcoded `CURRENT_DATE` would fire
    /// contract-expiry alerts on the wrong agreements in sim mode.)
    clock: Arc<dyn boss_clock_client::ClockClient>,
}

#[cfg(feature = "postgres")]
impl InferenceDispatcher {
    pub fn new(
        pool: PgPool,
        repo: Arc<dyn MlRepository>,
        clock: Arc<dyn boss_clock_client::ClockClient>,
    ) -> Self {
        Self {
            pool,
            repo,
            plugins: HashMap::new(),
            clock,
        }
    }

    /// Register a plugin. Last writer wins for a given name; the
    /// dispatcher errors on duplicate registration in tests via
    /// `register_plugin_strict` if needed later.
    pub fn register_plugin(&mut self, plugin: Arc<dyn InferencePlugin>) {
        self.plugins.insert(plugin.name(), plugin);
    }

    pub fn plugin_names(&self) -> Vec<&'static str> {
        self.plugins.keys().copied().collect()
    }

    /// Run the model's inference for one entity, write the
    /// prediction, and return it.
    ///
    /// `declarative-rule` is implemented in step 3 by wrapping
    /// the model's SQL in a single-entity filter; the broader
    /// batch path is identical otherwise.
    /// `heuristic-formula` lands in step 4.
    pub async fn infer_one(
        &self,
        model_id: &str,
        entity_id: &str,
    ) -> Result<MlPrediction, InferError> {
        let model = self.load_inference_model(model_id).await?;
        match &model.kind {
            ModelKind::DeclarativeRule => {
                let mut report = BatchInferReport {
                    model_id: model.id.clone(),
                    ..Default::default()
                };
                let predictions = self
                    .run_declarative_rule(&model, Some(entity_id), &mut report)
                    .await?;
                predictions.into_iter().next().ok_or_else(|| {
                    InferError::NotFound(format!(
                        "model {model_id} produced no row for entity_id={entity_id}"
                    ))
                })
            }
            ModelKind::HeuristicFormula => {
                let spec = self.heuristic_formula_spec(&model)?.clone();
                let plugin = self
                    .plugins
                    .get(spec.plugin_name.as_str())
                    .cloned()
                    .ok_or_else(|| {
                        InferError::NotFound(format!(
                            "no plugin registered with name `{}`",
                            spec.plugin_name
                        ))
                    })?;
                let mut report = BatchInferReport {
                    model_id: model.id.clone(),
                    ..Default::default()
                };
                let preds = self
                    .run_heuristic_formula_one(
                        &model,
                        &spec,
                        plugin.as_ref(),
                        entity_id,
                        &mut report,
                    )
                    .await?;
                preds.into_iter().next().ok_or_else(|| {
                    InferError::NotFound(format!(
                        "plugin `{}` produced no output for entity_id={entity_id}",
                        spec.plugin_name
                    ))
                })
            }
            other => Err(InferError::BadRequest(format!(
                "model {model_id} kind `{}` is not inference-driven",
                other.as_str()
            ))),
        }
    }

    /// Run the model's inference across every matching entity.
    /// `declarative-rule` runs the SQL once;
    /// `heuristic-formula` enumerates entities via the spec's
    /// `entity_query`, then calls the registered plugin per entity.
    pub async fn infer_batch(&self, model_id: &str) -> Result<BatchInferReport, InferError> {
        let model = self.load_inference_model(model_id).await?;
        let mut report = BatchInferReport {
            model_id: model.id.clone(),
            ..Default::default()
        };
        match &model.kind {
            ModelKind::DeclarativeRule => {
                self.run_declarative_rule(&model, None, &mut report).await?;
                Ok(report)
            }
            ModelKind::HeuristicFormula => {
                let spec = self.heuristic_formula_spec(&model)?.clone();
                let plugin = self
                    .plugins
                    .get(spec.plugin_name.as_str())
                    .cloned()
                    .ok_or_else(|| {
                        InferError::NotFound(format!(
                            "no plugin registered with name `{}`",
                            spec.plugin_name
                        ))
                    })?;
                self.run_heuristic_formula_batch(&model, &spec, plugin.as_ref(), &mut report)
                    .await?;
                Ok(report)
            }
            other => Err(InferError::BadRequest(format!(
                "model {model_id} kind `{}` is not inference-driven",
                other.as_str()
            ))),
        }
    }

    /// Step 4 single-entity path. The dispatcher already validated
    /// the model and looked up the plugin; this just calls the
    /// plugin and writes one prediction.
    async fn run_heuristic_formula_one(
        &self,
        model: &MlModel,
        spec: &crate::types::HeuristicFormulaSpec,
        plugin: &dyn InferencePlugin,
        entity_id: &str,
        report: &mut BatchInferReport,
    ) -> Result<Vec<MlPrediction>, InferError> {
        let today = self.clock.now().await.now.date_naive();
        let pred = self
            .invoke_plugin(model, spec, plugin, entity_id, &today, report)
            .await;
        Ok(pred.into_iter().collect())
    }

    /// Step 4 batch path. Enumerates entity ids via `entity_query`
    /// (one column, named `id` or `entity_id` — first column is
    /// taken so the spec author isn't bound to a particular alias),
    /// calls the plugin per entity, accumulates the report.
    async fn run_heuristic_formula_batch(
        &self,
        model: &MlModel,
        spec: &crate::types::HeuristicFormulaSpec,
        plugin: &dyn InferencePlugin,
        report: &mut BatchInferReport,
    ) -> Result<(), InferError> {
        use sqlx::Row;
        let rows = sqlx::query(&spec.entity_query)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| InferError::Storage(format!("entity_query failed: {e}")))?;

        let today = self.clock.now().await.now.date_naive();
        for row in &rows {
            // Tolerate either the column being named `id`,
            // `entity_id`, or simply being the first column.
            let entity_id = row
                .try_get::<String, _>("entity_id")
                .or_else(|_| row.try_get::<String, _>("id"))
                .or_else(|_| row.try_get::<String, _>(0));
            let entity_id = match entity_id {
                Ok(v) => v,
                Err(e) => {
                    report
                        .errors
                        .push(format!("entity_query row missing id column: {e}"));
                    report.skipped += 1;
                    continue;
                }
            };
            let _ = self
                .invoke_plugin(model, spec, plugin, &entity_id, &today, report)
                .await;
        }
        Ok(())
    }

    /// Plugin call + write. Returns `Some(prediction)` on success,
    /// `None` on plugin or storage error (which has already been
    /// recorded in `report`).
    async fn invoke_plugin(
        &self,
        model: &MlModel,
        spec: &crate::types::HeuristicFormulaSpec,
        plugin: &dyn InferencePlugin,
        entity_id: &str,
        today: &chrono::NaiveDate,
        report: &mut BatchInferReport,
    ) -> Option<MlPrediction> {
        // Source `now` from ClockClient — the same instant the
        // dispatcher used for `today` above — so plugins that read
        // `ctx.now` (e.g. churn-risk's "days since last invoice")
        // respect sim-time.
        let now_resp = self.clock.now().await;
        let ctx = InferContext {
            pool: &self.pool,
            now: now_resp.now,
        };
        let output = match plugin.infer(&spec.entity_type, entity_id, &ctx).await {
            Ok(o) => o,
            Err(e) => {
                report.errors.push(format!(
                    "plugin `{}` for {}: {e}",
                    spec.plugin_name, entity_id
                ));
                report.skipped += 1;
                return None;
            }
        };
        let pred_id = format!("pred-{}-{}-{}", model.id, entity_id, today);
        let input = crate::types::CreatePredictionInput {
            id: pred_id,
            model_id: model.id.clone(),
            entity_type: spec.entity_type.clone(),
            entity_id: entity_id.to_string(),
            score: output.score,
            payload: Some(output.payload),
        };
        match self.repo.create_prediction(&input).await {
            Ok(p) => {
                report.written += 1;
                Some(p)
            }
            Err(e) => {
                report
                    .errors
                    .push(format!("storage write for {entity_id}: {e}"));
                report.skipped += 1;
                None
            }
        }
    }

    /// Step 3: execute a `declarative-rule` model. When `entity_id`
    /// is `Some`, wraps the model's SQL in a single-row CTE filter
    /// so `infer_one` reuses the same code path. Returns every
    /// prediction written (the caller decides what to do with
    /// them); the `report` accumulates totals + errors.
    ///
    /// The `score_expr` column must be a Postgres `float8` / `int*`
    /// — we don't decode `numeric` (avoids the `bigdecimal` sqlx
    /// feature dep). Authors should `::float8` arithmetic results
    /// in the model's SQL: e.g. `(end_date - CURRENT_DATE)::float`.
    async fn run_declarative_rule(
        &self,
        model: &MlModel,
        entity_id: Option<&str>,
        report: &mut BatchInferReport,
    ) -> Result<Vec<MlPrediction>, InferError> {
        let spec = match model.inference_spec.as_ref() {
            Some(InferenceSpec::DeclarativeRule(s)) => s,
            _ => {
                return Err(InferError::BadRequest(format!(
                    "model {} expected declarative-rule spec",
                    model.id
                )));
            }
        };

        // Bind sim-today as $1 to every declarative-rule query so
        // TOML authors use `$1::date` instead of `CURRENT_DATE` (which
        // would leak wallclock in sim mode). Entity-filtered single-row
        // mode wraps the base query and adds entity_id as $2.
        let today = self.clock.now().await.now.date_naive();
        let final_sql = match entity_id {
            Some(_) => format!(
                "WITH inner_q AS ({base}) \
                 SELECT * FROM inner_q WHERE {col} = $2",
                base = spec.query,
                col = spec.entity_id_column,
            ),
            None => spec.query.clone(),
        };

        let mut q = sqlx::query(&final_sql).bind(today);
        if let Some(eid) = entity_id {
            q = q.bind(eid);
        }
        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| InferError::Storage(format!("declarative-rule query failed: {e}")))?;
        let mut written = Vec::with_capacity(rows.len());
        for row in &rows {
            match self.row_to_prediction(model, spec, row, &today).await {
                Ok(p) => {
                    written.push(p);
                    report.written += 1;
                }
                Err(e) => {
                    report.errors.push(e.to_string());
                    report.skipped += 1;
                }
            }
        }
        Ok(written)
    }

    /// Map one SQL row to a stored prediction:
    ///   - read entity_id from `spec.entity_id_column`
    ///   - read score from `spec.score_expr` (a column name)
    ///   - render `spec.payload_template` against the row
    ///   - upsert into ml_predictions with a stable id keyed on
    ///     (model_id, entity_id, day) so daily reruns are
    ///     idempotent within the day.
    async fn row_to_prediction(
        &self,
        model: &MlModel,
        spec: &crate::types::DeclarativeRuleSpec,
        row: &sqlx::postgres::PgRow,
        today: &chrono::NaiveDate,
    ) -> Result<MlPrediction, InferError> {
        use sqlx::Row;

        let entity_id: String = row
            .try_get::<String, _>(spec.entity_id_column.as_str())
            .map_err(|e| {
                InferError::BadRequest(format!(
                    "row missing string column `{}`: {e}",
                    spec.entity_id_column
                ))
            })?;
        let score: f64 = read_f64(row, &spec.score_expr).map_err(|e| {
            InferError::BadRequest(format!("row score column `{}`: {e}", spec.score_expr))
        })?;
        let payload = render_template(&spec.payload_template, row);

        let row_key_part = match spec.row_key_column.as_deref() {
            Some(col) => {
                let raw = cell_to_json(row, col);
                let s = match &raw {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                };
                format!("-{s}")
            }
            None => String::new(),
        };
        let pred_id = format!("pred-{}-{}-{}{}", model.id, entity_id, today, row_key_part);
        let input = crate::types::CreatePredictionInput {
            id: pred_id,
            model_id: model.id.clone(),
            entity_type: spec.entity_type.clone(),
            entity_id,
            score,
            payload: Some(payload),
        };
        let pred = self.repo.create_prediction(&input).await?;
        Ok(pred)
    }

    async fn load_inference_model(&self, model_id: &str) -> Result<MlModel, InferError> {
        let summary = self
            .repo
            .model_summary_by_id(model_id)
            .await?
            .ok_or_else(|| InferError::NotFound(format!("model {model_id}")))?;
        if !summary.model.kind.is_inference_driven() {
            return Err(InferError::BadRequest(format!(
                "model {model_id} kind `{}` is not inference-driven",
                summary.model.kind.as_str()
            )));
        }
        if summary.model.inference_spec.is_none() {
            return Err(InferError::BadRequest(format!(
                "model {model_id} kind `{}` requires inference_spec",
                summary.model.kind.as_str()
            )));
        }
        Ok(summary.model)
    }

    fn heuristic_formula_spec<'a>(
        &self,
        model: &'a MlModel,
    ) -> Result<&'a crate::types::HeuristicFormulaSpec, InferError> {
        match model.inference_spec.as_ref() {
            Some(InferenceSpec::HeuristicFormula(s)) => Ok(s),
            Some(InferenceSpec::DeclarativeRule(_)) | None => Err(InferError::BadRequest(format!(
                "model {} expected heuristic-formula spec",
                model.id
            ))),
        }
    }

    /// Borrow the underlying pool for plugins / executors.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Read a numeric column as f64, accepting f64 / f32 / i64 / i32 /
/// i16 / Decimal-as-string. Postgres returns subtraction of two
/// dates as `i32` and arithmetic results often as `numeric`, so we
/// cast through whatever shape the driver hands us.
#[cfg(feature = "postgres")]
fn read_f64(row: &sqlx::postgres::PgRow, col: &str) -> Result<f64, String> {
    use sqlx::Row;
    if let Ok(v) = row.try_get::<f64, _>(col) {
        return Ok(v);
    }
    if let Ok(v) = row.try_get::<f32, _>(col) {
        return Ok(v as f64);
    }
    if let Ok(v) = row.try_get::<i64, _>(col) {
        return Ok(v as f64);
    }
    if let Ok(v) = row.try_get::<i32, _>(col) {
        return Ok(v as f64);
    }
    if let Ok(v) = row.try_get::<i16, _>(col) {
        return Ok(v as f64);
    }
    Err(format!("column `{col}` is not a numeric type"))
}

/// Render a payload template against a SQL row. Strings of the
/// shape `"{{column_name}}"` are replaced wholesale by the column
/// value (preserving its native JSON type — number stays number).
/// Strings with `{{...}}` interpolation inside text are
/// supported too via simple string substitution. Non-string
/// template nodes pass through unchanged.
#[cfg(feature = "postgres")]
fn render_template(template: &serde_json::Value, row: &sqlx::postgres::PgRow) -> serde_json::Value {
    use serde_json::Value;
    match template {
        Value::String(s) => {
            // Whole-string token: "{{col}}" → typed cell value.
            if let Some(col) = s
                .strip_prefix("{{")
                .and_then(|s| s.strip_suffix("}}"))
                .map(str::trim)
                && !col.is_empty()
            {
                return cell_to_json(row, col);
            }
            // Inline interpolation inside a longer string.
            Value::String(interpolate_string(s, row))
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| render_template(v, row)).collect())
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), render_template(v, row));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(feature = "postgres")]
fn interpolate_string(s: &str, row: &sqlx::postgres::PgRow) -> String {
    use serde_json::Value;
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        match after_open.find("}}") {
            Some(close) => {
                let col = after_open[..close].trim();
                let cell = cell_to_json(row, col);
                match cell {
                    Value::String(s) => out.push_str(&s),
                    Value::Null => {}
                    other => out.push_str(&other.to_string()),
                }
                rest = &after_open[close + 2..];
            }
            None => {
                out.push_str(&rest[open..]);
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Read a single cell as JSON. Tries common Postgres types in
/// order; falls back to Null if none match (the dispatcher writes
/// the prediction anyway, with the cell elided from payload).
#[cfg(feature = "postgres")]
fn cell_to_json(row: &sqlx::postgres::PgRow, col: &str) -> serde_json::Value {
    use serde_json::Value;
    use sqlx::Row;
    if let Ok(v) = row.try_get::<Option<String>, _>(col) {
        return v.map(Value::String).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<f64>, _>(col) {
        return v
            .and_then(|x| serde_json::Number::from_f64(x).map(Value::Number))
            .unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i64>, _>(col) {
        return v.map(|x| Value::Number(x.into())).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i32>, _>(col) {
        return v
            .map(|x| Value::Number((x as i64).into()))
            .unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i16>, _>(col) {
        return v
            .map(|x| Value::Number((x as i64).into()))
            .unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<bool>, _>(col) {
        return v.map(Value::Bool).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<chrono::NaiveDate>, _>(col) {
        return v
            .map(|d| Value::String(d.to_string()))
            .unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(col) {
        return v
            .map(|d| Value::String(d.to_rfc3339()))
            .unwrap_or(Value::Null);
    }
    Value::Null
}

#[cfg(all(test, feature = "postgres"))]
mod tests {
    use super::*;
    use crate::InMemoryMlRepo;
    use crate::types::{
        DeclarativeRuleSpec, HeuristicFormulaSpec, InferenceSpec, MlModel, ModelKind, ModelStatus,
    };
    use boss_testing::TestDb;
    use chrono::Utc;
    use std::sync::Arc;

    fn dr_model(id: &str, spec: DeclarativeRuleSpec) -> MlModel {
        MlModel {
            id: id.to_string(),
            name: id.to_string(),
            kind: ModelKind::DeclarativeRule,
            version: "v1".to_string(),
            status: ModelStatus::Active,
            accuracy: None,
            accuracy_metric: None,
            training_data_ref: None,
            description: None,
            inference_spec: Some(InferenceSpec::DeclarativeRule(spec)),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn hf_model(id: &str, spec: HeuristicFormulaSpec) -> MlModel {
        MlModel {
            id: id.to_string(),
            name: id.to_string(),
            kind: ModelKind::HeuristicFormula,
            version: "v1".to_string(),
            status: ModelStatus::Active,
            accuracy: None,
            accuracy_metric: None,
            training_data_ref: None,
            description: None,
            inference_spec: Some(InferenceSpec::HeuristicFormula(spec)),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Test plugin — returns score = digits-in-id / 100.0 plus
    /// payload echoing the entity_id. Lets us assert the
    /// dispatcher invoked it once per row with the right ids.
    struct DigitScorer;

    #[async_trait]
    impl InferencePlugin for DigitScorer {
        fn name(&self) -> &'static str {
            "digit-scorer"
        }
        async fn infer(
            &self,
            entity_type: &str,
            entity_id: &str,
            _ctx: &InferContext<'_>,
        ) -> Result<InferOutput, InferError> {
            let digits: String = entity_id.chars().filter(|c| c.is_ascii_digit()).collect();
            let n: f64 = digits.parse().unwrap_or(0.0);
            Ok(InferOutput {
                score: n / 100.0,
                payload: serde_json::json!({
                    "entity_type": entity_type,
                    "entity_id": entity_id,
                }),
            })
        }
    }

    /// Test plugin that always errors — exercises the
    /// dispatcher's report.errors path.
    struct AlwaysFails;

    #[async_trait]
    impl InferencePlugin for AlwaysFails {
        fn name(&self) -> &'static str {
            "always-fails"
        }
        async fn infer(
            &self,
            _entity_type: &str,
            entity_id: &str,
            _ctx: &InferContext<'_>,
        ) -> Result<InferOutput, InferError> {
            Err(InferError::Plugin(format!(
                "synthetic failure for {entity_id}"
            )))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn declarative_rule_writes_predictions() {
        let db = TestDb::new().await;
        let pool = &db.pool;
        sqlx::query(
            "CREATE TABLE fake_accounts (
                id TEXT PRIMARY KEY,
                end_date DATE NOT NULL
            )",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO fake_accounts VALUES ('a-1', CURRENT_DATE + 14), ('a-2', CURRENT_DATE + 45), ('a-3', CURRENT_DATE + 90)")
            .execute(pool)
            .await
            .unwrap();

        let repo: Arc<dyn MlRepository> = Arc::new(InMemoryMlRepo::new());
        let model = dr_model(
            "mdl-test",
            DeclarativeRuleSpec {
                query: "SELECT id AS account_id, \
                        (end_date - $1::date)::int AS days_remaining, \
                        ((end_date - $1::date)::float / 60.0) AS score \
                    FROM fake_accounts \
                    WHERE end_date BETWEEN $1::date AND $1::date + 60"
                    .into(),
                score_expr: "score".into(),
                entity_type: "account".into(),
                entity_id_column: "account_id".into(),
                row_key_column: None,
                payload_template: serde_json::json!({
                    "rule": "contract-expiring-soon",
                    "days_remaining": "{{days_remaining}}"
                }),
            },
        );
        repo.upsert_model(&model).await.unwrap();
        let dispatcher = InferenceDispatcher::new(
            pool.clone(),
            repo.clone(),
            Arc::new(boss_clock_client::WallClockClient),
        );
        let report = dispatcher.infer_batch("mdl-test").await.unwrap();
        assert_eq!(report.written, 2, "two accounts within 60-day window");
        assert!(
            report.errors.is_empty(),
            "no row errors: {:?}",
            report.errors
        );

        let preds = repo
            .recent_predictions_for_model("mdl-test", 10)
            .await
            .unwrap();
        assert_eq!(preds.len(), 2);
        for p in &preds {
            assert_eq!(p.entity_type, "account");
            assert!(p.score > 0.0 && p.score <= 1.0);
            let payload = p.payload.as_ref().unwrap();
            assert_eq!(payload["rule"], "contract-expiring-soon");
            assert!(payload["days_remaining"].is_number());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn declarative_rule_reruns_within_day_are_idempotent() {
        let db = TestDb::new().await;
        let pool = &db.pool;
        sqlx::query("CREATE TABLE fake_accounts (id TEXT PRIMARY KEY)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO fake_accounts VALUES ('a-1')")
            .execute(pool)
            .await
            .unwrap();

        let repo: Arc<dyn MlRepository> = Arc::new(InMemoryMlRepo::new());
        let model = dr_model(
            "mdl-idem",
            DeclarativeRuleSpec {
                query: "SELECT id AS account_id, 0.5::float8 AS score FROM fake_accounts".into(),
                score_expr: "score".into(),
                entity_type: "account".into(),
                entity_id_column: "account_id".into(),
                row_key_column: None,
                payload_template: serde_json::Value::Null,
            },
        );
        repo.upsert_model(&model).await.unwrap();
        let dispatcher = InferenceDispatcher::new(
            pool.clone(),
            repo.clone(),
            Arc::new(boss_clock_client::WallClockClient),
        );
        dispatcher.infer_batch("mdl-idem").await.unwrap();
        dispatcher.infer_batch("mdl-idem").await.unwrap();
        let preds = repo
            .recent_predictions_for_model("mdl-idem", 10)
            .await
            .unwrap();
        assert_eq!(preds.len(), 1, "second run is a no-op (same day)");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn infer_one_filters_to_single_entity() {
        let db = TestDb::new().await;
        let pool = &db.pool;
        sqlx::query("CREATE TABLE fake_accounts (id TEXT PRIMARY KEY)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO fake_accounts VALUES ('a-1'), ('a-2'), ('a-3')")
            .execute(pool)
            .await
            .unwrap();

        let repo: Arc<dyn MlRepository> = Arc::new(InMemoryMlRepo::new());
        let model = dr_model(
            "mdl-one",
            DeclarativeRuleSpec {
                query: "SELECT id AS account_id, 0.42::float8 AS score FROM fake_accounts".into(),
                score_expr: "score".into(),
                entity_type: "account".into(),
                entity_id_column: "account_id".into(),
                row_key_column: None,
                payload_template: serde_json::Value::Null,
            },
        );
        repo.upsert_model(&model).await.unwrap();
        let dispatcher = InferenceDispatcher::new(
            pool.clone(),
            repo.clone(),
            Arc::new(boss_clock_client::WallClockClient),
        );
        let p = dispatcher.infer_one("mdl-one", "a-2").await.unwrap();
        assert_eq!(p.entity_id, "a-2");
        assert_eq!(p.score, 0.42);
    }

    /// row_key_column lets a single rule fire multiple times per
    /// (entity, day). Without it, two rows for the same account
    /// collide on prediction id and only one survives.
    #[tokio::test(flavor = "multi_thread")]
    async fn row_key_column_allows_multiple_predictions_per_entity() {
        let db = TestDb::new().await;
        let pool = &db.pool;
        sqlx::query(
            "CREATE TABLE fake_contracts ( \
                 id TEXT PRIMARY KEY, \
                 account_id TEXT NOT NULL, \
                 days INT NOT NULL \
             )",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO fake_contracts VALUES \
                 ('c-1', 'a-1', 7), \
                 ('c-2', 'a-1', 25), \
                 ('c-3', 'a-1', 50)",
        )
        .execute(pool)
        .await
        .unwrap();

        let repo: Arc<dyn MlRepository> = Arc::new(InMemoryMlRepo::new());
        let model = dr_model(
            "mdl-multi",
            DeclarativeRuleSpec {
                query: "SELECT account_id, id AS contract_id, days, \
                        ((60 - days)::float8 / 60.0) AS score \
                        FROM fake_contracts"
                    .into(),
                score_expr: "score".into(),
                entity_type: "account".into(),
                entity_id_column: "account_id".into(),
                row_key_column: Some("contract_id".into()),
                payload_template: serde_json::json!({
                    "contract_id": "{{contract_id}}",
                    "days": "{{days}}"
                }),
            },
        );
        repo.upsert_model(&model).await.unwrap();
        let dispatcher = InferenceDispatcher::new(
            pool.clone(),
            repo.clone(),
            Arc::new(boss_clock_client::WallClockClient),
        );
        let report = dispatcher.infer_batch("mdl-multi").await.unwrap();
        assert_eq!(
            report.written, 3,
            "all 3 contracts should produce a prediction"
        );
        let preds = repo
            .recent_predictions_for_model("mdl-multi", 10)
            .await
            .unwrap();
        assert_eq!(preds.len(), 3);
        // All three are for the same account.
        assert!(preds.iter().all(|p| p.entity_id == "a-1"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn heuristic_formula_calls_plugin_per_entity() {
        let db = TestDb::new().await;
        let pool = &db.pool;
        sqlx::query("CREATE TABLE fake_accounts (id TEXT PRIMARY KEY)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO fake_accounts VALUES ('a-10'), ('a-25'), ('a-90')")
            .execute(pool)
            .await
            .unwrap();

        let repo: Arc<dyn MlRepository> = Arc::new(InMemoryMlRepo::new());
        let model = hf_model(
            "mdl-hf",
            HeuristicFormulaSpec {
                plugin_name: "digit-scorer".into(),
                entity_type: "account".into(),
                entity_query: "SELECT id FROM fake_accounts ORDER BY id".into(),
            },
        );
        repo.upsert_model(&model).await.unwrap();
        let mut dispatcher = InferenceDispatcher::new(
            pool.clone(),
            repo.clone(),
            Arc::new(boss_clock_client::WallClockClient),
        );
        dispatcher.register_plugin(Arc::new(DigitScorer));
        let report = dispatcher.infer_batch("mdl-hf").await.unwrap();
        assert_eq!(report.written, 3);
        assert!(report.errors.is_empty(), "no errors: {:?}", report.errors);

        let preds = repo
            .recent_predictions_for_model("mdl-hf", 10)
            .await
            .unwrap();
        assert_eq!(preds.len(), 3);
        let mut by_id: std::collections::HashMap<String, f64> = preds
            .iter()
            .map(|p| (p.entity_id.clone(), p.score))
            .collect();
        assert_eq!(by_id.remove("a-10"), Some(0.10));
        assert_eq!(by_id.remove("a-25"), Some(0.25));
        assert_eq!(by_id.remove("a-90"), Some(0.90));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn heuristic_formula_infer_one_skips_entity_query() {
        let db = TestDb::new().await;
        let pool = &db.pool;
        // entity_query is intentionally invalid SQL — infer_one
        // must not run it.
        let repo: Arc<dyn MlRepository> = Arc::new(InMemoryMlRepo::new());
        let model = hf_model(
            "mdl-hf-one",
            HeuristicFormulaSpec {
                plugin_name: "digit-scorer".into(),
                entity_type: "account".into(),
                entity_query: "SELECT id FROM table_does_not_exist".into(),
            },
        );
        repo.upsert_model(&model).await.unwrap();
        let mut dispatcher = InferenceDispatcher::new(
            pool.clone(),
            repo.clone(),
            Arc::new(boss_clock_client::WallClockClient),
        );
        dispatcher.register_plugin(Arc::new(DigitScorer));
        let p = dispatcher.infer_one("mdl-hf-one", "a-77").await.unwrap();
        assert_eq!(p.entity_id, "a-77");
        assert_eq!(p.score, 0.77);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn heuristic_formula_plugin_errors_become_skipped() {
        let db = TestDb::new().await;
        let pool = &db.pool;
        sqlx::query("CREATE TABLE fake_accounts (id TEXT PRIMARY KEY)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO fake_accounts VALUES ('a-1'), ('a-2')")
            .execute(pool)
            .await
            .unwrap();

        let repo: Arc<dyn MlRepository> = Arc::new(InMemoryMlRepo::new());
        let model = hf_model(
            "mdl-hf-fail",
            HeuristicFormulaSpec {
                plugin_name: "always-fails".into(),
                entity_type: "account".into(),
                entity_query: "SELECT id FROM fake_accounts".into(),
            },
        );
        repo.upsert_model(&model).await.unwrap();
        let mut dispatcher = InferenceDispatcher::new(
            pool.clone(),
            repo.clone(),
            Arc::new(boss_clock_client::WallClockClient),
        );
        dispatcher.register_plugin(Arc::new(AlwaysFails));
        let report = dispatcher.infer_batch("mdl-hf-fail").await.unwrap();
        assert_eq!(report.written, 0);
        assert_eq!(report.skipped, 2);
        assert_eq!(report.errors.len(), 2);
        for e in &report.errors {
            assert!(e.contains("synthetic failure"), "{e}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn heuristic_formula_unknown_plugin_is_not_found() {
        let db = TestDb::new().await;
        let pool = &db.pool;
        let repo: Arc<dyn MlRepository> = Arc::new(InMemoryMlRepo::new());
        let model = hf_model(
            "mdl-hf-missing",
            HeuristicFormulaSpec {
                plugin_name: "no-such-plugin".into(),
                entity_type: "account".into(),
                entity_query: "SELECT 'x' AS id".into(),
            },
        );
        repo.upsert_model(&model).await.unwrap();
        let dispatcher = InferenceDispatcher::new(
            pool.clone(),
            repo.clone(),
            Arc::new(boss_clock_client::WallClockClient),
        );
        let err = dispatcher.infer_batch("mdl-hf-missing").await.unwrap_err();
        assert!(matches!(err, InferError::NotFound(_)), "got {err:?}");
    }
}
