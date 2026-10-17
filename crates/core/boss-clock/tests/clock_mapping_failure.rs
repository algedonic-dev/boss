//! A history row cannot prove a clock which already served an
//! unpersisted mapping. Failed durable writes must not publish it.
#![cfg(feature = "postgres")]

use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::{body::Body, http::Request};
use boss_clock::{
    http::ClockApiState,
    types::{ClockMode, SimClockParams},
};
use chrono::{NaiveDate, Utc};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

#[tokio::test]
async fn a_failed_mapping_persistence_neither_reports_success_nor_changes_served_state() {
    let initial = SimClockParams {
        epoch_start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        epoch_end: None,
        warp_factor: 1.0,
        wall_anchor: Utc::now(),
        paused: false,
        paused_at: None,
        paused_offset_seconds: 0.0,
        restart_in_progress: false,
    };
    let params = Arc::new(RwLock::new(Some(initial)));
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(100))
        .connect_lazy("postgres://127.0.0.1:1/unreachable_clock_mapping_test")
        .unwrap();
    let app = boss_clock::http::router(ClockApiState {
        mode: ClockMode::Sim,
        params: params.clone(),
        pool: Some(pool),
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/clock/configure")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"epoch_start":"2030-01-01"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !response.status().is_success(),
        "no durable mapping is not success"
    );
    assert_eq!(
        serde_json::to_value(*params.read().unwrap()).unwrap(),
        serde_json::to_value(Some(initial)).unwrap(),
        "failed persistence must not publish a new served mapping"
    );
    let reading = app
        .oneshot(
            Request::builder()
                .uri("/api/clock/now")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !reading.status().is_success(),
        "unavailable durable authority is not a proven cached clock mapping"
    );
}
