//! `/api/jobs/health` reads the migration ledger it is serving from, so
//! "deployed" and "deployed but not migrated" stop looking alike
//! (design a5323701, backlog 7c298c34).
//!
//! What only a real database can answer: that the adapter reads the
//! table `migrate.sh` writes, that a row missing from it comes back as
//! `pending: 1` naming that file, and that a database with no ledger at
//! all answers `schema: null` — "could not read", never zero — with the
//! health check itself still 200.
//!
//! TestDb loads the schema without `migrate.sh`, so it has no
//! `schema_migrations` table of its own. The test creates it in
//! `migrate.sh`'s shape and fills it the way a full run would — one row
//! per file this build's tree holds — then drops one row.
//!
//! The subject is WHAT the ledger answers, never how fast the gate's
//! PostgreSQL serves it: gate-run reds on 2026-10-21/22 read
//! `pending: null` at the dropped-row assertion on cars that never
//! touched this file, because the production two-second
//! [`READ_TIMEOUT`] expired while a contended database queued the read
//! (f10419f9). So the router here is handed [`ANSWER_BOUND`], and the
//! dropped-row test plants that contention for good: it holds the
//! ledger locked until the read has waited past the production bound.
//! The bound itself is judged where it is the subject, in
//! `schema_level`'s own unit tests.

use std::sync::Arc;

use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::schema_level::{PgSchemaLedger, READ_TIMEOUT, SchemaLedger, expected_migrations};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use serde_json::Value;

/// The bound a test that judges the ledger's ANSWER reads it under —
/// a backstop against a hang, not a claim about the gate's load.
const ANSWER_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

fn app(ledger: Arc<dyn SchemaLedger>) -> axum::Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(FakePolicyClient::builder().build());
    router(JobsApiState {
        schema_ledger: Some(ledger),
        schema_read_timeout: ANSWER_BOUND,
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

/// `migrate.sh`'s ledger, filled as a complete run of this tree fills it.
async fn a_fully_migrated_ledger(db: &TestDb) {
    sqlx::query(
        "CREATE TABLE schema_migrations (
            id TEXT PRIMARY KEY,
            checksum TEXT NOT NULL,
            applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    for id in expected_migrations() {
        sqlx::query("INSERT INTO schema_migrations (id, checksum) VALUES ($1, 'test')")
            .bind(id)
            .execute(&db.pool)
            .await
            .unwrap();
    }
}

async fn schema_on_health(db: &TestDb) -> Value {
    schema_served_by(app(Arc::new(PgSchemaLedger::new(db.pool.clone())))).await
}

async fn schema_served_by(app: axum::Router) -> Value {
    let resp = TestRequest::get("/api/jobs/health").send(&app).await;
    resp.assert_status(StatusCode::OK);
    let body: Value = resp.assert_json();
    body["capabilities"]
        .get("schema")
        .cloned()
        .unwrap_or_else(|| panic!("health carries no schema field at all: {body}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fully_migrated_database_reads_pending_zero_at_this_trees_head() {
    let db = TestDb::new().await;
    a_fully_migrated_ledger(&db).await;
    let schema = schema_on_health(&db).await;
    let last = expected_migrations().last().copied().unwrap();
    assert_eq!(schema["head"], last, "{schema}");
    assert_eq!(schema["pending"], 0, "{schema}");
    assert!(schema["first_pending"].is_null(), "{schema}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_ledger_row_reads_pending_one_naming_that_file() {
    let db = TestDb::new().await;
    a_fully_migrated_ledger(&db).await;
    // A middle entry, so the head is unmoved and only the verdict can
    // tell: the case a `commit` comparison reads as converged.
    let dropped = expected_migrations()[expected_migrations().len() / 2];
    sqlx::query("DELETE FROM schema_migrations WHERE id = $1")
        .bind(dropped)
        .execute(&db.pool)
        .await
        .unwrap();

    // The gate's contention, planted: the ledger is locked, the health
    // read is seen queued behind the lock, and the lock outlives the
    // production bound before it is released. The answer must still be
    // the ledger's.
    let mut holder = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE schema_migrations IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let health = tokio::spawn(schema_served_by(app(Arc::new(PgSchemaLedger::new(
        db.pool.clone(),
    )))));
    tokio::time::timeout(ANSWER_BOUND, async {
        loop {
            let queued: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname = current_database() \
                 AND wait_event_type = 'Lock' AND query LIKE 'SELECT id FROM schema_migrations%')",
            )
            .fetch_one(&db.pool)
            .await
            .unwrap();
            if queued {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the health read never queued behind the ledger lock");
    tokio::time::sleep(READ_TIMEOUT + std::time::Duration::from_millis(500)).await;
    holder.commit().await.unwrap();

    let schema = health.await.unwrap();
    assert_eq!(schema["pending"], 1, "{schema}");
    assert_eq!(schema["first_pending"], dropped, "{schema}");
    assert_eq!(
        schema["head"],
        expected_migrations().last().copied().unwrap(),
        "{schema}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_with_no_ledger_reads_null_never_zero() {
    let db = TestDb::new().await;
    let schema = schema_on_health(&db).await;
    assert!(
        schema.is_null(),
        "an unreadable ledger must answer null, not a count: {schema}"
    );
}
