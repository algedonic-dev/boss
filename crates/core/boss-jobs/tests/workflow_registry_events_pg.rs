//! Postgres-backed coverage for the workflow-registry event
//! contract (protocol-policy-publish.md, Constraints): every
//! registry write records an outbox event in the SAME transaction
//! as the workflows row. The InMemory adapter's matching contract
//! is pinned in `registry::tests` (lib test); this file proves the
//! Pg adapter actually stages the rows in `event_outbox`.

use boss_core::actor::ActorId;
use boss_jobs::events::{WORKFLOW_DRAFT_SAVED, WORKFLOW_PUBLISHED, WORKFLOW_RETIRED};
use boss_jobs::registry::{PgWorkflows, StepSpec, Terminal, WorkflowRegistry, WorkflowSpec};
use boss_testing::TestDb;

/// Minimal VIABLE spec — publish runs the viability gate, so a
/// step-less fixture would be refused before any event was staged.
fn spec(kind: &str, label: &str) -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        kind,
        label,
        "platform",
        vec!["account".into()],
        vec![
            StepSpec {
                title: "start".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                ..Default::default()
            },
            StepSpec {
                title: "finish".into(),
                kind: "task".into(),
                ready_when: "steps.start.done".into(),
                terminal: Some(Terminal {
                    outcome: "done".into(),
                }),
                ..Default::default()
            },
        ],
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
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    let now = chrono::Utc::now();

    registry
        .create_draft(spec("morning-brew", "Morning Brew"), &author(), now)
        .await
        .expect("draft");
    registry
        .publish("morning-brew", &author(), now)
        .await
        .expect("publish");

    assert_eq!(
        outbox_kinds(&db).await,
        vec![
            WORKFLOW_DRAFT_SAVED.to_string(),
            WORKFLOW_PUBLISHED.to_string()
        ],
        "each registry write stages exactly one outbox row, in write order"
    );

    // The published payload is the promoted spec, with the actor
    // riding as `_actor` in EventStamp's exact shape.
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1")
            .bind(WORKFLOW_PUBLISHED)
            .fetch_one(&db.pool)
            .await
            .expect("published payload");
    assert_eq!(payload["kind"], "morning-brew");
    assert_eq!(payload["version"], 1);
    assert_eq!(payload["status"], "active");
    assert_eq!(payload["_actor"], "emp-cto");
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_retire_stages_once_and_stays_silent_when_already_retired() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    let now = chrono::Utc::now();

    registry
        .create_draft(spec("morning-brew", "Morning Brew"), &author(), now)
        .await
        .expect("draft");
    registry
        .publish("morning-brew", &author(), now)
        .await
        .expect("publish");

    registry
        .retire("morning-brew", &author(), now)
        .await
        .expect("retire");
    // Idempotent second retire touches no row → records no event
    // (the rows_affected > 0 means event discipline).
    registry
        .retire("morning-brew", &author(), now)
        .await
        .expect("repeat retire");

    let retired: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind = $1")
        .bind(WORKFLOW_RETIRED)
        .fetch_one(&db.pool)
        .await
        .expect("count retired");
    assert_eq!(retired, 1, "the no-op retire must not stage a second event");
}

#[tokio::test(flavor = "multi_thread")]
async fn conditional_publication_rolls_back_its_row_when_the_outbox_refuses() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    sqlx::query(
        "CREATE FUNCTION refuse_conditional_event() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN IF NEW.payload->>'kind' = 'conditional-outbox-refusal' THEN
           RAISE EXCEPTION 'causal conditional outbox refusal'; END IF; RETURN NEW; END $$",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse_conditional_event BEFORE INSERT ON event_outbox
         FOR EACH ROW EXECUTE FUNCTION refuse_conditional_event()",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let result = registry
        .publish_authored_if_absent(
            spec("conditional-outbox-refusal", "Must roll back"),
            boss_core::job::JobId::new(),
            &author(),
            chrono::Utc::now(),
        )
        .await;
    assert!(
        matches!(result, Err(boss_jobs::registry::WorkflowError::Storage(_))),
        "{result:?}"
    );
    assert!(
        registry
            .list_versions("conditional-outbox-refusal")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(outbox_kinds(&db).await.is_empty());
}
