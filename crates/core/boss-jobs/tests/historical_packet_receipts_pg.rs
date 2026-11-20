#![cfg(feature = "postgres")]
use boss_core::{event::Event, job::JobId, port::EventBus};
use boss_events::outbox::{drain_outbox_once, record_event_in_tx};
use boss_jobs::{JobsRepository, PgJobs};
use boss_testing::{RecordingEventBus, TestDb};
use serde_json::json;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn bounded_packet_history_contains_original_nested_states_and_claim_chunks() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let job = JobId::new();
    let other = JobId::new();
    let claim = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    let mut rows = vec![
        Event::new(
            "jobs",
            boss_jobs::events::STATION_SELECTION_EVIDENCE,
            json!({"claim_event_id":claim,"text":"original bytes"}),
            now,
        ),
        Event::new(
            "jobs",
            boss_jobs::events::STATION_CLAIMED,
            json!({"receipt_json":json!({"selected":{"job_id":job}}).to_string()}),
            now,
        ),
        Event::new(
            "jobs",
            boss_jobs::events::STEP_FIRST_RECORDED,
            json!({"step":{"job_id":job}}),
            now,
        ),
        Event::new(
            "jobs",
            boss_jobs::events::STATION_CLAIMED,
            json!({"receipt_json":json!({"selected":{"job_id":other}}).to_string()}),
            now,
        ),
        Event::new(
            "jobs",
            boss_jobs::events::STATION_CLAIMED,
            json!({"receipt_json":"invalid json"}),
            now,
        ),
    ];
    rows[1].id = claim;
    let mut tx = db.pool.begin().await.unwrap();
    for row in &rows {
        record_event_in_tx(&mut tx, row).await.unwrap();
    }
    tx.commit().await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let found = repo.historical_events(&job, i64::MAX, 100).await.unwrap();
    assert_eq!(
        found.iter().map(|r| r.event.id).collect::<Vec<_>>(),
        rows[..3].iter().map(|r| r.id).collect::<Vec<_>>()
    );
    assert!(
        repo.historical_events(&job, i64::MAX, 2).await.is_err(),
        "the complete original receipt cannot be silently truncated"
    );
    let prefix = repo
        .historical_events(&job, found[1].seq, 100)
        .await
        .unwrap();
    assert_eq!(
        prefix.len(),
        2,
        "the receipt lookup must respect the same frozen audit cut"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn old_manifest_only_claim_identity_is_read_from_its_original_chunks() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let job = JobId::new();
    let claim = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    let text = json!({"selected":{"job_id":job}}).to_string();
    let split = text.len() / 2;
    let mut rows = vec![
        Event::new(
            "jobs",
            boss_jobs::events::STATION_SELECTION_EVIDENCE,
            json!({"claim_event_id":claim,"index":0,"text":&text[..split]}),
            now,
        ),
        Event::new(
            "jobs",
            boss_jobs::events::STATION_SELECTION_EVIDENCE,
            json!({"claim_event_id":claim,"index":1,"text":&text[split..]}),
            now,
        ),
        Event::new(
            "jobs",
            boss_jobs::events::STATION_CLAIMED,
            json!({"receipt_manifest":{"version":2}}),
            now,
        ),
    ];
    rows[2].id = claim;
    let mut tx = db.pool.begin().await.unwrap();
    for row in &rows {
        record_event_in_tx(&mut tx, row).await.unwrap();
    }
    tx.commit().await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let found = repo.historical_events(&job, i64::MAX, 100).await.unwrap();
    assert_eq!(
        found.iter().map(|r| r.event.id).collect::<Vec<_>>(),
        rows.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    // Routing finds originals; it must not rewrite their conserved payloads
    // or promote an unvalidated receipt into full state evidence.
    assert_eq!(found[2].event.payload, rows[2].payload);
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_legacy_routing_json_cannot_prevent_the_original_fact_being_conserved() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let event = Event::new(
        "jobs",
        boss_jobs::events::STATION_CLAIMED,
        json!({"receipt_json":"{\"selected\":{\"job_id\":1e1000000}}"}),
        chrono::Utc::now(),
    );
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &event).await.unwrap();
    tx.commit().await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    let report = drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    assert_eq!(
        report.delivered, 1,
        "an index may decline a malformed routing identity, not discard its only original fact"
    );
    assert!(
        repo.historical_events(&JobId::new(), i64::MAX, 100)
            .await
            .unwrap()
            .is_empty()
    );
}
