//! Outbox retention — a delivered row is deleted once it is older than
//! the retention window, and nothing else ever is (backlog eec0c1f3,
//! incident d3c0a67c).
//!
//! Until this car nothing deleted an `event_outbox` row: the relay only
//! stamped `delivered_at`, so every event was stored twice for good —
//! once in the outbox and once in `audit_log`, the system of record.
//! Measured 2026-10-01: the outbox's delivered rows over 720 h were
//! 1,030,520, exactly `audit_log`'s row count, on the day the SoR
//! Postgres volume filled.
//!
//! Contract under test (`boss_events::outbox::prune_delivered_outbox`):
//! - an UNDELIVERED row survives at any age — pending or dead-lettered,
//!   even when its audit row already exists;
//! - a DELIVERED row older than the window is deleted, its audit_log
//!   row stays, and the deletion leaves a fact naming how many rows went;
//! - a DELIVERED row inside the window survives;
//! - a "delivered" row whose event is NOT in audit_log survives — the
//!   copy is only deleted when the system of record is proven to hold it;
//! - a delivered row whose ID audit_log holds under a DIFFERENT fact
//!   survives — the proof is the fact, not the id (review afdc2d5d, N1);
//! - a pass works in bounded batches and stops at its row cap.

use std::sync::Arc;
use std::time::Duration;

use boss_core::event::Event;
use boss_core::port::EventBus;
use boss_events::outbox::{
    PRUNED_KIND, drain_outbox_once, prune_delivered_outbox, record_event_in_tx,
};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::Utc;
use uuid::Uuid;

const WEEK: Duration = Duration::from_secs(7 * 24 * 3600);

/// Stage `n` events and drain them through the relay, so each has its
/// audit_log row and a `delivered_at` — the shape every live row has.
/// Returns the outbox ids in staging order.
async fn stage_and_deliver(db: &TestDb, n: usize) -> Vec<i64> {
    let mut ids = Vec::new();
    for _ in 0..n {
        let e = Event {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source: "outbox-retention-test".to_string(),
            kind: "outbox.test.retained".to_string(),
            payload: serde_json::json!({"n": 1}),
        };
        let mut tx = db.pool.begin().await.unwrap();
        record_event_in_tx(&mut tx, &e).await.expect("record");
        tx.commit().await.unwrap();
        let id: i64 = sqlx::query_scalar("SELECT id FROM event_outbox WHERE event_id = $1")
            .bind(e.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        ids.push(id);
    }
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 1000)
        .await
        .expect("drain");
    assert_eq!(stats.delivered, n as u64, "every staged row delivered");
    ids
}

/// Move a row `days` into the past: staged then, delivered a second
/// later — `delivered_at` is never before `created_at` on a live row.
async fn age(db: &TestDb, id: i64, days: i64) {
    sqlx::query(
        "UPDATE event_outbox \
            SET created_at = NOW() - make_interval(days => $2::int), \
                delivered_at = CASE WHEN delivered_at IS NULL THEN NULL \
                    ELSE NOW() - make_interval(days => $2::int) + interval '1 second' END \
          WHERE id = $1",
    )
    .bind(id)
    .bind(days as i32)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn exists(db: &TestDb, id: i64) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM event_outbox WHERE id = $1)")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

/// The prune facts staged so far, oldest first, as their payloads.
async fn pruned_facts(db: &TestDb) -> Vec<serde_json::Value> {
    sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY id")
        .bind(PRUNED_KIND)
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_undelivered_row_of_any_age_survives() {
    let db = TestDb::new().await;
    // Both rows were drained first, so each HAS its audit_log row: the
    // only thing keeping them is that neither is delivered.
    let ids = stage_and_deliver(&db, 2).await;
    let (pending, dead) = (ids[0], ids[1]);
    sqlx::query("UPDATE event_outbox SET delivered_at = NULL WHERE id = $1")
        .bind(pending)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE event_outbox SET delivered_at = NULL, \
                dead_lettered_at = NOW() - interval '365 days', \
                dead_letter_reason = 'maximum payload exceeded' \
          WHERE id = $1",
    )
    .bind(dead)
    .execute(&db.pool)
    .await
    .unwrap();
    age(&db, pending, 365).await;
    age(&db, dead, 365).await;

    let stats = prune_delivered_outbox(&db.pool, WEEK, 1000, 100_000)
        .await
        .expect("prune");

    assert_eq!(stats.deleted, 0, "nothing undelivered is ever pruned");
    assert!(
        exists(&db, pending).await,
        "a pending row a year old survives"
    );
    assert!(
        exists(&db, dead).await,
        "a dead-lettered row a year old survives"
    );
    assert!(pruned_facts(&db).await.is_empty(), "no deletion, no fact");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delivered_row_past_the_age_is_deleted() {
    let db = TestDb::new().await;
    let ids = stage_and_deliver(&db, 1).await;
    let event_id: Uuid = sqlx::query_scalar("SELECT event_id FROM event_outbox WHERE id = $1")
        .bind(ids[0])
        .fetch_one(&db.pool)
        .await
        .unwrap();
    age(&db, ids[0], 8).await;

    let stats = prune_delivered_outbox(&db.pool, WEEK, 1000, 100_000)
        .await
        .expect("prune");

    assert_eq!(stats.deleted, 1);
    assert!(!exists(&db, ids[0]).await, "the delivered copy is gone");
    let in_log: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM audit_log WHERE event_id = $1)")
            .bind(event_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(in_log, "the system of record still holds the fact");

    // The deletion is on the record, staged with it.
    let facts = pruned_facts(&db).await;
    assert_eq!(facts.len(), 1, "one batch, one fact: {facts:?}");
    assert_eq!(facts[0]["deleted"], 1);
    assert_eq!(facts[0]["first_outbox_id"], ids[0]);
    assert_eq!(facts[0]["last_outbox_id"], ids[0]);
    assert_eq!(facts[0]["retention_hours"], 168);
    assert!(
        facts[0]["delivered_before"].is_string(),
        "the cutoff is named: {}",
        facts[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delivered_row_inside_the_age_survives() {
    let db = TestDb::new().await;
    let ids = stage_and_deliver(&db, 3).await;
    // A redelivered dead letter: staged a month ago, set aside, put back
    // and delivered yesterday. Its age is its DELIVERY's age, and it
    // sits below every younger row in id order, where no staging-order
    // bound can shelter it.
    age(&db, ids[0], 30).await;
    sqlx::query("UPDATE event_outbox SET delivered_at = NOW() - interval '1 day' WHERE id = $1")
        .bind(ids[0])
        .execute(&db.pool)
        .await
        .unwrap();
    age(&db, ids[1], 6).await;
    // ids[2] was delivered just now.

    let stats = prune_delivered_outbox(&db.pool, WEEK, 1000, 100_000)
        .await
        .expect("prune");

    assert_eq!(stats.deleted, 0);
    assert!(
        exists(&db, ids[0]).await,
        "delivered yesterday, however old its staging"
    );
    assert!(exists(&db, ids[1]).await, "six days old is inside a week");
    assert!(exists(&db, ids[2]).await, "delivered a moment ago");
    assert!(pruned_facts(&db).await.is_empty());
}

/// Review afdc2d5d, finding N1, machine-run by the reviewer as probe (c).
/// The relay's audit insert is `WHERE NOT EXISTS (… event_id = $1)`, so
/// a second, DIFFERENT event staged under an id audit_log already holds
/// is skipped without a word and then stamped delivered. While the
/// outbox kept every row, its UNIQUE(event_id) refused that second
/// staging; once retention has pruned the first copy it no longer can.
/// Unreachable today — every live id is a fresh v4 — but the deletion
/// must not depend on that: it deletes a copy only when audit_log holds
/// the same FACT, not merely the same id.
#[tokio::test(flavor = "multi_thread")]
async fn a_delivered_row_whose_id_audit_log_holds_under_another_fact_survives() {
    let db = TestDb::new().await;
    let reused = Uuid::new_v4();
    let stage = async |kind: &str, n: i64| {
        let e = Event {
            id: reused,
            timestamp: Utc::now(),
            source: "outbox-retention-test".to_string(),
            kind: kind.to_string(),
            payload: serde_json::json!({"n": n}),
        };
        let mut tx = db.pool.begin().await.unwrap();
        record_event_in_tx(&mut tx, &e).await.expect("record");
        tx.commit().await.unwrap();
        let id: i64 = sqlx::query_scalar("SELECT id FROM event_outbox WHERE event_id = $1")
            .bind(reused)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let bus = RecordingEventBus::new();
        let stats = drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 1000)
            .await
            .expect("drain");
        (id, stats.audit_inserted)
    };

    // Fact A under the id: relayed, aged out, pruned.
    let (a, inserted) = stage("outbox.test.first-fact", 1).await;
    assert_eq!(inserted, 1);
    age(&db, a, 8).await;
    let first = prune_delivered_outbox(&db.pool, WEEK, 1000, 100_000)
        .await
        .expect("prune");
    assert_eq!(first.deleted, 1, "A's copy is in audit_log, so it goes");

    // Fact B under the SAME id: the outbox accepts it (A's row is gone),
    // and the relay's NOT EXISTS skips its audit insert.
    // (The drain also relays the pass's own pruned fact, so its insert
    // count is not B's; read the log for B instead.)
    let (b, _) = stage("outbox.test.second-fact", 2).await;
    let b_in_log: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE kind = 'outbox.test.second-fact'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        b_in_log, 0,
        "the relay skipped B's audit insert — the hazard"
    );
    let b_delivered: bool =
        sqlx::query_scalar("SELECT delivered_at IS NOT NULL FROM event_outbox WHERE id = $1")
            .bind(b)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(b_delivered, "and stamped B delivered anyway");
    // Age every row, the first pass's own pruned fact included: a young
    // row staged before B would bound the walk below B and shelter it
    // for the wrong reason.
    let all: Vec<i64> = sqlx::query_scalar("SELECT id FROM event_outbox")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    for id in all {
        age(&db, id, 8).await;
    }

    let second = prune_delivered_outbox(&db.pool, WEEK, 1000, 100_000)
        .await
        .expect("prune");
    assert_eq!(
        second.deleted, 1,
        "only the first pass's pruned fact goes: audit_log holds the id, not B's fact"
    );
    assert!(exists(&db, b).await, "B's outbox row is the only copy of B");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delivered_row_whose_event_is_not_in_audit_log_survives() {
    let db = TestDb::new().await;
    // A row stamped delivered without ever being drained — no path in
    // the tree writes one, and if any ever did, its outbox copy would be
    // the only copy there is.
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO event_outbox (event_id, timestamp, source, kind, payload, created_at, delivered_at) \
         VALUES ($1, NOW(), 'outbox-retention-test', 'outbox.test.orphan', '{}', \
                 NOW() - interval '30 days', NOW() - interval '30 days') \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let stats = prune_delivered_outbox(&db.pool, WEEK, 1000, 100_000)
        .await
        .expect("prune");

    assert_eq!(stats.deleted, 0);
    assert!(
        exists(&db, id).await,
        "only a copy audit_log holds is deleted"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pass_deletes_in_batches_and_stops_at_its_cap() {
    let db = TestDb::new().await;
    let ids = stage_and_deliver(&db, 5).await;
    for id in &ids {
        age(&db, *id, 10).await;
    }

    // Batches of two, at most four rows this pass.
    let first = prune_delivered_outbox(&db.pool, WEEK, 2, 4)
        .await
        .expect("prune");
    assert_eq!(first.deleted, 4);
    assert_eq!(first.batches, 2);
    let facts = pruned_facts(&db).await;
    assert_eq!(facts.len(), 2, "one fact per batch: {facts:?}");
    assert!(facts.iter().all(|f| f["deleted"] == 2));
    assert!(
        exists(&db, ids[4]).await,
        "the cap left the last row for the next pass"
    );

    let second = prune_delivered_outbox(&db.pool, WEEK, 2, 4)
        .await
        .expect("prune");
    assert_eq!(second.deleted, 1, "the next pass takes what the cap left");
    assert!(!exists(&db, ids[4]).await);
    // The facts the passes staged are pending, so no pass deletes them.
    assert_eq!(pruned_facts(&db).await.len(), 3);
}

/// The registry says what retention did to a neighbouring field (review
/// afdc2d5d, N3): `overtaken_by` on `events.outbox.redelivered` is a
/// lower bound once delivered rows past the window are deleted, and the
/// kind this car emits is declared.
#[tokio::test(flavor = "multi_thread")]
async fn the_registry_declares_the_pruned_kind_and_the_lower_bound() {
    let db = TestDb::new().await;
    let declared: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM event_kinds WHERE kind_pattern = $1)")
            .bind(PRUNED_KIND)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(declared, "{PRUNED_KIND} is declared");
    let note: String = sqlx::query_scalar(
        "SELECT f->>'note' FROM event_kinds, jsonb_array_elements(payload_fields) f \
          WHERE kind_pattern = 'events.outbox.redelivered' AND f->>'name' = 'overtaken_by'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(note.contains("LOWER bound"), "overtaken_by note: {note}");
}
