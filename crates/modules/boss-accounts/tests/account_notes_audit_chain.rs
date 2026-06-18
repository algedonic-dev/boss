//! Audit-chain test for `account_notes`.
//!
//! Every write to `account_notes` — the dedicated POST/DELETE
//! surface AND the auto-posted interaction notes from team-member
//! assign / unassign — must emit an audit_log event. Without it a
//! `boss-rebuild-all` cycle would lose the entire interaction log
//! (CASCADE through accounts + no events to replay).
//!
//! This test exercises both the standalone POST and the team-
//! change auto-post, then asserts both rows survive
//! `rebuild_accounts` from `audit_log` alone. Soft-delete
//! survival is covered separately.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_accounts::account_notes::account_notes_router;
use boss_accounts::account_team_members::account_team_router;
use boss_accounts::accounts::accounts_router;
use boss_accounts::rebuild_accounts;
use boss_assets_client::FakeAssetsClient;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct NoteRow {
    id: String,
    account_id: String,
    actor_id: String,
    kind: String,
    body: String,
    deleted_at: Option<DateTime<Utc>>,
    deleted_by: Option<String>,
}

async fn snapshot_notes(pool: &PgPool) -> Vec<NoteRow> {
    sqlx::query_as(
        "SELECT id, account_id, actor_id, kind, body, deleted_at, deleted_by \
         FROM account_notes ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn build_app(pool: PgPool) -> Router {
    let publisher = DomainPublisher::new(RecordingEventBus::new(), "accounts")
        .with_audit(Arc::new(PgAuditWriter::new(pool.clone())));
    accounts_router(
        pool.clone(),
        Some(publisher.clone()),
        Arc::new(FakeAssetsClient::with_count(0)),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    )
    .merge(account_team_router(
        pool.clone(),
        Some(publisher.clone()),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    ))
    .merge(account_notes_router(
        pool,
        Some(publisher),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    ))
}

async fn seed_employee(pool: &PgPool, id: &str) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone, created_at) \
         VALUES ('loc-test', 'Test Location', 'office', 'America/Chicago', NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO employees (id, name, email, role, department, hire_date, location, employment_type, status, manager_id) \
         VALUES ($1, 'Test', $2, 'territory-rep', 'sales', '2024-01-15', 'loc-test', 'full-time', 'active', NULL) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(format!("{id}@boss.example"))
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_survive_rebuild() {
    let db = TestDb::new().await;
    seed_employee(&db.pool, "emp-rep-001").await;
    let app = build_app(db.pool.clone()).await;

    // Create the parent account (FK target for notes).
    TestRequest::post("/api/people/accounts")
        .json(&serde_json::json!({
            "id": "acc-001", "name": "Test", "director": "Dr. T",
            "city": "Austin", "state": "TX", "tier": "gold",
            "customer_since": "2025-06-01",
            "territory_rep_id": "emp-rep-001",
            "account_type": "wholesale-distributor", "contacts": [],
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Standalone note via the dedicated POST.
    TestRequest::post("/api/people/accounts/acc-001/notes")
        .json(&serde_json::json!({
            "kind": "call",
            "body": "Q3 pricing call",
            "actor_id": "emp-rep-001",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Auto-posted interaction note via team-member assign.
    TestRequest::post("/api/people/accounts/acc-001/account-team")
        .json(&serde_json::json!({
            "employee_id": "emp-rep-001",
            "role": "executive-sponsor",
            "actor_id": "emp-rep-001",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    let pre = snapshot_notes(&db.pool).await;
    assert_eq!(pre.len(), 2, "expected 2 note rows, got {pre:?}");
    let kinds: Vec<&str> = pre.iter().map(|r| r.kind.as_str()).collect();
    assert!(kinds.contains(&"call"));
    assert!(kinds.contains(&"interaction"));

    let report = rebuild_accounts(&db.pool).await.expect("rebuild");
    assert!(
        report.notes_posted >= 2,
        "rebuild should reproduce both notes, got {report:?}"
    );

    let post = snapshot_notes(&db.pool).await;
    assert_eq!(pre, post, "notes must round-trip exactly through audit_log");
}

#[tokio::test(flavor = "multi_thread")]
async fn note_soft_delete_survives_rebuild() {
    let db = TestDb::new().await;
    seed_employee(&db.pool, "emp-rep-002").await;
    let app = build_app(db.pool.clone()).await;

    TestRequest::post("/api/people/accounts")
        .json(&serde_json::json!({
            "id": "acc-002", "name": "Test2", "director": "Dr. T",
            "city": "Austin", "state": "TX", "tier": "gold",
            "customer_since": "2025-06-01",
            "territory_rep_id": "emp-rep-002",
            "account_type": "wholesale-distributor", "contacts": [],
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    let create_resp = TestRequest::post("/api/people/accounts/acc-002/notes")
        .json(&serde_json::json!({
            "kind": "note",
            "body": "to-be-deleted",
            "actor_id": "emp-rep-002",
        }))
        .send(&app)
        .await;
    create_resp.assert_status(StatusCode::CREATED);
    let body: serde_json::Value = create_resp.assert_json();
    let note_id = body["id"].as_str().unwrap().to_string();

    TestRequest::delete(format!(
        "/api/people/accounts/acc-002/notes/{note_id}?actor_id=emp-rep-002"
    ))
    .send(&app)
    .await
    .assert_status(StatusCode::NO_CONTENT);

    let pre = snapshot_notes(&db.pool).await;
    let pre_target = pre.iter().find(|r| r.id == note_id).expect("note row");
    assert!(pre_target.deleted_at.is_some(), "soft-delete stamped");
    assert_eq!(pre_target.deleted_by.as_deref(), Some("emp-rep-002"));

    let report = rebuild_accounts(&db.pool).await.expect("rebuild");
    assert!(
        report.notes_deleted >= 1,
        "rebuild must replay the soft-delete"
    );

    let post = snapshot_notes(&db.pool).await;
    let post_target = post.iter().find(|r| r.id == note_id).expect("note row");
    assert!(
        post_target.deleted_at.is_some(),
        "soft-delete must round-trip exactly through audit_log"
    );
    assert_eq!(post_target.deleted_by.as_deref(), Some("emp-rep-002"));
}
