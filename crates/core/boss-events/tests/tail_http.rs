//! Integration tests for `audit_tail_router` — the `/api/events/tail`
//! read surface over `audit_log`.
//!
//! Covers: authz (operator tier + ceo/cto role allow, plain user
//! denies), source/kind filtering, limit clamping, descending order.

#![cfg(feature = "postgres")]

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use boss_core::audit::AuditWriter;
use boss_core::event::Event;
use boss_events::{PgAuditWriter, audit_tail_router};
use boss_testing::TestDb;
use chrono::{Duration, TimeZone, Utc};
use tower::ServiceExt;
use uuid::Uuid;

fn operator_user() -> String {
    serde_json::json!({
        "id": "emp-op",
        "role": "service-tech",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

fn cto_user() -> String {
    serde_json::json!({
        "id": "emp-001",
        "role": "cto",
        "access_tier": "user",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "executive",
    })
    .to_string()
}

fn plain_user() -> String {
    serde_json::json!({
        "id": "emp-2",
        "role": "service-tech",
        "access_tier": "user",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

async fn seed(writer: &PgAuditWriter, source: &str, kind: &str, mins_ago: i64) -> Uuid {
    let id = Uuid::new_v4();
    let ts = Utc::now() - Duration::minutes(mins_ago);
    writer
        .write(&Event {
            id,
            timestamp: ts,
            source: source.into(),
            kind: kind.into(),
            payload: serde_json::json!({"mins_ago": mins_ago}),
        })
        .await
        .unwrap();
    id
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn get_req(uri: &str, user_header: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-boss-user", user_header)
        .header(header::ACCEPT, "application/json")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_user_is_forbidden() {
    let db = TestDb::new().await;
    let app: Router = audit_tail_router(db.pool.clone());

    let resp = app
        .oneshot(get_req("/api/events/tail", &plain_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn cto_role_is_allowed_even_on_user_tier() {
    // Executive roles get global read access. In production the
    // executive set is registered at startup from the Class registry;
    // register it here so `has_global_read("cto")` resolves.
    boss_core::roles::init_executive_roles(
        ["ceo", "coo", "cto", "cfo"].into_iter().map(String::from),
    );
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed(&writer, "jobs", "job.created", 1).await;

    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req("/api/events/tail", &cto_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_tier_is_allowed() {
    let db = TestDb::new().await;
    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req("/api/events/tail", &operator_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_by_source_and_returns_newest_first() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    // 3 rows across 2 sources, at 3 distinct timestamps (newest last
    // in insert order so we can verify DESC on read).
    seed(&writer, "jobs", "job.created", 30).await;
    seed(&writer, "assets", "asset.updated", 20).await;
    let newest_jobs = seed(&writer, "jobs", "job.step.updated", 5).await;

    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req("/api/events/tail?source=jobs", &operator_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 2, "assets row should be filtered out");
    // Newest first → the 5-min-ago row leads.
    assert_eq!(
        rows[0]["event_id"].as_str().unwrap(),
        newest_jobs.to_string()
    );
    assert_eq!(rows[0]["source"].as_str().unwrap(), "jobs");
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_by_kind_substring_case_insensitive() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed(&writer, "jobs", "job.created", 10).await;
    seed(&writer, "jobs", "job.step.updated", 5).await;
    seed(&writer, "assets", "asset.updated", 1).await;

    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req("/api/events/tail?kind=STEP", &operator_user()))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"].as_str().unwrap(), "job.step.updated");
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_clamps_and_payload_roundtrips() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    let id = Uuid::new_v4();
    writer
        .write(&Event {
            id,
            timestamp: Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap(),
            source: "jobs".into(),
            kind: "job.created".into(),
            payload: serde_json::json!({"hello": "world", "n": 42}),
        })
        .await
        .unwrap();

    let app: Router = audit_tail_router(db.pool.clone());
    // limit=0 should clamp up to 1; the one row we wrote still appears.
    let resp = app
        .oneshot(get_req("/api/events/tail?limit=0", &operator_user()))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["payload"]["hello"].as_str().unwrap(), "world");
    assert_eq!(rows[0]["payload"]["n"].as_i64().unwrap(), 42);
}
