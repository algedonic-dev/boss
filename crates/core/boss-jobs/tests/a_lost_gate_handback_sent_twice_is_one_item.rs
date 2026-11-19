//! A lost-gate hand-back sent twice is one item — over the in-memory
//! store (backlog 7cedd72f). The scenarios and why they exist are in
//! `lost_gate_handback/mod.rs`; the Postgres half is
//! `a_lost_gate_handback_sent_twice_is_one_item_pg.rs`.

mod lost_gate_handback;

use std::sync::Arc;

use async_trait::async_trait;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use lost_gate_handback::{Ledger, PERSON, app, backlog_item};

struct Memory(Arc<InMemoryJobs>);

#[async_trait]
impl Ledger for Memory {
    /// The in-memory store records only this test's packet.
    async fn events_about(&self, _id: &str) -> usize {
        self.0.recorded_events().len()
    }
}

fn harness() -> (axum::Router, Memory) {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(backlog_item()).expect("seed backlog-item");
    let jobs = Arc::new(InMemoryJobs::new());
    let app = app(jobs.clone(), kinds as Arc<dyn WorkflowRegistry>);
    (app, Memory(jobs))
}

#[tokio::test]
async fn the_handback_sent_twice_names_one_item() {
    let (app, ledger) = harness();
    lost_gate_handback::the_handback_sent_twice_is_one_item(&app, &ledger, PERSON).await;
}

/// `owner_for_filing` answers NOBODY when the platform-owner read is
/// refused; admission then resolves the owner, so the stored owner is
/// not the sent one — and the re-send must still be the same item.
#[tokio::test]
async fn the_handback_filed_with_no_owner_sent_twice_names_one_item() {
    let (app, ledger) = harness();
    lost_gate_handback::the_handback_sent_twice_is_one_item(
        &app,
        &ledger,
        boss_core::platform_owner::NOBODY,
    )
    .await;
}

#[tokio::test]
async fn a_moved_reading_is_refused_and_read_back_as_the_first() {
    let (app, ledger) = harness();
    lost_gate_handback::a_moved_reading_is_refused_and_the_read_back_recovers_the_item(
        &app, &ledger,
    )
    .await;
}
