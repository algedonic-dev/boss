//! A projection rebuild refuses to replay a log that is missing a
//! committed write (design b046f510, backlog d6656496).
//!
//! A live writer stages its event in `event_outbox` in the same
//! transaction as its projection row; the relay copies it into
//! `audit_log` later. A wipe-and-replay between the two deleted the
//! row and had no fact to restore it from. These pin the shared
//! replay driver's answer — refuse while the log is short, replay once
//! it is whole — and the lock that keeps the answer true until the
//! wipe commits. Every truncating rebuilder that does not ride the
//! driver calls the same helper; `every_truncating_rebuilder_proves_
//! the_log_complete.rs` holds them to it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use boss_core::event::Event;
use boss_core::port::{EventBus, EventBusError, EventStream};
use boss_events::outbox::{
    drain_outbox_once, lock_and_assert_log_complete, record_event_in_tx, undrained, wait_for_drain,
};
use boss_events::replay::{Applied, ReplayStats, replay_projection};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{TimeZone, Utc};
use sqlx::PgPool;
use uuid::Uuid;

const KIND: &str = "replay.probe.created";
const LOCK_KEY: i64 = boss_core::rebuild::lock_key("replay-probe");

/// A bus that always fails: the relay's audit INSERT commits, the
/// publish fails, and the row stays pending with its fact in the log.
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

async fn projection(db: &TestDb) {
    sqlx::query("CREATE TABLE replay_probe (id TEXT PRIMARY KEY)")
        .execute(&db.pool)
        .await
        .unwrap();
}

fn probe_event(id: &str) -> Event {
    Event {
        id: Uuid::new_v4(),
        timestamp: Utc.with_ymd_and_hms(2026, 9, 28, 12, 0, 0).unwrap(),
        source: "replay-probe".to_string(),
        kind: KIND.to_string(),
        payload: serde_json::json!({ "id": id }),
    }
}

/// The live writer's shape: the projection row and its staged event
/// commit together, and nothing drains. Returns the staged event's id.
async fn live_write(pool: &PgPool, id: &str) -> Uuid {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO replay_probe (id) VALUES ($1)")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let event = probe_event(id);
    record_event_in_tx(&mut tx, &event).await.unwrap();
    tx.commit().await.unwrap();
    event.id
}

/// The rebuild under test: wipe `replay_probe`, replay its facts.
async fn rebuild(pool: &PgPool) -> Result<ReplayStats, String> {
    replay_projection(
        pool,
        LOCK_KEY,
        &["DELETE FROM replay_probe"],
        "kind = 'replay.probe.created'",
        async |conn, ev| {
            let id = ev.payload["id"].as_str().unwrap_or_default().to_string();
            sqlx::query("INSERT INTO replay_probe (id) VALUES ($1)")
                .bind(id)
                .execute(&mut *conn)
                .await
                .map_err(|e| e.to_string())?;
            Ok(Applied::Yes)
        },
    )
    .await
}

async fn rows(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT id FROM replay_probe ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// The defect itself: the write is committed, its fact is not yet in
/// audit_log. The rebuild refuses — naming the count and when the
/// oldest write committed — and the row is still there. Once the relay
/// drains, the same rebuild replays it back.
#[tokio::test(flavor = "multi_thread")]
async fn a_replay_refuses_while_a_committed_write_is_undrained() {
    let db = TestDb::new().await;
    projection(&db).await;
    let event_id = live_write(&db.pool, "probe-1").await;
    let staged = undrained(&db.pool).await.unwrap();
    assert_eq!(staged.count, 1);
    let oldest = staged.oldest_created_at.unwrap().to_rfc3339();
    let row_id: i64 = sqlx::query_scalar("SELECT id FROM event_outbox WHERE event_id = $1")
        .bind(event_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let refusal = rebuild(&db.pool)
        .await
        .expect_err("a replay missing a committed write must refuse");
    assert!(
        refusal.contains("1 committed write(s) have not reached audit_log"),
        "the refusal names the count: {refusal}"
    );
    assert!(
        refusal.contains(&oldest),
        "the refusal dates the oldest write ({oldest}): {refusal}"
    );
    // A verdict names what failed: the row a reader goes to look at,
    // and where to look first (review F4).
    for named in [
        format!("event_outbox id {row_id}"),
        format!("event_id {event_id}"),
        format!("kind {KIND}"),
        "relay stuck on publish".to_string(),
    ] {
        assert!(
            refusal.contains(&named),
            "the refusal names {named:?}: {refusal}"
        );
    }
    assert_eq!(rows(&db.pool).await, ["probe-1"], "the row survives");

    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    let stats = rebuild(&db.pool).await.expect("a whole log replays");
    assert_eq!(stats.processed, 1);
    assert_eq!(rows(&db.pool).await, ["probe-1"], "replayed from the log");
}

/// The relay's audit INSERT committed and its publish failed: the row
/// is still pending (`delivered_at` NULL) but the log holds the fact,
/// so the replay is whole and must not refuse — `delivered_at IS NULL`
/// alone would.
#[tokio::test(flavor = "multi_thread")]
async fn a_copied_but_unstamped_write_does_not_refuse() {
    let db = TestDb::new().await;
    projection(&db).await;
    live_write(&db.pool, "probe-2").await;
    let failing = Arc::new(FailingBus {
        attempts: AtomicUsize::new(0),
    });
    drain_outbox_once(&db.pool, &(failing.clone() as Arc<dyn EventBus>), 100)
        .await
        .unwrap();
    assert_eq!(failing.attempts.load(Ordering::SeqCst), 1);
    let unstamped: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE delivered_at IS NULL")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(unstamped, 1, "the row is still pending");

    let stats = rebuild(&db.pool).await.expect("the log holds the fact");
    assert_eq!(stats.processed, 1);
    assert_eq!(rows(&db.pool).await, ["probe-2"]);
}

/// The check holds until the wipe commits: once a rebuild has proved
/// the log whole, a writer staging a new event waits for the rebuild's
/// transaction rather than slipping in between the check and the wipe.
#[tokio::test(flavor = "multi_thread")]
async fn a_writer_waits_for_a_rebuild_that_has_proved_the_log() {
    let db = TestDb::new().await;
    projection(&db).await;
    let mut rebuild_tx = db.pool.begin().await.unwrap();
    lock_and_assert_log_complete(&mut rebuild_tx, &["replay_probe"])
        .await
        .expect("an empty outbox is a whole log");

    let pool = db.pool.clone();
    let writer = tokio::spawn(async move { live_write(&pool, "probe-3").await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !writer.is_finished(),
        "a write committed between the check and the wipe"
    );

    rebuild_tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), writer)
        .await
        .expect("the writer proceeds once the rebuild commits")
        .unwrap();
    assert_eq!(undrained(&db.pool).await.unwrap().count, 1);
}

/// The helper interpolates its tables into `LOCK TABLE`, so anything
/// but a bare identifier is refused before any SQL runs.
#[tokio::test(flavor = "multi_thread")]
async fn the_lock_refuses_a_table_that_is_not_a_bare_identifier() {
    let db = TestDb::new().await;
    let mut tx = db.pool.begin().await.unwrap();
    let err = lock_and_assert_log_complete(&mut tx, &["replay_probe; DROP TABLE x"])
        .await
        .expect_err("not an identifier");
    assert!(err.contains("bare identifiers"), "{err}");
}

/// The unlocked wait returns as soon as the relay has drained, and at
/// its deadline hands back what is still owed rather than refusing —
/// the locked check is the one that decides.
#[tokio::test(flavor = "multi_thread")]
async fn the_drain_wait_returns_what_is_still_owed_at_its_deadline() {
    let db = TestDb::new().await;
    projection(&db).await;
    assert_eq!(
        wait_for_drain(&db.pool, Duration::ZERO)
            .await
            .unwrap()
            .count,
        0
    );
    live_write(&db.pool, "probe-4").await;
    let owed = wait_for_drain(&db.pool, Duration::from_millis(300))
        .await
        .unwrap();
    assert_eq!(owed.count, 1);

    let pool = db.pool.clone();
    let relay = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let bus = RecordingEventBus::new();
        drain_outbox_once(&pool, &(bus as Arc<dyn EventBus>), 100)
            .await
            .unwrap();
    });
    let drained = wait_for_drain(&db.pool, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(drained.count, 0, "the wait sees the relay catch up");
    relay.await.unwrap();
}

/// Review F1: a writer that touches NO wiped table but stages a fact
/// the replay folds — a cross-service writer. Only the SHARE lock on
/// event_outbox holds it off; without it the check reads zero, the
/// writer commits, and the replay misses its fact. The writer in
/// `a_writer_waits_for_a_rebuild_that_has_proved_the_log` also writes
/// the wiped table, so the table lock alone blocks it and that test
/// cannot tell whether the outbox lock exists.
#[tokio::test(flavor = "multi_thread")]
async fn a_writer_of_another_table_waits_for_the_rebuild() {
    let db = TestDb::new().await;
    projection(&db).await;
    let mut rebuild_tx = db.pool.begin().await.unwrap();
    lock_and_assert_log_complete(&mut rebuild_tx, &["replay_probe"])
        .await
        .expect("an empty outbox is a whole log");

    let pool = db.pool.clone();
    let writer = tokio::spawn(async move {
        let mut tx = pool.begin().await.unwrap();
        record_event_in_tx(&mut tx, &probe_event("x-1"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !writer.is_finished(),
        "a fact was staged between the check and the wipe"
    );
    rebuild_tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), writer)
        .await
        .expect("the writer proceeds once the rebuild commits")
        .unwrap();
}

/// Is `table` held in ACCESS EXCLUSIVE by some session right now?
async fn held_exclusively(pool: &PgPool, table: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_locks l JOIN pg_class c ON c.oid = l.relation \
         WHERE c.relname = $1 AND l.mode = 'AccessExclusiveLock' AND l.granted)",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Review F2: the jobs-create shape — parent row, its fact, THEN the
/// child row — against a wipe listed child first (FK order). The
/// rebuild holds the child and waits for the parent the writer holds,
/// while the writer waits for the child: a cycle. No fixed lock order
/// is safe for every writer shape, so the rebuild takes its locks under
/// a lock timeout shorter than Postgres's deadlock check and backs off:
/// the LIVE WRITE commits, and the rebuild, on its retry, sees that
/// write undrained and refuses — never a deadlock on either side.
///
/// Deterministic: the writer does not guess the rebuild's timing, it
/// waits until pg_locks shows the rebuild holding the child.
#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_yields_to_a_writer_it_would_deadlock() {
    let db = TestDb::new().await;
    for ddl in [
        "CREATE TABLE rp_parent (id TEXT PRIMARY KEY)",
        "CREATE TABLE rp_child (id TEXT PRIMARY KEY, parent TEXT REFERENCES rp_parent(id))",
    ] {
        sqlx::query(ddl).execute(&db.pool).await.unwrap();
    }

    let (parent_held_tx, parent_held) = tokio::sync::oneshot::channel();
    let pool = db.pool.clone();
    let writer = tokio::spawn(async move {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("INSERT INTO rp_parent VALUES ('p')")
            .execute(&mut *tx)
            .await
            .unwrap();
        record_event_in_tx(&mut tx, &probe_event("p"))
            .await
            .unwrap();
        parent_held_tx.send(()).unwrap();
        // Close the cycle only once the rebuild holds the child.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while !held_exclusively(&pool, "rp_child").await {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the rebuild never took the child"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        sqlx::query("INSERT INTO rp_child VALUES ('c', 'p')")
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())
    });

    parent_held.await.unwrap();
    let rebuild = replay_projection(
        &db.pool,
        LOCK_KEY,
        &["DELETE FROM rp_child", "DELETE FROM rp_parent"],
        "kind = 'replay.probe.created'",
        async |_conn, _ev| Ok(Applied::Yes),
    )
    .await;
    let written = tokio::time::timeout(Duration::from_secs(30), writer)
        .await
        .expect("the writer finishes")
        .unwrap();

    assert_eq!(
        written,
        Ok(()),
        "the live write committed; rebuild={rebuild:?}"
    );
    let refusal = rebuild.expect_err("the writer's fact is still undrained");
    assert!(
        refusal.contains("1 committed write(s) have not reached audit_log"),
        "the rebuild backed off and then refused on the undrained write, \
         not on a deadlock: {refusal}"
    );
    let children: i64 = sqlx::query_scalar("SELECT count(*) FROM rp_child")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(children, 1, "the live write's rows survive");
}
