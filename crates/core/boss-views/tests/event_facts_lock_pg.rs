//! `event_facts` has two writers: the full rebuild `boss-rebuild-all`
//! runs, and the catch-up `boss-views-catchup` runs every five minutes.
//! Until backlog 8d5ac7c5 neither took the projection's advisory lock,
//! so `boss-rebuild-all`'s "every step is advisory-locked" was false of
//! this one. The projection is windowed (50k-row statements over a log
//! of ~700k rows), so the lock cannot be one transaction's: both paths
//! hold `lock_key("event-facts")` at SESSION level for their whole run.
//!
//! What this does NOT close: a live write whose fact still sits in
//! `event_outbox` is invisible to every rebuilder, locked or not
//! (backlog d6656496).

use boss_testing::TestDb;
use std::time::Duration;

const KEY: i64 = boss_core::rebuild::lock_key("event-facts");

async fn hold(pool: &sqlx::PgPool) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(KEY)
        .execute(&mut *holder)
        .await
        .unwrap();
    holder
}

/// The lock is a session lock, so a run that forgot to release it would
/// leave it held by a pooled connection long after it returned. Taking
/// it from another session proves the run let it go.
async fn assert_released(pool: &sqlx::PgPool) {
    let mut tx = pool.begin().await.unwrap();
    let free: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(KEY)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(free, "the run finished and still held the event-facts lock");
}

async fn write_event(pool: &sqlx::PgPool) {
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES (gen_random_uuid(), NOW(), 'test', 'test.lock', '{}'::jsonb)",
    )
    .execute(pool)
    .await
    .expect("audit row inserts");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_full_rebuild_waits_on_the_event_facts_lock() {
    let db = TestDb::new().await;
    write_event(&db.pool).await;
    let holder = hold(&db.pool).await;

    let pool = db.pool.clone();
    let rebuild = tokio::spawn(async move { boss_views::rebuild_event_facts(&pool).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !rebuild.is_finished(),
        "the full rebuild ran while another session held the event-facts lock"
    );

    holder.rollback().await.unwrap();
    let report = tokio::time::timeout(Duration::from_secs(30), rebuild)
        .await
        .expect("the rebuild finishes once the lock is released")
        .unwrap()
        .expect("rebuild");
    assert_eq!(report.rows_projected, 1);
    assert_released(&db.pool).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_catch_up_waits_on_the_event_facts_lock() {
    let db = TestDb::new().await;
    write_event(&db.pool).await;
    let holder = hold(&db.pool).await;

    let pool = db.pool.clone();
    let catch_up = tokio::spawn(async move { boss_views::catch_up_event_facts(&pool).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !catch_up.is_finished(),
        "the catch-up ran while another session held the event-facts lock"
    );

    holder.rollback().await.unwrap();
    let report = tokio::time::timeout(Duration::from_secs(30), catch_up)
        .await
        .expect("the catch-up finishes once the lock is released")
        .unwrap()
        .expect("catch-up");
    assert_eq!(report.rows_projected, 1);
    assert_released(&db.pool).await;
}
