//! A process's environment mode cannot stand in for an original
//! shared mode witness, especially during a rolling restart.
#![cfg(feature = "postgres")]
use std::sync::{Arc, RwLock};

use axum::{body::Body, http::Request};
use boss_clock::{http::ClockApiState, types::ClockMode};
use boss_testing::TestDb;
use tower::ServiceExt;

fn app(db: &TestDb, mode: ClockMode) -> axum::Router {
    boss_clock::http::router(ClockApiState {
        mode,
        params: Arc::new(RwLock::new(None)),
        pool: Some(db.pool.clone()),
    })
}

async fn read(app: axum::Router) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .uri("/api/clock/now")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn persistent_wall_mode_requires_its_original_mode_witness() {
    let db = TestDb::new().await;
    assert!(
        !read(app(&db, ClockMode::Wall)).await.status().is_success(),
        "the current environment is not original mode/coverage evidence"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sim_process_refuses_a_different_recorded_global_mode() {
    let db = TestDb::new().await;
    sqlx::query("INSERT INTO sim_clock(epoch_start_date,warp_factor) VALUES ('2026-01-01',1)")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO clock_mapping_mode(mode,epoch) VALUES ('wall',gen_random_uuid())")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        !read(app(&db, ClockMode::Sim)).await.status().is_success(),
        "mixed-mode processes must not claim incompatible global mappings"
    );
    assert!(read(app(&db, ClockMode::Wall)).await.status().is_success());
}

#[tokio::test(flavor = "multi_thread")]
async fn real_startup_and_api_writers_leave_replayable_original_revisions() {
    let db = TestDb::new().await;
    sqlx::query("INSERT INTO sim_clock(epoch_start_date,warp_factor) VALUES ('2026-01-01',1)")
        .execute(&db.pool)
        .await
        .unwrap();
    boss_clock::postgres::record_mode(&db.pool, ClockMode::Sim)
        .await
        .unwrap();
    let router = app(&db, ClockMode::Sim);
    for path in ["pause", "resume", "restart-epoch"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/clock/{path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{path} is recorded before success"
        );
    }
    let facts: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind='clock.mapping.recorded' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts.len(), 5);
    assert!(facts[0]["mode"].is_null());
    assert_eq!(facts[1]["mode"], "sim");
    assert!(facts[1]["epoch"].is_string());
    assert_eq!(facts[2]["params"]["paused"], true);
    assert_eq!(facts[3]["params"]["paused"], false);
    assert_ne!(
        facts[4]["params"]["wall_anchor"],
        facts[3]["params"]["wall_anchor"]
    );
    for pair in facts.windows(2) {
        assert_eq!(pair[1]["previous_revision"], pair[0]["revision"]);
    }
    // A direct Jobs-style reset reaches the same actual served authority.
    sqlx::query("UPDATE sim_clock SET epoch_start_date='2031-02-03',wall_anchor=clock_timestamp() WHERE id=1")
        .execute(&db.pool).await.unwrap();
    let response = read(router).await;
    assert!(response.status().is_success());
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(body["now"].as_str().unwrap().starts_with("2031-02-03"));
}
