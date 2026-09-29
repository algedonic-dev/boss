//! Operator-audit-trail tests for the four boss-ledger control-
//! plane handlers: create-period, lock, unlock, create-revenue-
//! schedule. These rows are system-of-record (gl_periods +
//! revenue_schedules aren't derived from anything), so the
//! events don't drive a rebuilder — they're for the auditor
//! "who did what when" trail.
//!
//! Each handler records a `ledger.period.{locked,unlocked,created}`
//! or `ledger.revenue_schedule.created` event carrying the actor_id
//! + timestamp + key fields — on the transactional outbox, in the
//! same tx as the row (outbox phase 2); the relay drain moves it to
//! audit_log. So an auditor has a record of who locked period FY26
//! and when — not just that the row currently shows status='locked'.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_ledger::http::{LedgerApiState, router};
use boss_testing::{RecordingEventBus, TestDb};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn build_router(db: &TestDb) -> Router {
    // No publisher: stamps fall back to source="ledger" and the
    // handlers record on the outbox — deliberately NO direct audit
    // writer, so these tests only pass through the real
    // outbox → relay → audit_log path.
    router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        // The read gate is not this test's subject (tests/the_ledger_read_gate.rs).
        policy: std::sync::Arc::new(boss_policy_client::PermissivePolicyClient),
    })
}

/// The `X-Boss-User` a controller's session carries: the period doors
/// refuse a request that names no caller (backlog 25a4f7f9).
const CONTROLLER: &str = r#"{"id":"emp-controller","role":"controller","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"finance"}"#;

/// Drain the outbox through the relay pipeline (outbox → audit_log →
/// bus → delivered), then count. Every payload SELECT in these tests
/// follows a count call, so the drain here covers them too.
async fn count_audit_events(db: &TestDb, kind: &str) -> i64 {
    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE kind = $1")
        .bind(kind)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    row.0
}

/// POST as a finance controller: creating a period or a revenue schedule
/// asks policy for its caller and refuses an unsigned one 401 (backlog
/// 34f0a954). The lock and unlock below build their own signed requests.
async fn post_signed(app: Router, path: &str, body: Value) -> (StatusCode, Value) {
    let signer = r#"{"id":"emp-controller","role":"controller","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"finance"}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("x-boss-user", signer)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn create_period_emits_audit_trail() {
    let db = TestDb::new().await;
    let app = build_router(&db);
    let (status, body) = post_signed(app, "/api/ledger/periods", json!({"year": 2099})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("id").is_some(), "response carries period id");

    let n = count_audit_events(&db, "ledger.period.created").await;
    assert_eq!(n, 1, "exactly one ledger.period.created event landed");

    let payload: (Value,) = sqlx::query_as(
        "SELECT payload FROM audit_log WHERE kind = 'ledger.period.created' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload.0["kind"], "year");
    assert!(payload.0["actor_id"].is_string(), "event carries actor_id");
    assert!(
        payload.0["created_at"].is_string(),
        "event carries timestamp"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lock_and_unlock_emit_audit_trail() {
    let db = TestDb::new().await;
    let app = build_router(&db);

    // Create the period via the HTTP path (also lands the
    // create event — we'll filter to lock/unlock kinds below).
    let (_, body) = post_signed(app.clone(), "/api/ledger/periods", json!({"year": 2098})).await;
    let id = body["id"].as_str().expect("period id").to_string();

    // Lock. The locker is the signed caller, not a body field (backlog
    // 975c228f). The request must BE signed: closing or reopening a
    // month asks policy for its caller, and an unsigned one is refused
    // (backlog 25a4f7f9) — it used to lock as `automation:platform`.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/ledger/periods/{id}/lock"))
                .header("content-type", "application/json")
                .header("x-boss-user", CONTROLLER)
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let locked = count_audit_events(&db, "ledger.period.locked").await;
    assert_eq!(locked, 1, "lock event landed exactly once");

    let payload: (Value,) = sqlx::query_as(
        "SELECT payload FROM audit_log WHERE kind = 'ledger.period.locked' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload.0["locked_by"], "emp-controller");
    assert!(payload.0["checksum"].is_string(), "event carries checksum");
    assert!(payload.0["actor_id"].is_string());

    // Unlock.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/ledger/periods/{id}/unlock"))
                .header("x-boss-user", CONTROLLER)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let unlocked = count_audit_events(&db, "ledger.period.unlocked").await;
    assert_eq!(unlocked, 1, "unlock event landed exactly once");

    let payload: (Value,) = sqlx::query_as(
        "SELECT payload FROM audit_log WHERE kind = 'ledger.period.unlocked' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload.0["period_id"], id);
    assert!(payload.0["actor_id"].is_string());
    assert!(payload.0["unlocked_at"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn create_revenue_schedule_emits_audit_trail() {
    let db = TestDb::new().await;
    // FK target — revenue_schedules.account_id REFERENCES accounts(id).
    sqlx::query(
        "INSERT INTO accounts (id, name, director, city, state, tier, customer_since, territory_rep_id, account_type) \
         VALUES ('acc-001', 'Test Co', 'Director', 'Austin', 'TX', 'gold', '2025-06-01', 'emp-rep-001', 'wholesale-distributor') \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let app = build_router(&db);

    let (status, _) = post_signed(
        app,
        "/api/ledger/revenue-schedules",
        json!({
            "id": "rs-test-001",
            "source_kind": "agreement",
            "source_id": "sa-001",
            "account_id": "acc-001",
            "revenue_category": "subscription",
            "revenue_account": "4000",
            "deferred_account": "2200",
            "total_cents": 1_200_000,
            "start_date": "2026-01-01",
            "end_date": "2026-12-31",
            "frequency": "monthly",
            "next_recognition_date": "2026-01-31"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let n = count_audit_events(&db, "ledger.revenue_schedule.created").await;
    assert_eq!(n, 1, "schedule create event landed exactly once");

    let payload: (Value,) = sqlx::query_as(
        "SELECT payload FROM audit_log WHERE kind = 'ledger.revenue_schedule.created' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload.0["schedule_id"], "rs-test-001");
    assert_eq!(payload.0["frequency"], "monthly");
    assert_eq!(payload.0["total_cents"], 1_200_000);
    assert!(payload.0["actor_id"].is_string());
    assert!(payload.0["created_at"].is_string());
}
