//! Native TCP disconnect behavior, isolated from the build-free manifest pin.
#[path = "support/postgres_disconnect_settings.rs"]
mod postgres_disconnect_settings;
use postgres_disconnect_settings::declared_settings;

#[tokio::test]
async fn the_real_server_detects_a_disconnected_query_only_when_polling_is_enabled() {
    use sqlx::{Connection, Executor};
    use std::time::Duration;
    let db = boss_testing::TestDb::new().await;
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(
        version >= 160000,
        "the deployed PostgreSQL16 contract requires PG16+"
    );
    let settings = declared_settings();
    let enabled = settings
        .iter()
        .find(|(key, _)| key == "client_connection_check_interval")
        .expect("the manifest must declare polling")
        .1
        .clone();
    // Polling is not a query deadline: a connected client's query may outlive
    // the interval and must still return its result.
    let mut alive = sqlx::PgConnection::connect(&db.url()).await.unwrap();
    sqlx::query("SELECT set_config('client_connection_check_interval',$1,false)")
        .bind(&enabled)
        .execute(&mut alive)
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(20),
        alive.execute("SELECT pg_sleep(11)"),
    )
    .await
    .unwrap()
    .unwrap();
    alive.close().await.unwrap();
    for (interval, should_end) in [("0".to_owned(), false), (enabled, true)] {
        let mut connection = sqlx::PgConnection::connect(&db.url()).await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        let tcp: bool = sqlx::query_scalar("SELECT inet_client_addr() IS NOT NULL")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert!(
            tcp,
            "disconnect and keepalive controls require a real TCP client"
        );
        sqlx::query("SELECT set_config('client_connection_check_interval',$1,false)")
            .bind(&interval)
            .execute(&mut connection)
            .await
            .unwrap();
        for (key, value) in settings
            .iter()
            .filter(|(key, _)| key.starts_with("tcp_keepalives_"))
        {
            let actual: String = sqlx::query_scalar("SELECT set_config($1,$2,false)")
                .bind(key)
                .bind(value)
                .fetch_one(&mut connection)
                .await
                .unwrap();
            assert_eq!(&actual, value);
        }
        let query = tokio::spawn(async move { connection.execute("SELECT pg_sleep(40)").await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND state='active' AND wait_event='PgSleep')")
                    .bind(pid).fetch_one(&db.pool).await.unwrap();
                if active { break; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.unwrap();
        // Aborting this owned client drops its socket, without a PostgreSQL
        // cancellation request. Only the server's client polling can notice it.
        query.abort();
        assert!(query.await.unwrap_err().is_cancelled());
        let observation = tokio::time::timeout(
            Duration::from_secs(if should_end { 15 } else { 2 }),
            async {
                loop {
                    let present: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",
                    )
                    .bind(pid)
                    .fetch_one(&db.pool)
                    .await
                    .unwrap();
                    if !present {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        )
        .await;
        // Cleanup is bounded to this isolated TestDb and the exact owned PID;
        // run it even when a behavioral assertion will fail.
        let _: bool = sqlx::query_scalar(
            "SELECT COALESCE(pg_cancel_backend(pid),false) FROM pg_stat_activity WHERE pid=$1",
        )
        .bind(pid)
        .fetch_optional(&db.pool)
        .await
        .unwrap()
        .unwrap_or(false);
        assert_eq!(
            observation.is_ok(),
            should_end,
            "interval {interval}: disconnected backend lifetime"
        );
        let healthy: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(healthy, 1);
    }
}
