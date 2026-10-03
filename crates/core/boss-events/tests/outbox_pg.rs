//! Transactional event outbox — the durable hand-off that makes an
//! event's persistence atomic with the state change it describes
//! (docs/design/transactional-audit-log.md, Option B).
//!
//! Contract under test:
//! - `record_event_in_tx` joins the caller's transaction: commit
//!   lands the row, rollback removes every trace, and a ref-check
//!   rejection aborts the caller's write instead of punching a
//!   post-commit provenance hole (the 2026-07-13 incident class).
//! - `drain_outbox_once` is the whole relay pipeline: outbox →
//!   audit_log (chained, id-ordered) → bus → delivered_at, and every
//!   crash point retries idempotently (no duplicate audit rows, no
//!   lost notifications).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use boss_core::actor::ActorId;
use boss_core::event::Event;
use boss_core::port::{EventBus, EventBusError, EventStream};
use boss_events::outbox::{
    MAX_STAGED_EVENT_BYTES, REDELIVERED_KIND, RESOLVED_KIND, dead_lettered_count,
    drain_outbox_once, pending_count, read_back, record_event_in_tx, redeliver_dead_letter,
    relay_lag, resolve_dead_letter, undrained,
};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{TimeZone, Utc};
use uuid::Uuid;

fn event(kind: &str) -> Event {
    Event {
        id: Uuid::new_v4(),
        timestamp: Utc.with_ymd_and_hms(2026, 7, 13, 12, 0, 0).unwrap(),
        source: "outbox-test".to_string(),
        kind: kind.to_string(),
        payload: serde_json::json!({"n": 1}),
    }
}

/// A bus that always fails — the relay's crash-point double.
struct FailingBus {
    attempts: AtomicUsize,
}
#[async_trait::async_trait]
impl EventBus for FailingBus {
    async fn publish(&self, _e: Event) -> Result<(), EventBusError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(EventBusError::PublishFailed("bus down".into()))
    }
    async fn subscribe(&self, _p: &str) -> Result<Box<dyn EventStream>, EventBusError> {
        Err(EventBusError::SubscribeFailed("stub".into()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn commit_then_drain_lands_in_audit_log_bus_and_marks_delivered() {
    let db = TestDb::new().await;
    let e = event("outbox.test.committed");

    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &e).await.expect("record");
    tx.commit().await.unwrap();

    assert_eq!(pending_count(&db.pool).await.unwrap(), 1);

    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .expect("drain");
    assert_eq!(stats.delivered, 1);

    // Audit row exists, chained (the trigger filled the hash pair).
    let (kind, hash_len): (String, i32) =
        sqlx::query_as("SELECT kind, length(row_hash)::int FROM audit_log WHERE event_id = $1")
            .bind(e.id)
            .fetch_one(&db.pool)
            .await
            .expect("audit row");
    assert_eq!(kind, "outbox.test.committed");
    assert_eq!(hash_len, 32);

    // Bus saw it; outbox is drained.
    bus.assert_event_emitted("outbox.test.committed");
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_leaves_no_trace() {
    let db = TestDb::new().await;
    let e = event("outbox.test.rolled-back");

    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &e).await.expect("record");
    tx.rollback().await.unwrap();

    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE event_id = $1")
        .bind(e.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "rollback must remove the outbox row");
}

#[tokio::test(flavor = "multi_thread")]
async fn ref_check_rejection_aborts_the_recording_transaction() {
    // The incident shape: an event referencing an account that does
    // not exist. With the outbox trigger the rejection happens INSIDE
    // the domain tx — the caller gets an Err and aborts, instead of
    // committing state whose provenance is silently dropped later.
    let db = TestDb::new().await;
    sqlx::query(
        "INSERT INTO audit_log_ref_checks (event_kind, field_path, ref_table, ref_column) \
         VALUES ('outbox.test.ref-checked', 'account_id', 'accounts', 'id')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let mut e = event("outbox.test.ref-checked");
    e.payload = serde_json::json!({"account_id": "account-does-not-exist"});

    let mut tx = db.pool.begin().await.unwrap();
    // TestDb disables ref-checks database-wide (test sessions write
    // audit_log without full projections); re-enable for exactly this
    // transaction — SET LOCAL cannot leak through the pool.
    sqlx::query("SET LOCAL audit_log.ref_check = 'on'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let err = record_event_in_tx(&mut tx, &e)
        .await
        .expect_err("missing ref must reject the insert");
    assert!(
        err.contains("non-existent"),
        "error should carry the trigger's message: {err}"
    );
    // The tx is poisoned by the failed statement — rollback and
    // verify nothing landed anywhere.
    tx.rollback().await.unwrap();
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn bus_failure_retries_without_duplicating_the_audit_row() {
    // Crash-point contract: audit INSERT committed, NATS publish
    // failed → the row stays pending. The re-drain must NOT insert a
    // second audit row (idempotent by event_id), must re-publish, and
    // must then mark delivered.
    let db = TestDb::new().await;
    let e = event("outbox.test.retry");

    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &e).await.unwrap();
    tx.commit().await.unwrap();

    let failing = Arc::new(FailingBus {
        attempts: AtomicUsize::new(0),
    });
    let stats = drain_outbox_once(&db.pool, &(failing.clone() as Arc<dyn EventBus>), 100)
        .await
        .expect("drain with failing bus is not an error — the row just stays pending");
    assert_eq!(stats.delivered, 0);
    assert_eq!(failing.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        pending_count(&db.pool).await.unwrap(),
        1,
        "undelivered row stays pending"
    );

    // The audit half already landed (committed before the publish).
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_id = $1")
        .bind(e.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // Retry with a working bus: no duplicate, published, delivered.
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(stats.delivered, 1);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_id = $1")
        .bind(e.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 1, "retry must not duplicate the audit row");
    bus.assert_event_emitted("outbox.test.retry");
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);
}

/// Backlog 72c50b8b: how long a committed write waits before its event
/// reaches audit_log — the exposure window of every audit_log reader,
/// rebuilds included — was unmeasurable. `undrained` is the one query
/// that answers it (and the one design b046f510's rebuild calls inside
/// its lock), so this pins each of its edges: empty, one staged row
/// with its true age, the same answer inside a transaction, a row whose
/// audit half landed (pending, NOT undrained), and delivery.
#[tokio::test(flavor = "multi_thread")]
async fn undrained_counts_a_committed_write_until_its_audit_row_lands() {
    let db = TestDb::new().await;
    let empty = undrained(&db.pool).await.unwrap();
    assert_eq!(empty.count, 0);
    assert_eq!(empty.oldest_created_at, None);
    assert_eq!(empty.behind_head_seconds, None);

    let e = event("outbox.test.undrained");
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &e).await.unwrap();
    tx.commit().await.unwrap();
    let created: chrono::DateTime<Utc> =
        sqlx::query_scalar("SELECT created_at FROM event_outbox WHERE event_id = $1")
            .bind(e.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();

    // One row, and it is the head: dated, and nothing staged after it.
    let one = undrained(&db.pool).await.unwrap();
    assert_eq!(one.count, 1);
    assert_eq!(one.oldest_created_at, Some(created));
    assert_eq!(one.behind_head_seconds, Some(0.0));

    // A later write lands 90 s after it: the log is now 90 s of writes
    // behind, and the oldest undrained row still dates the gap.
    let later = event("outbox.test.undrained.later");
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &later).await.unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        "UPDATE event_outbox SET created_at = $2::timestamptz + INTERVAL '90 seconds' \
         WHERE event_id = $1",
    )
    .bind(later.id)
    .bind(created)
    .execute(&db.pool)
    .await
    .unwrap();
    let two = undrained(&db.pool).await.unwrap();
    assert_eq!(two.count, 2);
    assert_eq!(two.oldest_created_at, Some(created));
    assert_eq!(two.behind_head_seconds, Some(90.0));

    // The rebuild's shape: the same query on a transaction's connection.
    let mut tx = db.pool.begin().await.unwrap();
    assert_eq!(undrained(&mut *tx).await.unwrap(), two);
    tx.rollback().await.unwrap();

    // Audit half lands, publish fails: still PENDING, no longer UNDRAINED
    // — the log now holds the fact, so a rebuild reading it is whole.
    let failing = Arc::new(FailingBus {
        attempts: AtomicUsize::new(0),
    });
    drain_outbox_once(&db.pool, &(failing as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(pending_count(&db.pool).await.unwrap(), 2);
    assert_eq!(undrained(&db.pool).await.unwrap(), empty);

    // Delivered: nothing owed, and each delivery is a lag sample.
    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);
    assert_eq!(undrained(&db.pool).await.unwrap(), empty);
    for (id, secs) in [(e.id, 2), (later.id, 10)] {
        sqlx::query(
            "UPDATE event_outbox SET created_at = delivered_at - make_interval(secs => $2) \
             WHERE event_id = $1",
        )
        .bind(id)
        .bind(f64::from(secs))
        .execute(&db.pool)
        .await
        .unwrap();
    }
    let lag = relay_lag(&db.pool, 24).await.unwrap();
    assert_eq!(lag.window_hours, 24);
    assert_eq!(lag.delivered, 2);
    assert_eq!(lag.p50_seconds, Some(6.0));
    let p95 = lag.p95_seconds.unwrap();
    assert!((p95 - 9.6).abs() < 1e-9, "p95 {p95}");
    assert_eq!(lag.max_seconds, Some(10.0));

    // The window ends at the newest delivery: a delivery 30 h before it
    // falls outside 24 h and inside 48 h.
    sqlx::query(
        "UPDATE event_outbox SET delivered_at = delivered_at - INTERVAL '30 hours', \
                                 created_at = created_at - INTERVAL '30 hours' \
         WHERE event_id = $1",
    )
    .bind(e.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let day = relay_lag(&db.pool, 24).await.unwrap();
    assert_eq!((day.delivered, day.max_seconds), (1, Some(10.0)));
    let two_days = relay_lag(&db.pool, 48).await.unwrap();
    assert_eq!(two_days.delivered, 2);

    // The window MEASURED is what the outbox still holds, not what was
    // asked for (review afdc2d5d, N2): retention prunes delivered rows
    // past a week, so a 720 h request answers a week's sample. Asked
    // for 720 h here, the sample reaches back 30 h and says so.
    let month = relay_lag(&db.pool, 720).await.unwrap();
    assert_eq!(month.window_hours, 720);
    let covered = month.covered_hours.expect("a sample spans some hours");
    assert!((covered - 30.0).abs() < 0.01, "covered {covered}");
    assert!(
        (two_days.covered_hours.unwrap() - 30.0).abs() < 0.01,
        "the same rows, the same span"
    );
}

/// A window with nothing delivered in it answers zero samples and no
/// percentiles — never a zero lag, which would read as an instant relay.
#[tokio::test(flavor = "multi_thread")]
async fn relay_lag_over_an_empty_window_has_no_percentiles() {
    let db = TestDb::new().await;
    let lag = relay_lag(&db.pool, 1).await.unwrap();
    assert_eq!(lag.delivered, 0);
    assert_eq!(lag.p50_seconds, None);
    assert_eq!(lag.p95_seconds, None);
    assert_eq!(lag.max_seconds, None);
    assert_eq!(lag.covered_hours, None);
}

/// A bus that refuses what NATS refuses: an encoded event larger than
/// the server's `max_payload`. The refusal is deterministic — the same
/// row is refused on every attempt — which is what made it a poison
/// pill for a relay that treated every publish error as "retry this
/// row" (backlog e4019cbc). Everything under the bound goes through to
/// the recording bus.
struct MaxPayloadBus {
    max_payload: usize,
    attempts: AtomicUsize,
    inner: Arc<RecordingEventBus>,
}
#[async_trait::async_trait]
impl EventBus for MaxPayloadBus {
    async fn publish(&self, e: Event) -> Result<(), EventBusError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        let size = serde_json::to_vec(&e).unwrap().len();
        if size > self.max_payload {
            return Err(EventBusError::Refused(format!(
                "max payload size exceeded: Payload size limit of {} exceeded by message size of {size}",
                self.max_payload
            )));
        }
        self.inner.publish(e).await
    }
    async fn subscribe(&self, _p: &str) -> Result<Box<dyn EventStream>, EventBusError> {
        Err(EventBusError::SubscribeFailed("stub".into()))
    }
}

async fn stage(db: &TestDb, e: &Event) {
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, e).await.expect("record");
    tx.commit().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_row_is_dead_lettered_and_the_rows_behind_it_are_delivered() {
    // The poison-pill shape (backlog e4019cbc): one row the bus refuses
    // deterministically, then an ordinary row behind it. Before the
    // fix the relay `break`s on the first and the second is never
    // delivered, on this drain or any later one.
    let db = TestDb::new().await;
    let mut oversized = event("outbox.test.oversized");
    oversized.payload = serde_json::json!({"blob": "x".repeat(4096)});
    let after = event("outbox.test.after-the-refusal");
    stage(&db, &oversized).await;
    stage(&db, &after).await;

    let recording = RecordingEventBus::new();
    let bus = Arc::new(MaxPayloadBus {
        max_payload: 1024,
        attempts: AtomicUsize::new(0),
        inner: recording.clone(),
    });
    let stats = drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .expect("a refusal is not a storage error");
    assert_eq!(
        stats.delivered, 1,
        "the row behind the refusal is delivered"
    );
    assert_eq!(stats.dead_lettered, 1);
    recording.assert_event_emitted("outbox.test.after-the-refusal");

    // CONSERVATION: the refused fact is not lost. Its audit row landed
    // in phase 1, and its outbox row keeps the payload, whole, beside
    // the reason the bus gave — never stamped delivered.
    let (delivered, dead_at, reason, blob_len): (
        Option<chrono::DateTime<Utc>>,
        Option<chrono::DateTime<Utc>>,
        Option<String>,
        i32,
    ) = sqlx::query_as(
        "SELECT delivered_at, dead_lettered_at, dead_letter_reason, \
                length(payload->>'blob')::int \
         FROM event_outbox WHERE event_id = $1",
    )
    .bind(oversized.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(delivered.is_none(), "a refused row was never delivered");
    assert!(dead_at.is_some(), "the refused row is dead-lettered");
    let reason = reason.expect("the dead letter carries its reason");
    assert!(
        reason.contains("max payload size exceeded"),
        "reason is the bus's own words: {reason}"
    );
    assert_eq!(blob_len, 4096, "the payload is kept whole");
    let audit: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_id = $1")
        .bind(oversized.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(audit, 1, "the refused fact is in the system of record");

    // PROVENANCE: the dead letter is itself a fact, staged in the same
    // transaction that set the row aside, naming the row and its kind.
    let alarms: Vec<(serde_json::Value,)> = sqlx::query_as(
        "SELECT payload FROM event_outbox WHERE kind = 'events.outbox.dead_lettered'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(alarms.len(), 1, "one alarm per dead letter");
    let alarm = &alarms[0].0;
    assert_eq!(alarm["event_kind"], "outbox.test.oversized");
    assert_eq!(alarm["event_id"], oversized.id.to_string());
    assert!(
        alarm["outbox_id"].as_i64().is_some(),
        "names the row: {alarm}"
    );
    assert_eq!(alarm["reason"], reason);
    assert!(
        alarm["payload_bytes"].as_i64().unwrap() > 4096,
        "names the size the bus refused: {alarm}"
    );
    assert!(
        alarm.get("payload").is_none() && !alarm.to_string().contains(&"x".repeat(4096)),
        "the alarm must not carry the payload the bus refused"
    );

    // The alarm is an ordinary pending row: the next drain delivers it.
    assert_eq!(pending_count(&db.pool).await.unwrap(), 1);
    let stats = drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(stats.delivered, 1);
    assert_eq!(stats.dead_lettered, 0);
    recording.assert_event_emitted("events.outbox.dead_lettered");
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);

    // IDEMPOTENCE: a dead letter is never re-drained — no further
    // publish attempt, no second alarm.
    let attempts_before = bus.attempts.load(Ordering::SeqCst);
    let stats = drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(stats.delivered + stats.dead_lettered, 0);
    assert_eq!(bus.attempts.load(Ordering::SeqCst), attempts_before);
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE kind = 'events.outbox.dead_lettered'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transport_failure_stops_the_batch_and_dead_letters_nothing() {
    // The other half of the classification: a transport error says
    // nothing about the row, so the row stays pending, the batch stops
    // at it to keep publish order, and nothing is set aside.
    let db = TestDb::new().await;
    stage(&db, &event("outbox.test.transport-a")).await;
    stage(&db, &event("outbox.test.transport-b")).await;

    let failing = Arc::new(FailingBus {
        attempts: AtomicUsize::new(0),
    });
    let stats = drain_outbox_once(&db.pool, &(failing.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(stats.delivered, 0);
    assert_eq!(stats.dead_lettered, 0);
    assert_eq!(
        failing.attempts.load(Ordering::SeqCst),
        1,
        "stopped at the head"
    );
    assert_eq!(pending_count(&db.pool).await.unwrap(), 2);
    let dead: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE dead_lettered_at IS NOT NULL")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(dead, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn staging_refuses_an_event_the_bus_could_never_carry_naming_its_kind() {
    // Where the mistake is cheapest: the writer's own transaction. An
    // event whose encoding exceeds MAX_STAGED_EVENT_BYTES is refused
    // at record time, naming the kind and both sizes, and nothing is
    // staged — rather than committing a row the relay must set aside.
    let db = TestDb::new().await;
    let mut e = event("outbox.test.too-large-to-stage");
    e.payload = serde_json::json!({"blob": "x".repeat(MAX_STAGED_EVENT_BYTES)});

    let mut tx = db.pool.begin().await.unwrap();
    let err = record_event_in_tx(&mut tx, &e)
        .await
        .expect_err("an event over the bound must be refused");
    assert!(
        err.contains("outbox.test.too-large-to-stage"),
        "names the kind: {err}"
    );
    assert!(
        err.contains(&MAX_STAGED_EVENT_BYTES.to_string()),
        "names the bound: {err}"
    );
    tx.rollback().await.unwrap();
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);

    // Just under the bound still stages: the bound refuses only what
    // the bus would refuse, plus the margin.
    let mut ok = event("outbox.test.just-under-the-bound");
    ok.payload = serde_json::json!({"blob": "x".repeat(MAX_STAGED_EVENT_BYTES - 1024)});
    stage(&db, &ok).await;
    assert_eq!(pending_count(&db.pool).await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn drain_preserves_outbox_order_in_audit_log() {
    let db = TestDb::new().await;
    let first = event("outbox.test.order-a");
    let second = event("outbox.test.order-b");

    for e in [&first, &second] {
        let mut tx = db.pool.begin().await.unwrap();
        record_event_in_tx(&mut tx, e).await.unwrap();
        tx.commit().await.unwrap();
    }

    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();

    let ids: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT event_id FROM audit_log WHERE kind LIKE 'outbox.test.order-%' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        ids.iter().map(|r| r.0).collect::<Vec<_>>(),
        vec![first.id, second.id],
        "audit id order must follow outbox order"
    );
    // Bus order too — the relay is the single ordering point.
    let kinds: Vec<String> = bus.events().iter().map(|e| e.kind.clone()).collect();
    assert_eq!(kinds, vec!["outbox.test.order-a", "outbox.test.order-b"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_outbox_drain_is_a_cheap_no_op() {
    let db = TestDb::new().await;
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(stats.delivered, 0);
    assert_eq!(bus.event_count(), 0);
}

// ---------------------------------------------------------------------
// The way out of a dead letter (backlog e22b692e). A row the relay set
// aside stayed aside forever: nothing could put it back once its cause
// was fixed, and nothing could close it once a person judged it
// audit-only. `redeliver_dead_letter` and `resolve_dead_letter` are the
// two arms, and each is ONE transaction that changes the row and stages
// the fact saying so.
// ---------------------------------------------------------------------

fn operator() -> ActorId {
    ActorId::human("emp-outbox-test")
}

/// Set a staged row aside the way the relay does, without a bus: the
/// arms below are judged on the row's state, not on how it got there.
async fn dead_letter_by_hand(db: &TestDb, e: &Event, reason: &str) -> i64 {
    sqlx::query_scalar(
        "UPDATE event_outbox SET dead_lettered_at = NOW(), dead_letter_reason = $2 \
         WHERE event_id = $1 RETURNING id",
    )
    .bind(e.id)
    .bind(reason)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

async fn staged_of_kind(db: &TestDb, kind: &str) -> Vec<serde_json::Value> {
    sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY id")
        .bind(kind)
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

type RowState = (
    Option<chrono::DateTime<Utc>>,
    Option<chrono::DateTime<Utc>>,
    Option<String>,
    Option<chrono::DateTime<Utc>>,
    Option<String>,
);

async fn row_state(db: &TestDb, outbox_id: i64) -> RowState {
    sqlx::query_as(
        "SELECT delivered_at, dead_lettered_at, dead_letter_reason, \
                dead_letter_resolved_at, dead_letter_resolution \
         FROM event_outbox WHERE id = $1",
    )
    .bind(outbox_id)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redelivered_dead_letter_is_pending_again_and_the_act_is_a_fact() {
    // The whole round trip: the relay refuses a row, the cause is fixed
    // (here, a bus with a larger max_payload), the operator redelivers,
    // and the next drain publishes it — once in audit_log, once on the
    // bus, with the refusal the row no longer carries kept on the act.
    let db = TestDb::new().await;
    let mut oversized = event("outbox.test.refused-then-redelivered");
    oversized.payload = serde_json::json!({"blob": "x".repeat(4096)});
    stage(&db, &oversized).await;

    let small = Arc::new(MaxPayloadBus {
        max_payload: 1024,
        attempts: AtomicUsize::new(0),
        inner: RecordingEventBus::new(),
    });
    let stats = drain_outbox_once(&db.pool, &(small.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(stats.dead_lettered, 1);
    // Deliver the dead-letter alarm so the queue holds nothing else.
    drain_outbox_once(&db.pool, &(small.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);
    assert_eq!(dead_lettered_count(&db.pool).await.unwrap(), 1);

    let outbox_id: i64 = sqlx::query_scalar("SELECT id FROM event_outbox WHERE event_id = $1")
        .bind(oversized.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let (_, _, refusal, _, _) = row_state(&db, outbox_id).await;
    let refusal = refusal.expect("dead-lettered with a reason");

    let letter = redeliver_dead_letter(&db.pool, outbox_id, operator())
        .await
        .expect("a dead letter under the bound is redelivered");
    assert_eq!(letter.outbox_id, outbox_id);
    assert_eq!(letter.event_id, oversized.id);
    assert_eq!(letter.event_kind, "outbox.test.refused-then-redelivered");
    assert_eq!(letter.dead_letter_reason, refusal);
    // OUT OF ORDER, said: one later row (the dead-letter alarm itself)
    // reached subscribers before this event will.
    assert_eq!(letter.overtaken_by, Some(1), "{letter:?}");

    // The row is pending again — the relay's own predicate — and no
    // longer counted as a dead letter.
    let (delivered, dead_at, reason, resolved_at, _) = row_state(&db, outbox_id).await;
    assert!(delivered.is_none());
    assert!(
        dead_at.is_none() && reason.is_none(),
        "cleared for the relay"
    );
    assert!(resolved_at.is_none());
    assert_eq!(dead_lettered_count(&db.pool).await.unwrap(), 0);

    // PROVENANCE: the act is a fact, signed by who did it, carrying the
    // refusal the row gave up — and never the payload.
    let acts = staged_of_kind(&db, REDELIVERED_KIND).await;
    assert_eq!(acts.len(), 1, "one act, one fact");
    let act = &acts[0];
    assert_eq!(act["outbox_id"], outbox_id);
    assert_eq!(act["event_id"], oversized.id.to_string());
    assert_eq!(act["event_kind"], "outbox.test.refused-then-redelivered");
    assert_eq!(act["event_source"], "outbox-test");
    assert_eq!(act["dead_letter_reason"], refusal);
    assert!(act["dead_lettered_at"].as_str().is_some(), "{act}");
    assert!(act["payload_bytes"].as_i64().unwrap() > 4096, "{act}");
    assert_eq!(act["_actor"], "emp-outbox-test");
    assert_eq!(act["overtaken_by"], 1, "{act}");
    assert!(
        !act.to_string().contains(&"x".repeat(4096)),
        "the act must not carry the payload"
    );
    assert_eq!(
        pending_count(&db.pool).await.unwrap(),
        2,
        "the row + its act"
    );

    // The read-back confirms the row AND the staged fact, from the
    // database, not from the act's own return value.
    let back = read_back(&db.pool, outbox_id, REDELIVERED_KIND)
        .await
        .unwrap();
    assert_eq!(back.state, "pending");
    assert_eq!(back.resolution, None);
    let act_id: i64 = sqlx::query_scalar("SELECT id FROM event_outbox WHERE kind = $1")
        .bind(REDELIVERED_KIND)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(back.act_outbox_id, Some(act_id));

    // The cause is fixed: the next drain publishes both, and the
    // refused fact is still ONE row in audit_log.
    let recording = RecordingEventBus::new();
    let big = Arc::new(MaxPayloadBus {
        max_payload: 1024 * 1024,
        attempts: AtomicUsize::new(0),
        inner: recording.clone(),
    });
    let stats = drain_outbox_once(&db.pool, &(big.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(stats.delivered, 2);
    assert_eq!(stats.dead_lettered, 0);
    recording.assert_event_emitted("outbox.test.refused-then-redelivered");
    recording.assert_event_emitted(REDELIVERED_KIND);
    let audit: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_id = $1")
        .bind(oversized.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(audit, 1, "redelivery re-publishes, it does not re-log");
    let (delivered, _, _, _, _) = row_state(&db, outbox_id).await;
    assert!(delivered.is_some(), "delivered this time");
}

#[tokio::test(flavor = "multi_thread")]
async fn redeliver_and_resolve_refuse_a_row_that_is_not_dead_lettered() {
    // A pending row, a delivered row and an id with no row: each is
    // refused by both arms, naming what the row IS, and neither the row
    // nor the outbox changes.
    let db = TestDb::new().await;
    let delivered = event("outbox.test.already-delivered");
    stage(&db, &delivered).await;
    let pending = event("outbox.test.still-pending");
    stage(&db, &pending).await;
    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus.clone() as Arc<dyn EventBus>), 1)
        .await
        .unwrap();
    let ids: Vec<(i64, Uuid)> = sqlx::query_as("SELECT id, event_id FROM event_outbox ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    // A batch of one: the first row was delivered, the second is pending.
    assert_eq!(ids[0].1, delivered.id);
    assert_eq!(ids[1].1, pending.id);
    let (delivered_id, pending_id) = (ids[0].0, ids[1].0);
    let before_delivered = row_state(&db, delivered_id).await;
    let before_pending = row_state(&db, pending_id).await;

    for (id, word) in [
        (delivered_id, "delivered"),
        (pending_id, "pending"),
        (i64::MAX, "no outbox row"),
    ] {
        let err = redeliver_dead_letter(&db.pool, id, operator())
            .await
            .expect_err("not a dead letter")
            .to_string();
        assert!(err.contains(word), "redeliver of {id} names {word}: {err}");
        assert!(err.contains("not dead-lettered"), "{err}");
        let err = resolve_dead_letter(&db.pool, id, "judged audit-only", operator())
            .await
            .expect_err("not a dead letter")
            .to_string();
        assert!(err.contains(word), "resolve of {id} names {word}: {err}");
    }
    assert_eq!(row_state(&db, delivered_id).await, before_delivered);
    assert_eq!(row_state(&db, pending_id).await, before_pending);
    assert!(staged_of_kind(&db, REDELIVERED_KIND).await.is_empty());
    assert!(staged_of_kind(&db, RESOLVED_KIND).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn redeliver_refuses_a_dead_letter_over_the_staging_bound() {
    // Redelivery re-stages within the bound `record_event_in_tx`
    // applies: a row over it would only be refused by the bus again, so
    // it is refused here, naming both sizes, and stays dead-lettered.
    // Staged by SQL, because the staging door itself refuses it — the
    // shape of a row set aside before that bound existed.
    let db = TestDb::new().await;
    let e = event("outbox.test.still-too-large");
    let blob = "x".repeat(MAX_STAGED_EVENT_BYTES);
    let outbox_id: i64 = sqlx::query_scalar(
        "INSERT INTO event_outbox (event_id, timestamp, source, kind, payload, \
                                   dead_lettered_at, dead_letter_reason) \
         VALUES ($1, $2, $3, $4, $5, NOW(), 'max payload size exceeded') RETURNING id",
    )
    .bind(e.id)
    .bind(e.timestamp)
    .bind(&e.source)
    .bind(&e.kind)
    .bind(serde_json::json!({ "blob": blob }))
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let err = redeliver_dead_letter(&db.pool, outbox_id, operator())
        .await
        .expect_err("over the bound")
        .to_string();
    assert!(
        err.contains(&MAX_STAGED_EVENT_BYTES.to_string()),
        "names the bound: {err}"
    );
    assert!(err.contains("outbox.test.still-too-large"), "{err}");
    assert!(!err.contains(&"x".repeat(64)), "never the payload: {err}");
    let (_, dead_at, reason, _, _) = row_state(&db, outbox_id).await;
    assert!(dead_at.is_some() && reason.is_some(), "still set aside");
    assert!(staged_of_kind(&db, REDELIVERED_KIND).await.is_empty());
    assert_eq!(dead_lettered_count(&db.pool).await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resolved_dead_letter_stays_aside_is_no_longer_open_and_the_act_is_a_fact() {
    let db = TestDb::new().await;
    let e = event("outbox.test.judged-audit-only");
    stage(&db, &e).await;
    let outbox_id = dead_letter_by_hand(&db, &e, "invalid subject").await;
    assert_eq!(dead_lettered_count(&db.pool).await.unwrap(), 1);
    assert_eq!(pending_count(&db.pool).await.unwrap(), 0);

    // A resolution with nothing to say is refused: the reason IS the
    // record of the decision.
    let err = resolve_dead_letter(&db.pool, outbox_id, "  \n", operator())
        .await
        .expect_err("an empty reason")
        .to_string();
    assert!(err.contains("reason"), "{err}");
    assert!(staged_of_kind(&db, RESOLVED_KIND).await.is_empty());

    let reason = "the subject cannot be fixed; the fact is audit-only by decision";
    let letter = resolve_dead_letter(&db.pool, outbox_id, reason, operator())
        .await
        .expect("a dead letter resolves");
    assert_eq!(letter.dead_letter_reason, "invalid subject");

    // The row stays set aside — still never delivered, still off the
    // queue — and is stamped resolved with the reason.
    let (delivered, dead_at, dl_reason, resolved_at, resolution) = row_state(&db, outbox_id).await;
    assert!(delivered.is_none(), "a resolved row was never delivered");
    assert!(dead_at.is_some());
    assert_eq!(dl_reason.as_deref(), Some("invalid subject"));
    assert!(resolved_at.is_some());
    assert_eq!(resolution.as_deref(), Some(reason));
    assert_eq!(
        dead_lettered_count(&db.pool).await.unwrap(),
        0,
        "no longer open"
    );

    let acts = staged_of_kind(&db, RESOLVED_KIND).await;
    assert_eq!(acts.len(), 1);
    assert_eq!(acts[0]["outbox_id"], outbox_id);
    assert_eq!(acts[0]["event_kind"], "outbox.test.judged-audit-only");
    assert_eq!(acts[0]["resolution"], reason);
    assert_eq!(acts[0]["dead_letter_reason"], "invalid subject");
    assert_eq!(acts[0]["_actor"], "emp-outbox-test");
    // Only the act is pending: the resolved row itself never is.
    assert_eq!(pending_count(&db.pool).await.unwrap(), 1);
    // The read-back takes the resolution's words from the ROW and names
    // the staged fact, rather than echoing what the operator sent.
    assert_eq!(letter.overtaken_by, None, "resolve moves nothing");
    let back = read_back(&db.pool, outbox_id, RESOLVED_KIND).await.unwrap();
    assert_eq!(back.state, "dead-lettered, resolved");
    assert_eq!(back.resolution.as_deref(), Some(reason));
    assert!(back.act_outbox_id.is_some(), "{back:?}");

    // IDEMPOTENCE: a resolved row is closed to both arms.
    for err in [
        redeliver_dead_letter(&db.pool, outbox_id, operator())
            .await
            .expect_err("resolved")
            .to_string(),
        resolve_dead_letter(&db.pool, outbox_id, "again", operator())
            .await
            .expect_err("resolved")
            .to_string(),
    ] {
        assert!(err.contains("resolved"), "{err}");
    }
    assert_eq!(staged_of_kind(&db, RESOLVED_KIND).await.len(), 1);
    assert!(staged_of_kind(&db, REDELIVERED_KIND).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redelivered_row_the_bus_refuses_again_is_dead_lettered_again_and_alarms_again() {
    // Redelivery on the operator's word when the cause is NOT fixed
    // (review L1): the relay sets the row aside a second time and
    // stages a second alarm — one open dead letter, two facts saying
    // it was refused, and nothing silent.
    let db = TestDb::new().await;
    let mut oversized = event("outbox.test.refused-twice");
    oversized.payload = serde_json::json!({"blob": "x".repeat(4096)});
    stage(&db, &oversized).await;
    let small = Arc::new(MaxPayloadBus {
        max_payload: 1024,
        attempts: AtomicUsize::new(0),
        inner: RecordingEventBus::new(),
    });
    let bus = small.clone() as Arc<dyn EventBus>;
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let outbox_id: i64 = sqlx::query_scalar("SELECT id FROM event_outbox WHERE event_id = $1")
        .bind(oversized.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();

    redeliver_dead_letter(&db.pool, outbox_id, operator())
        .await
        .expect("under the staging bound, so redelivered on the operator's word");
    let stats = drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    assert_eq!(stats.dead_lettered, 1, "refused again");

    assert_eq!(dead_lettered_count(&db.pool).await.unwrap(), 1);
    let alarms = staged_of_kind(&db, "events.outbox.dead_lettered").await;
    assert_eq!(alarms.len(), 2, "a second refusal is a second alarm");
    assert!(alarms.iter().all(|a| a["outbox_id"] == outbox_id));
    assert_eq!(staged_of_kind(&db, REDELIVERED_KIND).await.len(), 1);
    let (delivered, dead_at, reason, _, _) = row_state(&db, outbox_id).await;
    assert!(delivered.is_none());
    assert!(dead_at.is_some() && reason.is_some());
}
