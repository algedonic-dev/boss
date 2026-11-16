//! A lost-gate hand-back sent twice is one item — over Postgres
//! (backlog 7cedd72f). The same scenarios as the in-memory half, on the
//! adapter production runs: the packet is read back from the row the
//! first send wrote, so a field the store answers differently from the
//! body (a JSONB round-trip, a server-written owner) is compared the way
//! the live jobs API compares it. The scenarios and why they exist are in
//! `lost_gate_handback/mod.rs`.

mod lost_gate_handback;

use std::sync::Arc;

use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_jobs::PgJobs;
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry, WorkflowStatus};
use boss_testing::TestDb;
use chrono::Utc;
use lost_gate_handback::{Ledger, PERSON, app, backlog_item};
use sqlx::PgPool;

struct Outbox(PgPool);

#[async_trait]
impl Ledger for Outbox {
    /// Every event about the packet names it: `jobs.job.created` carries
    /// `id`, each `jobs.step.created` carries `job_id`.
    async fn events_about(&self, id: &str) -> usize {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM event_outbox \
              WHERE payload->>'id' = $1 OR payload->>'job_id' = $1",
        )
        .bind(id)
        .fetch_one(&self.0)
        .await
        .expect("count the outbox");
        usize::try_from(n).expect("a count is not negative")
    }
}

/// The router over a fresh database with the platform bundle's
/// `backlog-item` row published, unaltered.
async fn harness(db: &TestDb) -> (axum::Router, Outbox) {
    let author = ActorId::Human(PERSON.into());
    let kinds = PgWorkflows::for_fixture(db.pool.clone());
    let mut spec = backlog_item();
    spec.status = WorkflowStatus::Draft;
    kinds
        .create_draft(spec, &author, Utc::now())
        .await
        .expect("backlog-item drafts");
    kinds
        .publish("backlog-item", &author, Utc::now())
        .await
        .expect("backlog-item publishes");
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    let app = app(jobs, Arc::new(kinds) as Arc<dyn WorkflowRegistry>);
    (app, Outbox(db.pool.clone()))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_handback_sent_twice_names_one_item() {
    let db = TestDb::new().await;
    let (app, ledger) = harness(&db).await;
    lost_gate_handback::the_handback_sent_twice_is_one_item(&app, &ledger, PERSON).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_handback_filed_with_no_owner_sent_twice_names_one_item() {
    let db = TestDb::new().await;
    let (app, ledger) = harness(&db).await;
    lost_gate_handback::the_handback_sent_twice_is_one_item(
        &app,
        &ledger,
        boss_core::platform_owner::NOBODY,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_moved_reading_is_refused_and_read_back_as_the_first() {
    let db = TestDb::new().await;
    let (app, ledger) = harness(&db).await;
    lost_gate_handback::a_moved_reading_is_refused_and_the_read_back_recovers_the_item(
        &app, &ledger,
    )
    .await;
}
