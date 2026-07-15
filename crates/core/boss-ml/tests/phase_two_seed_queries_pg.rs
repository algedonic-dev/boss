//! Every seeded declarative-rule query must execute against the
//! current schema — the regression net for schema drift inside model
//! seeds. The fleet-era `systems` table lingered in two queries long
//! after the v1.1.0 fleet→assets rename (the nightly infer-batch
//! 500'd on the playground, and the failure aborted the batch so
//! every model after the broken one silently never ran) because
//! nothing executed the seed SQL at test time. This does.
//!
//! Mirrors the runtime contract in `inference.rs`: `$1` is bound to
//! the as-of date, and entity-filtered mode wraps the base query in
//! `WITH inner_q AS (…) SELECT * FROM inner_q WHERE {entity_id_column}
//! = $2` — both forms must prepare and run.

use serde::Deserialize;

const SEEDS_TOML: &str = include_str!("../seeds/phase_two_models.toml");

#[derive(Deserialize)]
struct Seeds {
    model: Vec<ModelSeed>,
}

#[derive(Deserialize)]
struct ModelSeed {
    id: String,
    /// Absent on the draft stub models that reserve catalog identity
    /// before they're built.
    #[serde(default)]
    inference_spec: Option<InferenceSpecSeed>,
}

/// Permissive view of `[model.inference_spec]` — heuristic-formula
/// specs carry different fields; only declarative-rule entries have
/// `query` + `entity_id_column`.
#[derive(Deserialize)]
struct InferenceSpecSeed {
    kind: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    entity_id_column: Option<String>,
}

#[tokio::test(flavor = "multi_thread")]
async fn declarative_rule_seed_queries_execute_against_schema() {
    let db = boss_testing::TestDb::new().await;
    let seeds: Seeds = toml::from_str(SEEDS_TOML).expect("phase_two_models.toml parses");
    // Fixed literal, not wallclock — the value is irrelevant, the
    // schema references are what's under test.
    let as_of = chrono::NaiveDate::from_ymd_opt(2026, 7, 15).expect("valid date");

    let mut checked = 0usize;
    for model in &seeds.model {
        let Some(spec) = &model.inference_spec else {
            continue;
        };
        if spec.kind != "declarative-rule" {
            continue;
        }
        let query = spec
            .query
            .as_deref()
            .unwrap_or_else(|| panic!("{}: declarative-rule seed without a query", model.id));

        // Batch mode: the query as shipped, $1 = as-of date. An empty
        // schema returns zero rows; a dead relation/column errors.
        if let Err(e) = sqlx::query(query).bind(as_of).fetch_all(&db.pool).await {
            panic!(
                "{}: seed query no longer executes against the schema: {e}",
                model.id
            );
        }

        // Entity-filtered mode, exactly as inference.rs wraps it —
        // catches an entity_id_column that the SELECT list doesn't
        // actually expose.
        let col = spec.entity_id_column.as_deref().unwrap_or_else(|| {
            panic!(
                "{}: declarative-rule seed without entity_id_column",
                model.id
            )
        });
        let wrapped = format!("WITH inner_q AS ({query}) SELECT * FROM inner_q WHERE {col} = $2");
        if let Err(e) = sqlx::query(&wrapped)
            .bind(as_of)
            .bind("entity-probe")
            .fetch_all(&db.pool)
            .await
        {
            panic!(
                "{}: entity-filtered wrap (column `{col}`) fails against the schema: {e}",
                model.id
            );
        }

        checked += 1;
    }

    assert!(
        checked >= 2,
        "expected at least two declarative-rule seeds to check, found {checked}"
    );
}
