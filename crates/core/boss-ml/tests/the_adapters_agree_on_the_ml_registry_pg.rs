//! The ML model registry and prediction log answer the same on both
//! `MlRepository` adapters — reliability mechanism C of design
//! 3036296f, "adapters agree" (`boss_testing::adapters_agree!`), on the
//! next core port in the census (backlog be459ab9): `MlRepository`,
//! which `InMemoryMlRepo` and `PgMlRepo` implement.
//!
//! WHY THIS PORT. It holds every registered model and every score the
//! inference dispatcher or an outside writer records against an
//! entity. The dispatcher's own tests (`boss-ml/src/inference.rs`) and
//! the HTTP handlers' tests ask `InMemoryMlRepo`; production is
//! answered by `PgMlRepo`. Until this suite each adapter carried its
//! own hand-written unit tests, which held each to itself and never to
//! the other.
//!
//! The first run found six disagreements, fixed in this car in
//! production code:
//!
//! - THE MODEL LIST'S ORDER. `all_model_summaries` sorted by name and
//!   stopped there on both: Postgres in the database's locale collation
//!   (`suite-ab` before `suite-a-z`, case folded), the double in byte
//!   order, and two versions of one model (`UNIQUE (name, version)`
//!   allows it) in HashMap order against plan order. Both now sort
//!   byte-wise (`COLLATE "C"`) by name, then id. Case
//!   `the_model_list_is_byte_ordered_by_name_then_id`.
//! - A RE-UPSERT'S `created_at`. Postgres's `ON CONFLICT DO UPDATE`
//!   keeps the first `created_at`; the double replaced the whole row,
//!   so a restart's bootstrap re-stamped every model's birth in the
//!   double only. The double now keeps the first. Case
//!   `a_re_upsert_replaces_every_field_but_created_at`.
//! - A TAKEN (name, version). The table's `UNIQUE (name, version)`
//!   refused a second model under another id with the raw constraint
//!   text as a `Storage` error (a 500); the double wrote it. Both now
//!   refuse with `BadRequest` naming the holder's id, and write
//!   nothing. Case
//!   `a_taken_name_and_version_is_refused_and_changes_nothing`.
//! - A NEGATIVE LIMIT. Postgres answered `LIMIT -1` with its own error
//!   text as `Storage`; the double cast it to `usize` and answered
//!   every row. Both now refuse with `BadRequest` naming the limit.
//!   Case `a_negative_limit_is_refused_by_both_prediction_lists`.
//! - AN INSTANT'S PRECISION. Postgres keeps microseconds; the double
//!   kept nanoseconds, both on a model's instants and on the
//!   `created_at` it stamps a prediction with, so the same write read
//!   back unequal. The double now truncates to microseconds, as the
//!   column does. Cases `an_instant_reads_back_at_microsecond_precision`
//!   and `a_prediction_reads_back_as_written_and_a_replay_answers_the_first_row`.
//! - THE PREDICTION LISTS' TIE. Both lists promise newest first by
//!   `created_at` and stopped there, so two predictions stamped in the
//!   same microsecond answered in insertion order in the double and
//!   plan order in Postgres. Both now break the tie on id (byte order).
//!   The port stamps `created_at` itself, so no case can force a tie
//!   through it; the case below holds the order for distinct instants
//!   and the tie-break is read in both adapters' source.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other one.
//! Model instants are fixed; prediction instants are the adapter's own
//! clock and are judged by their order and precision, never by value.
//! The migrations seed no ml_models or ml_predictions row, and both
//! adapters start empty.

use std::time::Duration;

use boss_ml::{
    CreatePredictionInput, DeclarativeRuleSpec, InMemoryMlRepo, InferenceSpec, MlError, MlModel,
    MlModelSummary, MlPrediction, MlRepository, ModelKind, ModelStatus, PgMlRepo,
};
use chrono::{DateTime, TimeZone, Timelike, Utc};

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryMlRepo::new(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (PgMlRepo::new(db.pool.clone()), db)
        },
    }
    cases {
        an_empty_store_answers_nothing,
        a_model_reads_back_as_written_with_its_spec,
        the_model_list_is_byte_ordered_by_name_then_id,
        a_re_upsert_replaces_every_field_but_created_at,
        a_taken_name_and_version_is_refused_and_changes_nothing,
        an_instant_reads_back_at_microsecond_precision,
        a_prediction_reads_back_as_written_and_a_replay_answers_the_first_row,
        a_prediction_on_a_missing_model_is_refused_and_writes_nothing,
        the_prediction_lists_narrow_and_answer_newest_first_capped_by_limit,
        a_negative_limit_is_refused_by_both_prediction_lists,
    }
}

// ----- fixtures ------------------------------------------------------------

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 30, h, m, 0).unwrap()
}

fn model(id: &str, name: &str, version: &str) -> MlModel {
    MlModel {
        id: id.into(),
        name: name.into(),
        kind: ModelKind::RiskScore,
        version: version.into(),
        status: ModelStatus::Draft,
        accuracy: None,
        accuracy_metric: None,
        training_data_ref: None,
        description: None,
        inference_spec: None,
        created_at: at(9, 0),
        updated_at: at(9, 0),
    }
}

async fn put<R: MlRepository>(r: &R, m: &MlModel) {
    r.upsert_model(m).await.expect("upsert_model answers");
}

fn input(id: &str, model_id: &str, entity_type: &str, entity_id: &str) -> CreatePredictionInput {
    CreatePredictionInput {
        id: id.into(),
        model_id: model_id.into(),
        entity_type: entity_type.into(),
        entity_id: entity_id.into(),
        score: 0.5,
        payload: None,
    }
}

/// A prediction, then a pause long enough that the next one's
/// `created_at` is a later microsecond on either adapter's clock.
async fn predict<R: MlRepository>(r: &R, i: CreatePredictionInput) -> MlPrediction {
    let row = r.create_prediction(&i).await.expect("create_prediction");
    tokio::time::sleep(Duration::from_millis(5)).await;
    row
}

fn names(rows: &[MlModelSummary]) -> Vec<(String, String)> {
    rows.iter()
        .map(|s| (s.model.name.clone(), s.model.id.clone()))
        .collect()
}

fn ids(rows: &[MlPrediction]) -> Vec<String> {
    rows.iter().map(|p| p.id.clone()).collect()
}

fn micro_precise(t: DateTime<Utc>) -> bool {
    t.nanosecond().is_multiple_of(1_000)
}

// ----- cases ---------------------------------------------------------------

/// Nothing written: every read answers empty or absent.
async fn an_empty_store_answers_nothing<R: MlRepository>(r: &R, adapter: &str) {
    assert!(
        r.all_model_summaries(None).await.expect("list").is_empty(),
        "{adapter}"
    );
    assert!(
        r.all_model_summaries(Some(ModelStatus::Active))
            .await
            .expect("list")
            .is_empty(),
        "{adapter}"
    );
    assert_eq!(
        r.model_summary_by_id("m-1").await.expect("by id"),
        None,
        "{adapter}"
    );
    assert!(
        r.predictions_for_entity("account", "a-1", 10)
            .await
            .expect("for entity")
            .is_empty(),
        "{adapter}"
    );
    assert!(
        r.recent_predictions_for_model("m-1", 10)
            .await
            .expect("for model")
            .is_empty(),
        "{adapter}"
    );
}

/// An upsert answers every field it was written with — every optional
/// field set, an inference spec carried whole — and a model with no
/// predictions summarises to a zero count and no latest instant.
async fn a_model_reads_back_as_written_with_its_spec<R: MlRepository>(r: &R, adapter: &str) {
    let m = MlModel {
        kind: ModelKind::DeclarativeRule,
        status: ModelStatus::Active,
        accuracy: Some(0.82),
        accuracy_metric: Some("auc".into()),
        training_data_ref: Some("s3://bk/train.parquet".into()),
        description: Some("Churn risk, rule form".into()),
        inference_spec: Some(InferenceSpec::DeclarativeRule(DeclarativeRuleSpec {
            query: "SELECT id FROM accounts WHERE $1::date IS NOT NULL".into(),
            score_expr: "0.5".into(),
            entity_type: "account".into(),
            entity_id_column: "id".into(),
            row_key_column: Some("agreement_id".into()),
            payload_template: serde_json::json!({"rule": "churn", "weight": 2}),
        })),
        created_at: at(9, 0),
        updated_at: at(9, 30),
        ..model("m-1", "churn", "v1")
    };
    put(r, &m).await;
    let want = MlModelSummary {
        model: m,
        predictions_24h: 0,
        latest_prediction_at: None,
    };
    assert_eq!(
        r.model_summary_by_id("m-1").await.expect("by id"),
        Some(want.clone()),
        "{adapter}"
    );
    assert_eq!(
        r.all_model_summaries(None).await.expect("list"),
        vec![want.clone()],
        "{adapter}"
    );
    assert_eq!(
        r.all_model_summaries(Some(ModelStatus::Active))
            .await
            .expect("list active"),
        vec![want],
        "{adapter}"
    );
    assert!(
        r.all_model_summaries(Some(ModelStatus::Draft))
            .await
            .expect("list draft")
            .is_empty(),
        "{adapter}"
    );
}

/// The model list sorts byte-wise by name — upper case before lower,
/// `-` before a letter, whatever the database's locale — and two
/// versions of one name by id. The ids are planted out of insertion
/// order so neither a map's order nor a table's can pass for the
/// tie-break. The status filter keeps the same order.
async fn the_model_list_is_byte_ordered_by_name_then_id<R: MlRepository>(r: &R, adapter: &str) {
    put(r, &model("m-3", "suite-ab", "v1")).await;
    put(r, &model("m-9", "suite-a-z", "v2")).await;
    put(r, &model("m-1", "suite-a-z", "v1")).await;
    put(r, &model("m-5", "suite-a-z", "v3")).await;
    put(r, &model("m-7", "Suite-b", "v1")).await;

    let want: Vec<(String, String)> = [
        ("Suite-b", "m-7"),
        ("suite-a-z", "m-1"),
        ("suite-a-z", "m-5"),
        ("suite-a-z", "m-9"),
        ("suite-ab", "m-3"),
    ]
    .iter()
    .map(|(n, i)| (n.to_string(), i.to_string()))
    .collect();
    assert_eq!(
        names(&r.all_model_summaries(None).await.expect("list")),
        want,
        "{adapter}"
    );
    assert_eq!(
        names(
            &r.all_model_summaries(Some(ModelStatus::Draft))
                .await
                .expect("list draft")
        ),
        want,
        "{adapter}"
    );
}

/// A second upsert on the same id replaces every field, and keeps the
/// FIRST `created_at` — a restart's bootstrap re-stamps `updated_at`
/// only.
async fn a_re_upsert_replaces_every_field_but_created_at<R: MlRepository>(r: &R, adapter: &str) {
    put(r, &model("m-1", "churn", "v1")).await;
    let second = MlModel {
        name: "churn-renamed".into(),
        kind: ModelKind::Regression,
        version: "v2".into(),
        status: ModelStatus::Shadow,
        accuracy: Some(0.5),
        accuracy_metric: Some("rmse".into()),
        training_data_ref: Some("ref-2".into()),
        description: Some("second".into()),
        created_at: at(10, 0),
        updated_at: at(11, 0),
        ..model("m-1", "churn", "v1")
    };
    put(r, &second).await;
    let got = r
        .model_summary_by_id("m-1")
        .await
        .expect("by id")
        .expect("model");
    assert_eq!(
        got.model,
        MlModel {
            created_at: at(9, 0),
            ..second
        },
        "{adapter}"
    );
    assert_eq!(
        r.all_model_summaries(None).await.expect("list").len(),
        1,
        "{adapter}"
    );
}

/// A model under a new id on a (name, version) another model holds is
/// refused as `BadRequest` naming the holder, and writes nothing; so is
/// moving an existing model onto a held pair, which leaves it as it
/// was. The same name at another version is a different model.
async fn a_taken_name_and_version_is_refused_and_changes_nothing<R: MlRepository>(
    r: &R,
    adapter: &str,
) {
    put(r, &model("m-1", "churn", "v1")).await;
    match r.upsert_model(&model("m-2", "churn", "v1")).await {
        Err(MlError::BadRequest(msg)) => assert_eq!(
            msg, "model churn version v1 is already registered as m-1",
            "{adapter}"
        ),
        other => panic!("{adapter}: a taken pair expected BadRequest, got {other:?}"),
    }
    assert_eq!(
        r.model_summary_by_id("m-2").await.expect("by id"),
        None,
        "{adapter}"
    );

    put(r, &model("m-3", "churn", "v2")).await;
    let moved = MlModel {
        description: Some("moved".into()),
        ..model("m-3", "churn", "v1")
    };
    match r.upsert_model(&moved).await {
        Err(MlError::BadRequest(msg)) => assert_eq!(
            msg, "model churn version v1 is already registered as m-1",
            "{adapter}"
        ),
        other => panic!("{adapter}: a move onto a taken pair expected BadRequest, got {other:?}"),
    }
    let kept = r
        .model_summary_by_id("m-3")
        .await
        .expect("by id")
        .expect("model");
    assert_eq!(kept.model, model("m-3", "churn", "v2"), "{adapter}");
    assert_eq!(
        names(&r.all_model_summaries(None).await.expect("list")),
        vec![
            ("churn".to_string(), "m-1".to_string()),
            ("churn".to_string(), "m-3".to_string())
        ],
        "{adapter}"
    );
}

/// A model's instants read back truncated to the microsecond — the
/// column's precision — on both adapters.
async fn an_instant_reads_back_at_microsecond_precision<R: MlRepository>(r: &R, adapter: &str) {
    let fine = at(9, 0) + chrono::Duration::nanoseconds(123_456_789);
    let m = MlModel {
        created_at: fine,
        updated_at: fine,
        ..model("m-1", "churn", "v1")
    };
    put(r, &m).await;
    let want = at(9, 0) + chrono::Duration::nanoseconds(123_456_000);
    let got = r
        .model_summary_by_id("m-1")
        .await
        .expect("by id")
        .expect("model");
    assert_eq!(got.model.created_at, want, "{adapter}");
    assert_eq!(got.model.updated_at, want, "{adapter}");
}

/// A prediction answers every field it was written with and a
/// microsecond-precise `created_at`; `get`-by-list and the model's
/// summary agree with it. A replay of its id — even carrying other
/// values — writes nothing and answers the FIRST row.
async fn a_prediction_reads_back_as_written_and_a_replay_answers_the_first_row<R: MlRepository>(
    r: &R,
    adapter: &str,
) {
    put(r, &model("m-1", "churn", "v1")).await;
    let i = CreatePredictionInput {
        score: 0.42,
        payload: Some(serde_json::json!({"signal": "high-churn", "n": 3})),
        ..input("p-1", "m-1", "account", "a-1")
    };
    let first = r.create_prediction(&i).await.expect("create");
    assert_eq!(
        first,
        MlPrediction {
            id: "p-1".into(),
            model_id: "m-1".into(),
            entity_type: "account".into(),
            entity_id: "a-1".into(),
            score: 0.42,
            payload: Some(serde_json::json!({"signal": "high-churn", "n": 3})),
            created_at: first.created_at,
        },
        "{adapter}"
    );
    assert!(
        micro_precise(first.created_at),
        "{adapter}: {}",
        first.created_at
    );

    let replay = CreatePredictionInput {
        score: 0.99,
        payload: None,
        ..input("p-1", "m-1", "device", "d-1")
    };
    assert_eq!(
        r.create_prediction(&replay).await.expect("replay"),
        first,
        "{adapter}"
    );
    assert_eq!(
        r.recent_predictions_for_model("m-1", 10)
            .await
            .expect("for model"),
        vec![first.clone()],
        "{adapter}"
    );
    assert_eq!(
        r.predictions_for_entity("account", "a-1", 10)
            .await
            .expect("for entity"),
        vec![first.clone()],
        "{adapter}"
    );
    assert!(
        r.predictions_for_entity("device", "d-1", 10)
            .await
            .expect("for entity")
            .is_empty(),
        "{adapter}"
    );
    let s = r
        .model_summary_by_id("m-1")
        .await
        .expect("by id")
        .expect("model");
    assert_eq!(s.predictions_24h, 1, "{adapter}");
    assert_eq!(s.latest_prediction_at, Some(first.created_at), "{adapter}");
}

/// A prediction naming a model that does not exist is refused as
/// `NotFound` naming the model, and writes nothing.
async fn a_prediction_on_a_missing_model_is_refused_and_writes_nothing<R: MlRepository>(
    r: &R,
    adapter: &str,
) {
    put(r, &model("m-1", "churn", "v1")).await;
    match r
        .create_prediction(&input("p-1", "m-x", "account", "a-1"))
        .await
    {
        Err(MlError::NotFound(msg)) => assert_eq!(msg, "model m-x", "{adapter}"),
        other => panic!("{adapter}: a missing model expected NotFound, got {other:?}"),
    }
    assert!(
        r.predictions_for_entity("account", "a-1", 10)
            .await
            .expect("for entity")
            .is_empty(),
        "{adapter}"
    );
    assert!(
        r.recent_predictions_for_model("m-x", 10)
            .await
            .expect("for model")
            .is_empty(),
        "{adapter}"
    );
    // The id stays free: the same id on the real model is written.
    let row = r
        .create_prediction(&input("p-1", "m-1", "account", "a-1"))
        .await
        .expect("create");
    assert_eq!(row.model_id, "m-1", "{adapter}");
}

/// `predictions_for_entity` answers the rows whose entity type AND id
/// both match, across models; `recent_predictions_for_model` the rows
/// of that model; each newest first and capped at `limit`, and a limit
/// of zero answers nothing. The model's summary counts its rows and
/// names its newest instant.
async fn the_prediction_lists_narrow_and_answer_newest_first_capped_by_limit<R: MlRepository>(
    r: &R,
    adapter: &str,
) {
    put(r, &model("m-1", "churn", "v1")).await;
    put(r, &model("m-2", "mtbf", "v1")).await;
    predict(r, input("p-c", "m-1", "account", "a-1")).await;
    predict(r, input("p-a", "m-2", "account", "a-1")).await;
    predict(r, input("p-x", "m-1", "device", "a-1")).await;
    predict(r, input("p-y", "m-1", "account", "a-2")).await;
    let newest = predict(r, input("p-b", "m-1", "account", "a-1")).await;

    assert_eq!(
        ids(&r
            .predictions_for_entity("account", "a-1", 10)
            .await
            .expect("for entity")),
        vec!["p-b", "p-a", "p-c"],
        "{adapter}"
    );
    assert_eq!(
        ids(&r
            .predictions_for_entity("account", "a-1", 2)
            .await
            .expect("for entity")),
        vec!["p-b", "p-a"],
        "{adapter}"
    );
    assert_eq!(
        ids(&r
            .recent_predictions_for_model("m-1", 10)
            .await
            .expect("for model")),
        vec!["p-b", "p-y", "p-x", "p-c"],
        "{adapter}"
    );
    assert_eq!(
        ids(&r
            .recent_predictions_for_model("m-1", 3)
            .await
            .expect("for model")),
        vec!["p-b", "p-y", "p-x"],
        "{adapter}"
    );
    assert!(
        r.predictions_for_entity("account", "a-1", 0)
            .await
            .expect("for entity")
            .is_empty(),
        "{adapter}"
    );
    assert!(
        r.recent_predictions_for_model("m-1", 0)
            .await
            .expect("for model")
            .is_empty(),
        "{adapter}"
    );

    let s = r
        .model_summary_by_id("m-1")
        .await
        .expect("by id")
        .expect("model");
    assert_eq!(s.predictions_24h, 4, "{adapter}");
    assert_eq!(s.latest_prediction_at, Some(newest.created_at), "{adapter}");
    let listed = r.all_model_summaries(None).await.expect("list");
    let counts: Vec<i64> = listed.iter().map(|s| s.predictions_24h).collect();
    assert_eq!(counts, vec![4, 1], "{adapter}");
}

/// A negative limit is refused as `BadRequest` naming it, by both
/// lists, rather than answered as every row or as a storage failure.
async fn a_negative_limit_is_refused_by_both_prediction_lists<R: MlRepository>(
    r: &R,
    adapter: &str,
) {
    put(r, &model("m-1", "churn", "v1")).await;
    predict(r, input("p-1", "m-1", "account", "a-1")).await;
    match r.predictions_for_entity("account", "a-1", -1).await {
        Err(MlError::BadRequest(msg)) => {
            assert_eq!(msg, "limit must not be negative, got -1", "{adapter}")
        }
        other => panic!("{adapter}: for entity, a negative limit answered {other:?}"),
    }
    match r.recent_predictions_for_model("m-1", -1).await {
        Err(MlError::BadRequest(msg)) => {
            assert_eq!(msg, "limit must not be negative, got -1", "{adapter}")
        }
        other => panic!("{adapter}: for model, a negative limit answered {other:?}"),
    }
}
