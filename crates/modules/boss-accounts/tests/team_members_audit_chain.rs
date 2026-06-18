//! Audit-chain test for `account_team_members`.
//!
//! The team-member assign / unassign / batch-assign handlers + the
//! `mirror_territory_rep` helper must emit an `audit_log` event for
//! every write. Without it a `boss-rebuild-all` cycle would wipe the
//! team roster: `rebuild_accounts` TRUNCATEs `accounts CASCADE`,
//! which cascades through the FK to `account_team_members`, leaving
//! no events to repopulate from.
//!
//! This test exercises (a) `mirror_territory_rep` via POST
//! /api/people/accounts and (b) the dedicated POST + DELETE
//! handlers, drops the projection, and asserts every team-member
//! row reappears from `audit_log` alone.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_accounts::account_team_members::account_team_router;
use boss_accounts::accounts::accounts_router;
use boss_accounts::rebuild_accounts;
use boss_assets_client::FakeAssetsClient;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::NaiveDate;
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct TeamRow {
    account_id: String,
    employee_id: String,
    role: String,
    assigned_on: NaiveDate,
}

async fn snapshot_team(pool: &PgPool) -> Vec<TeamRow> {
    sqlx::query_as(
        "SELECT account_id, employee_id, role, assigned_on \
         FROM account_team_members \
         ORDER BY account_id, role",
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
        pool,
        Some(publisher),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    ))
}

async fn seed_employee(pool: &PgPool, id: &str, role: &str) {
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
         VALUES ($1, 'Test', $2, $3, 'sales', '2024-01-15', 'loc-test', 'full-time', 'active', NULL) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(format!("{id}@boss.example"))
    .bind(role)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn team_assignments_survive_rebuild() {
    let db = TestDb::new().await;
    seed_employee(&db.pool, "emp-rep-001", "territory-rep").await;
    seed_employee(&db.pool, "emp-cs-001", "customer-success").await;
    let app = build_app(db.pool.clone()).await;

    // 1. Create an account — fires `accounts.account.created` AND
    //    `accounts.account.team-assigned` (via mirror_territory_rep).
    TestRequest::post("/api/people/accounts")
        .json(&serde_json::json!({
            "id": "acc-001",
            "name": "Test Brewery",
            "director": "Dr. Test",
            "city": "Austin",
            "state": "TX",
            "tier": "gold",
            "customer_since": "2025-06-01",
            "territory_rep_id": "emp-rep-001",
            "account_type": "wholesale-distributor",
            "contacts": [],
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // 2. Add a customer-success member via the dedicated POST.
    TestRequest::post("/api/people/accounts/acc-001/account-team")
        .json(&serde_json::json!({
            "employee_id": "emp-cs-001",
            "role": "customer-success",
            "actor_id": "emp-rep-001",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Pre-rebuild snapshot. Two rows: territory-rep + customer-success.
    let pre = snapshot_team(&db.pool).await;
    assert_eq!(pre.len(), 2, "expected 2 team rows, got {pre:?}");
    assert!(
        pre.iter()
            .any(|r| r.role == "territory-rep" && r.employee_id == "emp-rep-001")
    );
    assert!(
        pre.iter()
            .any(|r| r.role == "customer-success" && r.employee_id == "emp-cs-001")
    );

    // 3. Rebuild. The team roster must reappear from audit_log alone.
    let report = rebuild_accounts(&db.pool).await.expect("rebuild succeeds");
    assert!(
        report.team_members_upserted >= 2,
        "rebuild should reproduce at least the 2 team rows, got {report:?}"
    );

    let post = snapshot_team(&db.pool).await;
    assert_eq!(
        pre, post,
        "team_members must round-trip exactly through audit_log"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn team_unassignment_survives_rebuild() {
    let db = TestDb::new().await;
    seed_employee(&db.pool, "emp-rep-002", "territory-rep").await;
    seed_employee(&db.pool, "emp-cs-002", "customer-success").await;
    let app = build_app(db.pool.clone()).await;

    TestRequest::post("/api/people/accounts")
        .json(&serde_json::json!({
            "id": "acc-002",
            "name": "Maltworks",
            "director": "Dr. M",
            "city": "Boston",
            "state": "MA",
            "tier": "silver",
            "customer_since": "2025-06-01",
            "territory_rep_id": "emp-rep-002",
            "account_type": "bar-restaurant",
            "contacts": [],
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::post("/api/people/accounts/acc-002/account-team")
        .json(&serde_json::json!({
            "employee_id": "emp-cs-002",
            "role": "customer-success",
            "actor_id": "emp-rep-002",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Now unassign the customer-success rep.
    TestRequest::delete(
        "/api/people/accounts/acc-002/account-team/customer-success?actor_id=emp-rep-002",
    )
    .send(&app)
    .await
    .assert_status(StatusCode::NO_CONTENT);

    // Pre-rebuild: only the territory-rep row should remain.
    let pre = snapshot_team(&db.pool).await;
    assert_eq!(pre.len(), 1);
    assert_eq!(pre[0].role, "territory-rep");

    let report = rebuild_accounts(&db.pool).await.expect("rebuild succeeds");
    assert!(
        report.team_members_unassigned >= 1,
        "rebuild should replay the unassignment, got {report:?}"
    );

    let post = snapshot_team(&db.pool).await;
    assert_eq!(
        pre, post,
        "unassignment must round-trip exactly through audit_log"
    );
}
