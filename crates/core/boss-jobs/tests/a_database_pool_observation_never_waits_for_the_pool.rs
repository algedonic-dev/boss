#![cfg(feature = "postgres")]

#[path = "../src/bin/boss_jobs_api/database_diagnostics.rs"]
mod database_diagnostics;

use axum::http::StatusCode;
use boss_testing::{TestDb, TestRequest};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn fixed_disconnect_settings_keep_defaults_and_observer_context_distinct() {
    let db = TestDb::new().await;
    let sql = std::fs::read_to_string(
        boss_testing::repo_root().join("infra/forge/jobs-database-observation.sql"),
    )
    .unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    for command in [
        "SET TRANSACTION READ ONLY",
        "SET LOCAL statement_timeout=5000",
        "SET LOCAL lock_timeout=1000",
        "SET LOCAL client_connection_check_interval=123",
    ] {
        sqlx::query(command).execute(&mut *tx).await.unwrap();
    }
    let body: Value = sqlx::query_scalar(&sql).fetch_one(&mut *tx).await.unwrap();
    let config = &body["disconnect_settings"];
    assert_eq!(config["state"], "observed");
    let rows = config["rows"].as_array().expect("four fixed GUC rows");
    let names: Vec<_> = rows
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "client_connection_check_interval",
            "tcp_keepalives_count",
            "tcp_keepalives_idle",
            "tcp_keepalives_interval"
        ]
    );
    for row in rows {
        for key in ["setting", "reset_val", "source", "context"] {
            assert!(row[key].is_string(), "missing {key}: {row}");
        }
        assert!(row["pending_restart"].is_boolean());
        assert!(row.get("unit").is_some());
        assert!(row["why"].is_null());
    }
    assert_eq!(rows[0]["setting"], "123");
    assert_eq!(rows[0]["source"], "session");
    assert_ne!(rows[0]["setting"], rows[0]["reset_val"]);
    let session = &body["observer_session"];
    assert_eq!(session["statement_timeout"], "5s");
    assert_eq!(session["lock_timeout"], "1s");
    assert_eq!(session["transaction_read_only"], "on");
    assert!(session["backend_pid"].as_i64().unwrap() > 0);
    assert_eq!(session["transport"], "tcp");
    assert!(
        session["meaning"]
            .as_str()
            .unwrap()
            .contains("not an application-role ceiling")
    );
    let missing_sql = sql.replace(
        "'tcp_keepalives_count'",
        "'fixture_missing_disconnect_setting'",
    );
    let missing: Value = sqlx::query_scalar(&missing_sql)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(missing["disconnect_settings"]["state"], "unavailable");
    let absent = missing["disconnect_settings"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "fixture_missing_disconnect_setting")
        .unwrap();
    assert!(absent["setting"].is_null());
    assert!(absent["why"].as_str().unwrap().contains("unavailable"));
    // The isolated PG sidecar's socket is outside the test filesystem.
    // Exercise the NULL-address SQL branch explicitly, without presenting
    // this controlled input as a real Unix-socket connection measurement.
    let socket_sql = sql.replace("inet_client_addr()", "NULL::inet");
    let socket_body: Value = sqlx::query_scalar(&socket_sql)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(socket_body["observer_session"]["transport"], "unix-socket");
    assert!(socket_body["observer_session"]["client_address"].is_null());
    assert!(
        socket_body["disconnect_settings"]["meaning"]
            .as_str()
            .unwrap()
            .contains("reset_val")
    );
    println!(
        "actual isolated TCP settings: {config}; observer: {session}; controlled NULL-address branch: {}",
        socket_body["observer_session"]
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn a_fully_leased_pool_still_reports_without_acquiring_a_connection() {
    let db = TestDb::new().await;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db.url())
        .await
        .unwrap();
    let held = pool.acquire().await.unwrap();
    let app = database_diagnostics::router(pool.clone());
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        TestRequest::get("/api/jobs/database-observation").send(&app),
    )
    .await
    .expect("diagnostic must not join the saturated pool's wait queue");
    response.assert_status(StatusCode::OK);
    let body: Value = response.assert_json();
    assert_eq!(body["pool"]["size"], 1);
    assert_eq!(body["pool"]["idle"], 0);
    assert_eq!(body["pool"]["max_connections"], 1);
    assert!(body["pool"]["waiters"].is_null());
    assert!(
        body["pool"]["waiters_unavailable"]
            .as_str()
            .unwrap()
            .contains("public")
    );
    assert!(body["observed_at"].as_str().is_some());
    assert_eq!(body["process_id"], std::process::id());
    assert!(!body.to_string().contains(&db.url()));
    drop(held);
    pool.close().await;
}

#[tokio::test]
async fn a_cold_unreachable_pool_is_an_observation_not_a_database_probe() {
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_lazy("postgres://diagnostic-user:never-disclose-this@127.0.0.1:1/not-a-live-db")
        .unwrap();
    let app = database_diagnostics::router(pool.clone());
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        TestRequest::get("/api/jobs/database-observation").send(&app),
    )
    .await
    .unwrap();
    response.assert_status(StatusCode::OK);
    let body: Value = response.assert_json();
    assert_eq!(body["pool"]["size"], 0);
    assert_eq!(body["pool"]["idle"], 0);
    assert_eq!(body["pool"]["max_connections"], 3);
    assert_eq!(body["target"]["host"], "127.0.0.1");
    assert_eq!(body["target"]["port"], 1);
    assert_eq!(body["target"]["database"], "not-a-live-db");
    assert!(!body.to_string().contains("never-disclose-this"));
    assert!(!body.to_string().contains("diagnostic-user"));
    assert!(!body.to_string().contains("postgres://"));
    let response = TestRequest::post("/api/jobs/database-observation")
        .send(&app)
        .await;
    response.assert_status(StatusCode::METHOD_NOT_ALLOWED);
    pool.close().await;
}

#[tokio::test]
async fn the_assembled_mount_authenticates_without_joining_the_login_pool() {
    use boss_core::machine_gate::{MachineGate, Mode, Reading, Slots, gated};
    let db = TestDb::new().await;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db.url())
        .await
        .unwrap();
    let held = pool.acquire().await.unwrap();
    let bus = boss_testing::RecordingEventBus::new();
    let door = Arc::new(boss_jobs::agents::LoginDoor::new(
        Arc::new(boss_jobs::agents::PgAgents::new(pool.clone())),
        boss_core::publisher::DomainPublisher::new(bus, "jobs"),
    ));
    let inner = axum::Router::new().route("/ordinary", axum::routing::get(|| async { "ordinary" }));
    let app = database_diagnostics::mount_with_login(inner, pool.clone(), door);
    let app = gated(
        app,
        Arc::new(MachineGate::new(
            "jobs",
            &["/api/jobs/health"],
            Reading::new(
                Mode::Enforce,
                Slots::new(Some("fixture-machine-token".into()), None, None),
            ),
        )),
    );
    for token in [None, Some("invalid-fixture-token")] {
        let mut request = TestRequest::get("/api/jobs/database-observation");
        if let Some(token) = token {
            request = request.header(boss_core::machine_token::HEADER, token);
        }
        let response = tokio::time::timeout(Duration::from_secs(5), request.send(&app))
            .await
            .unwrap();
        response.assert_status(StatusCode::UNAUTHORIZED);
    }
    let user =
        serde_json::to_string(&boss_policy_client::User::service("diagnostic-reader")).unwrap();
    let ordinary = tokio::time::timeout(
        Duration::from_millis(100),
        TestRequest::get("/ordinary")
            .header(boss_core::machine_token::HEADER, "fixture-machine-token")
            .header("x-boss-user", user.clone())
            .send(&app),
    )
    .await;
    assert!(
        ordinary.is_err(),
        "positive control: the ordinary login still needs its held pool"
    );
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        TestRequest::get("/api/jobs/database-observation")
            .header(boss_core::machine_token::HEADER, "fixture-machine-token")
            .header("x-boss-user", user)
            .send(&app),
    )
    .await
    .expect("assembled diagnostic must not acquire through LoginDoor");
    response.assert_status(StatusCode::OK);
    drop(held);
    pool.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fixed_read_only_sql_observes_a_real_isolated_lock_wait() {
    let db = TestDb::new().await;
    let mut holder = db.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(17)")
        .execute(&mut *holder)
        .await
        .unwrap();
    let pool = db.pool.clone();
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        let mut tx = pool.begin().await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        ready.send(pid).unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(17)")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
    });
    let pid = waiting.await.unwrap();
    let seen = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')",
            )
            .bind(pid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let mut observation = db.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *observation)
        .await
        .unwrap();
    let sql = std::fs::read_to_string(
        boss_testing::repo_root().join("infra/forge/jobs-database-observation.sql"),
    )
    .unwrap();
    let result: Result<Value, _> = sqlx::query_scalar(&sql).fetch_one(&mut *observation).await;
    // The Python collector uses these marked exclusions for the fixed
    // postgres maintenance connection when the serving target is unread.
    let mut server_only = sql.clone();
    for section in ["database-local", "serving-activity"] {
        let (before, rest) = server_only
            .split_once(&format!("-- {section} begin"))
            .unwrap();
        let (_, after) = rest.split_once(&format!("-- {section} end")).unwrap();
        server_only = format!("{before}{after}");
    }
    assert!(!server_only.contains("pg_stat_database"));
    assert!(!server_only.contains("datname = current_database()"));
    let server_result: Result<Value, _> = sqlx::query_scalar(&server_only)
        .fetch_one(&mut *observation)
        .await;
    observation.rollback().await.unwrap();
    holder.rollback().await.unwrap();
    waiter.await.unwrap();
    seen.expect("isolated lock wait must actually exist before observing it");
    let body = result.unwrap();
    let server_body = server_result.unwrap();
    assert!(server_body.get("database_io").is_none());
    assert!(server_body["wal"].is_object());
    assert_eq!(body["transaction_read_only"], "on");
    assert!(
        body["wal"].is_object(),
        "WAL counters must be independently visible"
    );
    assert_eq!(body["checkpointer"]["source_view"], "pg_stat_bgwriter");
    assert!(body["checkpointer"]["counters"]["checkpoints_timed"].is_number());
    assert!(
        body["backend_io"].as_array().unwrap().iter().any(|row| {
            row["backend_type"]
                .as_str()
                .is_some_and(|kind| kind != "client backend")
        }),
        "background IO must not be filtered away"
    );
    assert!(
        body["activities"].as_array().unwrap().iter().all(|row| {
            row.get("application_name").is_some() && row.get("client_addr").is_some()
        })
    );
    assert_eq!(
        body["database"],
        db.pool.connect_options().get_database().unwrap()
    );
    assert!(
        body["wait_groups"].as_array().unwrap().iter().any(|row| {
            row["wait_event_type"] == "Lock" && row["count"].as_u64().unwrap_or(0) > 0
        })
    );
    assert!(body["activities"].as_array().unwrap().iter().any(|row| {
        row["pid"] == pid
            && row["blocking_pids"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty())
    }));
    assert!(
        body["activities"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row.get("query").is_none())
    );
}
