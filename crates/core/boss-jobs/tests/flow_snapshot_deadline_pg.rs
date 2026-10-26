#![cfg(feature = "postgres")]
//! SQL refusal must return the original backend idle, without a leaked setting.
//! A subsequent native snapshot has its own statement bounds; its total query
//! latency is not the connection-recovery contract (2450fba2).

use std::time::{Duration, Instant};

use boss_jobs::{JobsRepository, PgJobs, membership_replay::ReplayInput, port::JobScope};
use boss_testing::TestDb;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool, postgres::PgPoolOptions};

// A hung fixture must terminate; this is not a production latency contract.
const TEST_HANG_WATCHDOG: Duration = Duration::from_secs(120);

async fn single_connection(db: &TestDb) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(TEST_HANG_WATCHDOG)
        .connect_with(db.pool.connect_options().as_ref().clone())
        .await
        .unwrap()
}

async fn backend(connection: &mut PgConnection) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(connection)
        .await
        .unwrap()
}

async fn recovered(
    connection: &mut PgConnection,
    observer: &PgPool,
    original_backend: i32,
    original_timeout: &str,
) -> Result<(), String> {
    let (pid, setting): (i32, String) =
        sqlx::query_as("SELECT pg_backend_pid(),current_setting('statement_timeout')")
            .fetch_one(connection)
            .await
            .map_err(|error| error.to_string())?;
    if pid != original_backend {
        return Err(format!("backend changed: {original_backend} -> {pid}"));
    }
    if setting != original_timeout {
        return Err(format!(
            "statement_timeout leaked: expected {original_timeout}, observed {setting}"
        ));
    }
    let (state, no_transaction): (String, bool) =
        sqlx::query_as("SELECT state,xact_start IS NULL FROM pg_stat_activity WHERE pid=$1")
            .bind(pid)
            .fetch_one(observer)
            .await
            .map_err(|error| error.to_string())?;
    if state != "idle" || !no_transaction {
        return Err(format!(
            "backend {pid} has not rolled back: state={state}, no_transaction={no_transaction}"
        ));
    }
    Ok(())
}

async fn subsequent_snapshot(jobs: &PgJobs, since: DateTime<Utc>) -> Result<ReplayInput, String> {
    // Recovery is observed independently on the exact backend. This is a
    // fixture watchdog, not a latency promise for a multi-statement read:
    // each native statement still owns its unchanged 20s SQL bound. The
    // controlled lock below exposes the old 5s aggregate clock's false RED.
    tokio::time::timeout(
        TEST_HANG_WATCHDOG,
        jobs.membership_snapshot(since, Some("alice".into()), &JobScope::All, 100),
    )
    .await
    .map_err(|_| {
        "subsequent snapshot exceeded the 120s fixture watchdog; recovery is observed separately"
            .to_string()
    })?
    .map_err(|error| error.to_string())
}

#[tokio::test]
async fn a_locked_native_snapshot_refuses_and_restores_its_connection() {
    let phase = Instant::now();
    let db = TestDb::new().await;
    let pool = single_connection(&db).await;
    let original_backend = {
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query("SET statement_timeout = '0'")
            .execute(&mut *connection)
            .await
            .unwrap();
        backend(&mut connection).await
    };
    let jobs = PgJobs::new(pool.clone());
    let mut blocker = db.pool.begin().await.unwrap();
    let blocker_backend = backend(&mut blocker).await;
    sqlx::query("LOCK TABLE audit_log IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    eprintln!(
        "snapshot recovery: db={} backend={original_backend} blocker={blocker_backend} lock-held elapsed={:?}",
        db.pool.connect_options().get_database().unwrap(),
        phase.elapsed()
    );
    let since = Utc::now() - chrono::Duration::hours(24);
    let observed = tokio::time::timeout(
        TEST_HANG_WATCHDOG,
        jobs.membership_snapshot(since, Some("alice".into()), &JobScope::All, 100),
    )
    .await;
    // Always release the fixture lock, including the causal failure leg.
    blocker.rollback().await.unwrap();
    let refused = observed
        .expect("initial native snapshot exceeded the fixture hang watchdog")
        .expect_err("a snapshot cannot pass through an ACCESS EXCLUSIVE table lock");
    assert!(
        refused.to_string().contains("statement timeout"),
        "{refused}"
    );
    eprintln!(
        "snapshot recovery: SQL refused error={refused} elapsed={:?}",
        phase.elapsed()
    );
    {
        let mut connection = pool.acquire().await.unwrap();
        recovered(&mut connection, &db.pool, original_backend, "0")
            .await
            .expect("the exact backend must be idle and retain its original session timeout");
    }
    eprintln!(
        "snapshot recovery: original backend idle elapsed={:?}",
        phase.elapsed()
    );
    let reading = subsequent_snapshot(&jobs, since)
        .await
        .expect("an empty native schema yields a snapshot with honest unknown clock coverage");
    assert!(reading.until > reading.since);
    eprintln!(
        "snapshot recovery: subsequent native snapshot finished elapsed={:?}",
        phase.elapsed()
    );
    pool.close().await;
}

#[tokio::test]
async fn a_recovered_native_reader_can_wait_for_a_new_lock_beyond_five_seconds() {
    let db = TestDb::new().await;
    let pool = single_connection(&db).await;
    let reader = {
        let mut connection = pool.acquire().await.unwrap();
        let pid = backend(&mut connection).await;
        recovered(&mut connection, &db.pool, pid, "0")
            .await
            .unwrap();
        pid
    };
    let jobs = PgJobs::new(pool.clone());
    let mut blocker = db.pool.begin().await.unwrap();
    let owner = backend(&mut blocker).await;
    sqlx::query("LOCK TABLE audit_log IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let since = Utc::now() - chrono::Duration::hours(24);
    let read = subsequent_snapshot(&jobs, since);
    let release = async {
        tokio::time::timeout(Duration::from_secs(22), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT $2=ANY(pg_blocking_pids($1))")
                    .bind(reader)
                    .bind(owner)
                    .fetch_one(&db.pool)
                    .await
                    .unwrap();
                if waiting {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("native reader must reach the independently identified lock owner");
        // Only this confirmed blocking edge starts the deliberate delay.
        eprintln!("snapshot recovery control: reader={reader} blocked-by={owner}; hold6s");
        tokio::time::sleep(Duration::from_secs(6)).await;
        blocker.rollback().await.unwrap();
    };
    let (result, ()) = tokio::join!(read, release);
    assert!(
        result.is_ok(),
        "a new permitted SQL lock wait is not a failed connection recovery: {}",
        result.err().unwrap_or_default()
    );
    let mut connection = pool.acquire().await.unwrap();
    recovered(&mut connection, &db.pool, reader, "0")
        .await
        .unwrap();
    drop(connection);
    pool.close().await;
}

#[tokio::test]
async fn the_recovery_observer_rejects_missing_rollback() {
    let db = TestDb::new().await;
    let mut connection = db.pool.acquire().await.unwrap();
    let pid = backend(&mut connection).await;
    recovered(&mut connection, &db.pool, pid, "0")
        .await
        .unwrap();
    sqlx::query("BEGIN")
        .execute(&mut *connection)
        .await
        .unwrap();
    let refused = recovered(&mut connection, &db.pool, pid, "0").await;
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .unwrap();
    let why = refused.unwrap_err();
    eprintln!("snapshot recovery negative control: {why}");
    assert!(why.contains("has not rolled back"));
    recovered(&mut connection, &db.pool, pid, "0")
        .await
        .unwrap();
}

#[tokio::test]
async fn the_recovery_observer_rejects_a_leaked_session_timeout() {
    let db = TestDb::new().await;
    let mut connection = db.pool.acquire().await.unwrap();
    let pid = backend(&mut connection).await;
    recovered(&mut connection, &db.pool, pid, "0")
        .await
        .unwrap();
    sqlx::query("SET statement_timeout = '7s'")
        .execute(&mut *connection)
        .await
        .unwrap();
    let refused = recovered(&mut connection, &db.pool, pid, "0").await;
    sqlx::query("RESET statement_timeout")
        .execute(&mut *connection)
        .await
        .unwrap();
    let why = refused.unwrap_err();
    eprintln!("snapshot recovery negative control: {why}");
    assert!(why.contains("statement_timeout leaked"));
    recovered(&mut connection, &db.pool, pid, "0")
        .await
        .unwrap();
}
