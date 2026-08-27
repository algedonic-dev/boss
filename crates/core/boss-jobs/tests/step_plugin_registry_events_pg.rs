//! Postgres-backed coverage for the step-plugin registry event
//! contract (protocol-policy-publish.md, Constraints): every
//! registry write records an outbox event in the SAME transaction
//! as the step_plugins row. The InMemory adapter's matching
//! contract is pinned in `step_plugins::tests` (lib test); this
//! file proves the Pg adapter actually stages the rows in
//! `event_outbox`.

use boss_core::actor::ActorId;
use boss_jobs::events::{STEP_PLUGIN_DRAFT_SAVED, STEP_PLUGIN_PUBLISHED, STEP_PLUGIN_RETIRED};
use boss_jobs::{PgStepPlugins, StepPluginRegistry, StepPluginSpec};
use boss_testing::TestDb;

fn spec(kind: &str) -> StepPluginSpec {
    StepPluginSpec::draft(
        kind,
        format!("Test {kind}"),
        "qa",
        format!("{kind}.js"),
        serde_json::json!({}),
    )
}

fn author() -> ActorId {
    ActorId::Human("emp-cto".into())
}

async fn outbox_kinds(db: &TestDb) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM event_outbox ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .expect("read outbox kinds")
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_draft_and_publish_stage_their_events_in_the_outbox() {
    let db = TestDb::new().await;
    let registry = PgStepPlugins::new(db.pool.clone());
    let now = chrono::Utc::now();

    registry
        .create_draft(spec("emerald-inspection"), &author(), now)
        .await
        .expect("draft");
    registry
        .publish("emerald-inspection", &author(), now)
        .await
        .expect("publish");

    assert_eq!(
        outbox_kinds(&db).await,
        vec![
            STEP_PLUGIN_DRAFT_SAVED.to_string(),
            STEP_PLUGIN_PUBLISHED.to_string()
        ],
        "each registry write stages exactly one outbox row, in write order"
    );

    // The published payload is the promoted spec, with the actor
    // riding as `_actor` in EventStamp's exact shape.
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1")
            .bind(STEP_PLUGIN_PUBLISHED)
            .fetch_one(&db.pool)
            .await
            .expect("published payload");
    assert_eq!(payload["kind"], "emerald-inspection");
    assert_eq!(payload["version"], 1);
    assert_eq!(payload["status"], "active");
    assert_eq!(payload["_actor"], "emp-cto");
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_retire_stages_once_and_stays_silent_when_already_retired() {
    let db = TestDb::new().await;
    let registry = PgStepPlugins::new(db.pool.clone());
    let now = chrono::Utc::now();

    registry
        .create_draft(spec("emerald-inspection"), &author(), now)
        .await
        .expect("draft");
    registry
        .publish("emerald-inspection", &author(), now)
        .await
        .expect("publish");

    registry
        .retire("emerald-inspection", &author(), now)
        .await
        .expect("retire");
    // Idempotent second retire touches no row → records no event
    // (the rows_affected > 0 means event discipline).
    registry
        .retire("emerald-inspection", &author(), now)
        .await
        .expect("repeat retire");

    let retired: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind = $1")
        .bind(STEP_PLUGIN_RETIRED)
        .fetch_one(&db.pool)
        .await
        .expect("count retired");
    assert_eq!(retired, 1, "the no-op retire must not stage a second event");
}
