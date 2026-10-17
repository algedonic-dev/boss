//! Causal prerequisite for guarded gate records: a later state for one Job
//! cannot reach the audit log before an earlier locked state of that Job.
use std::sync::Arc;

use boss_core::{event::Event, port::EventBus};
use boss_events::outbox::{drain_outbox_once, record_event_in_tx};
use boss_testing::{RecordingEventBus, TestDb};
use serde_json::json;
use uuid::Uuid;

async fn observed_lock_wait(
    pool: &sqlx::PgPool,
    relay: &tokio::task::JoinHandle<Result<boss_events::outbox::DrainStats, String>>,
) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%FROM event_outbox%' AND query LIKE '%FOR UPDATE%')",
        ).fetch_one(pool).await.unwrap();
        if waiting {
            return true;
        }
        if relay.is_finished() || tokio::time::Instant::now() >= deadline {
            let activity: Vec<(i32, Option<String>, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
                "SELECT pid, state, wait_event_type, wait_event, query FROM pg_stat_activity WHERE datname=current_database() ORDER BY pid",
            ).fetch_all(pool).await.unwrap();
            eprintln!(
                "lock observation failed: relay_finished={}, runtime_parallelism={:?}, complete database activity={activity:?}",
                relay.is_finished(),
                std::thread::available_parallelism()
            );
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_real_relay_preserves_one_jobs_committed_state_order() {
    let db = TestDb::new().await;
    let job = Uuid::new_v4();
    let first = Event {
        id: Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        source: "queue-order-prerequisite".into(),
        kind: "jobs.job.updated".into(),
        payload: json!({"id":job,"metadata":{"state":"first"}}),
    };
    let second = Event {
        id: Uuid::new_v4(),
        payload: json!({"id":job,"metadata":{"state":"second"}}),
        ..first.clone()
    };
    for event in [&first, &second] {
        let mut tx = db.pool.begin().await.unwrap();
        record_event_in_tx(&mut tx, event).await.unwrap();
        tx.commit().await.unwrap();
    }
    let mut held = db.pool.begin().await.unwrap();
    let held_id: i64 =
        sqlx::query_scalar("SELECT id FROM event_outbox WHERE event_id=$1 FOR UPDATE")
            .bind(first.id)
            .fetch_one(&mut *held)
            .await
            .unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    let relay_pool = db.pool.clone();
    let relay_bus = bus.clone();
    let relay = tokio::spawn(async move { drain_outbox_once(&relay_pool, &relay_bus, 100).await });
    let lock_wait = observed_lock_wait(&db.pool, &relay).await;
    let premature: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    held.rollback().await.unwrap();
    let while_held = tokio::time::timeout(std::time::Duration::from_secs(5), relay)
        .await
        .expect("released row must unblock relay")
        .unwrap()
        .unwrap();
    let after_release = drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let originals: Vec<(i64, Uuid, String, serde_json::Value)> =
        sqlx::query_as("SELECT id,event_id,kind,payload FROM audit_log ORDER BY id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    eprintln!(
        "held original outbox row={held_id}; while_held={while_held:?}; after_release={after_release:?}; complete audit originals={originals:?}"
    );
    assert_eq!(originals.len(), 2);
    assert!(
        lock_wait,
        "the actual relay query must be observed waiting for the row lock"
    );
    assert_eq!(
        premature, 0,
        "later facts cannot overtake a locked predecessor"
    );
    assert_eq!(
        originals.iter().map(|row| row.1).collect::<Vec<_>>(),
        vec![first.id, second.id],
        "same-Job causal replay needs original committed order; a single inline receipt alone cannot repair reversal"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canceling_a_blocked_relay_keeps_both_facts_for_a_fresh_relay() {
    let db = TestDb::new().await;
    let mut ids = Vec::new();
    for state in ["first", "second"] {
        let event = Event {
            id: Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            source: "queue-order-cancellation".into(),
            kind: "outbox.test.ordered-cancellation".into(),
            payload: json!({"state":state}),
        };
        let mut tx = db.pool.begin().await.unwrap();
        record_event_in_tx(&mut tx, &event).await.unwrap();
        tx.commit().await.unwrap();
        ids.push(event.id);
    }
    let mut held = db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM event_outbox WHERE event_id=$1 FOR UPDATE")
        .bind(ids[0])
        .execute(&mut *held)
        .await
        .unwrap();
    let pool = db.pool.clone();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    let child_bus = bus.clone();
    let relay = tokio::spawn(async move { drain_outbox_once(&pool, &child_bus, 100).await });
    let lock_wait = observed_lock_wait(&db.pool, &relay).await;
    let unexpectedly_finished = relay.is_finished();
    relay.abort();
    let ending = relay.await;
    held.rollback().await.unwrap();
    let final_drain = drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let originals: Vec<Uuid> = sqlx::query_scalar("SELECT event_id FROM audit_log ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    eprintln!(
        "canceled relay={ending:?}; prematurely_finished={unexpectedly_finished}; final={final_drain:?}; originals={originals:?}"
    );
    assert!(
        !unexpectedly_finished,
        "the relay must wait for its locked predecessor"
    );
    assert!(
        lock_wait,
        "cancellation must interrupt an observed database lock wait"
    );
    assert!(ending.unwrap_err().is_cancelled());
    assert_eq!(originals, ids);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_real_relays_conserve_one_audit_identity_per_fact_in_order() {
    let db = TestDb::new().await;
    let mut ids = Vec::new();
    for n in 0..2 {
        let event = Event {
            id: Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            source: "queue-order-concurrency".into(),
            kind: "outbox.test.ordered-concurrency".into(),
            payload: json!({"n":n}),
        };
        let mut tx = db.pool.begin().await.unwrap();
        record_event_in_tx(&mut tx, &event).await.unwrap();
        tx.commit().await.unwrap();
        ids.push(event.id);
    }
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    let (a, b) = tokio::join!(
        drain_outbox_once(&db.pool, &bus, 1),
        drain_outbox_once(&db.pool, &bus, 1),
    );
    let final_drain = drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let originals: Vec<Uuid> = sqlx::query_scalar("SELECT event_id FROM audit_log ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    eprintln!("concurrent real drains={a:?}/{b:?}; final={final_drain:?}; originals={originals:?}");
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(originals, ids);
    assert_eq!(
        boss_events::outbox::pending_count(&db.pool).await.unwrap(),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_prolonged_claim_lock_returns_a_visible_error_without_losing_the_fact() {
    let db = TestDb::new().await;
    let event = Event {
        id: Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        source: "queue-order-timeout".into(),
        kind: "outbox.test.claim-timeout".into(),
        payload: json!({"retained":true}),
    };
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &event).await.unwrap();
    tx.commit().await.unwrap();
    let mut held = db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM event_outbox WHERE event_id=$1 FOR UPDATE")
        .bind(event.id)
        .execute(&mut *held)
        .await
        .unwrap();
    let pool = db.pool.clone();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    let child_bus = bus.clone();
    let mut relay = tokio::spawn(async move { drain_outbox_once(&pool, &child_bus, 100).await });
    assert!(observed_lock_wait(&db.pool, &relay).await);
    let ending = tokio::time::timeout(std::time::Duration::from_secs(3), &mut relay).await;
    let visible_error = matches!(&ending,Ok(Ok(Err(error))) if error.contains("lock timeout"));
    eprintln!("bounded claim outcome={ending:?}");
    if ending.is_err() {
        relay.abort();
        let _ = relay.await;
    }
    held.rollback().await.unwrap();
    let pending = boss_events::outbox::pending_count(&db.pool).await.unwrap();
    let fresh = drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let originals: Vec<Uuid> = sqlx::query_scalar("SELECT event_id FROM audit_log ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert!(
        visible_error,
        "a prolonged lock must return a diagnostic within the bounded claim window"
    );
    assert_eq!(pending, 1);
    assert_eq!(fresh.delivered, 1);
    assert_eq!(originals, vec![event.id]);
}
