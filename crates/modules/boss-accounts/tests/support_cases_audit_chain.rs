//! Audit-chain test for `support_cases`.
//!
//! The support-case create + update handlers must emit an audit_log
//! event for every write. Without it a `boss-rebuild-all` cycle would
//! lose every support case (TRUNCATE accounts CASCADE wipes
//! support_cases via the FK on account_id, with nothing to
//! repopulate it).
//!
//! This test exercises POST /support-cases (open) + PUT /
//! support-cases/{id} (status flip + assignee), drops the
//! projection, runs rebuild_accounts, and asserts both rows
//! reappear with the post-PUT field values intact.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_accounts::accounts::accounts_router;
use boss_accounts::rebuild_accounts;
use boss_accounts::support_cases::support_cases_router;
use boss_assets_client::FakeAssetsClient;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::NaiveDate;
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct CaseRow {
    id: String,
    account_id: String,
    status: String,
    assignee_id: Option<String>,
    resolved_on: Option<NaiveDate>,
    csat: Option<i16>,
}

async fn snapshot_cases(pool: &PgPool) -> Vec<CaseRow> {
    sqlx::query_as(
        "SELECT id, account_id, status, assignee_id, resolved_on, csat \
         FROM support_cases ORDER BY id",
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
    .merge(support_cases_router(
        pool,
        Some(publisher),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
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
async fn support_cases_survive_rebuild() {
    let db = TestDb::new().await;
    seed_employee(&db.pool, "emp-rep-001").await;
    let app = build_app(db.pool.clone()).await;

    // Parent account.
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

    // Open the case.
    TestRequest::post("/api/people/support-cases")
        .json(&serde_json::json!({
            "id": "sc-001",
            "account_id": "acc-001",
            "channel": "phone",
            "category": "billing",
            "subject": "Disputed invoice",
            "body": "Customer disputes the latest invoice line item.",
            "opened_on": "2026-05-04",
            "assignee_id": null,
            "status": "open",
            "resolved_on": null,
            "resolution_notes": null,
            "csat": null,
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Resolve it: assign + close + CSAT.
    TestRequest::put("/api/people/support-cases/sc-001")
        .json(&serde_json::json!({
            "status": "resolved",
            "assignee_id": "emp-rep-001",
            "resolved_on": "2026-05-04",
            "resolution_notes": "Adjustment posted, customer satisfied",
            "csat": 5,
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    let pre = snapshot_cases(&db.pool).await;
    assert_eq!(pre.len(), 1);
    assert_eq!(pre[0].status, "resolved");
    assert_eq!(pre[0].assignee_id.as_deref(), Some("emp-rep-001"));
    assert_eq!(pre[0].csat, Some(5));

    let report = rebuild_accounts(&db.pool).await.expect("rebuild");
    assert!(
        report.support_cases_opened >= 1,
        "rebuild should reproduce the open event, got {report:?}"
    );
    assert!(
        report.support_cases_updated >= 1,
        "rebuild should replay the update, got {report:?}"
    );

    let post = snapshot_cases(&db.pool).await;
    assert_eq!(
        pre, post,
        "support_cases must round-trip exactly through audit_log"
    );
}
