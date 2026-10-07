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

/// A client can finish closing before PostgreSQL receives Terminate.
/// Hold that real protocol message to make the gate's release race
/// deterministic, without delaying the queries or their responses.
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_catch_up_releases_its_lock_before_session_termination() {
    use sqlx::postgres::{PgPoolOptions, PgSslMode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;

    let db = TestDb::new().await;
    write_event(&db.pool).await;
    let original = db.pool.connect_options();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_address = (original.get_host().to_string(), original.get_port());
    let (release, delayed) = oneshot::channel::<()>();
    let (observed, terminated) = oneshot::channel::<()>();
    let proxy = tokio::spawn(async move {
        let (client, _) = listener.accept().await.unwrap();
        let server = TcpStream::connect(upstream_address).await.unwrap();
        let (mut client_read, mut client_write) = client.into_split();
        let (mut server_read, mut server_write) = server.into_split();
        let upload = async move {
            // PostgreSQL startup has no tag; subsequent messages do.
            let length = client_read.read_u32().await.unwrap();
            assert!((8..=1_048_576).contains(&length));
            let mut startup = vec![0; (length - 4) as usize];
            client_read.read_exact(&mut startup).await.unwrap();
            server_write.write_u32(length).await.unwrap();
            server_write.write_all(&startup).await.unwrap();
            loop {
                let tag = client_read.read_u8().await.unwrap();
                let length = client_read.read_u32().await.unwrap();
                assert!((4..=1_048_576).contains(&length));
                let mut body = vec![0; (length - 4) as usize];
                client_read.read_exact(&mut body).await.unwrap();
                if tag == b'X' {
                    assert_eq!(length, 4);
                    observed.send(()).unwrap();
                    delayed.await.unwrap();
                    server_write.write_u8(tag).await.unwrap();
                    server_write.write_u32(length).await.unwrap();
                    server_write.shutdown().await.unwrap();
                    break;
                }
                server_write.write_u8(tag).await.unwrap();
                server_write.write_u32(length).await.unwrap();
                server_write.write_all(&body).await.unwrap();
            }
        };
        let download = async move {
            tokio::io::copy(&mut server_read, &mut client_write)
                .await
                .unwrap();
        };
        tokio::join!(upload, download);
    });
    let options = original
        .as_ref()
        .clone()
        .host("127.0.0.1")
        .port(address.port())
        .ssl_mode(PgSslMode::Disable);
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let report = tokio::time::timeout(
        Duration::from_secs(30),
        boss_views::catch_up_event_facts(&pool),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(report.rows_projected, 1);
    tokio::time::timeout(Duration::from_secs(5), terminated)
        .await
        .unwrap()
        .unwrap();
    let mut check = db.pool.begin().await.unwrap();
    let released: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(KEY)
        .fetch_one(&mut *check)
        .await
        .unwrap();
    check.rollback().await.unwrap();
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .unwrap()
        .unwrap();
    assert!(
        released,
        "finished catch-up still holds the lock until Terminate reaches PostgreSQL"
    );
}
