//! End-to-end test for the people → audit_log chain, on the REAL
//! pipeline (outbox phase 2): the POST records the event on the
//! transactional outbox inside the adapter's tx; the relay drain
//! moves it to audit_log. Deliberately NO direct audit writer.

mod common;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_people::PgPeople;
use boss_people::http::{PeopleApiState, router};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use common::employee_fixture;
use sqlx::PgPool;

fn build_app(pool: PgPool) -> Router {
    // No publisher: the handler stamp falls back to source="people"
    // and the event records on the outbox.
    router(PeopleApiState {
        people: Arc::new(PgPeople::new(pool)),
        publisher: None,
        policy: None,
        subject_kinds: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn post_employee_lands_in_audit_log() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    let emp = employee_fixture("emp-audit-test");
    TestRequest::post("/api/people")
        .json(&emp)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Drain the outbox through the relay pipeline: outbox →
    // audit_log (chained) → bus → delivered.
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
    assert_eq!(stats.delivered, 1, "the create arrives via the outbox");

    let row: (String, String) = sqlx::query_as(
        "SELECT source, kind FROM audit_log \
         WHERE kind = 'people.employee.created' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST + drain");

    assert_eq!(row.0, "people");
    assert_eq!(row.1, "people.employee.created");
}
