//! End-to-end: drive accounts writes through the API, snapshot
//! `accounts` + `account_contacts`, drop them, rebuild from
//! `audit_log`, assert exact match.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_accounts::accounts::accounts_router;
use boss_accounts::rebuild_accounts;
use boss_assets_client::FakeAssetsClient;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_testing::{TestDb, TestRequest};
use chrono::NaiveDate;
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct AccountRow {
    id: String,
    name: String,
    director: String,
    city: String,
    state: String,
    tier: String,
    customer_since: NaiveDate,
    territory_rep_id: String,
    account_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct ContactRow {
    id: String,
    account_id: String,
    name: String,
    role: String,
    email: String,
    phone: Option<String>,
    is_primary: bool,
}

async fn snapshot_accounts(pool: &PgPool) -> Vec<AccountRow> {
    sqlx::query_as("SELECT id, name, director, city, state, tier, customer_since, territory_rep_id, account_type FROM accounts ORDER BY id")
        .fetch_all(pool).await.unwrap()
}

async fn snapshot_contacts(pool: &PgPool) -> Vec<ContactRow> {
    sqlx::query_as("SELECT id, account_id, name, role, email, phone, is_primary FROM account_contacts ORDER BY id")
        .fetch_all(pool).await.unwrap()
}

async fn build_app(pool: PgPool) -> Router {
    let publisher = DomainPublisher::new(boss_testing::RecordingEventBus::new(), "accounts")
        .with_audit(Arc::new(PgAuditWriter::new(pool.clone())));
    accounts_router(
        pool,
        Some(publisher),
        Arc::new(FakeAssetsClient::with_count(0)),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    )
}

/// Create the territory-rep employee that the FK on
/// `account_team_members.employee_id` requires.
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
         VALUES ($1, 'Pat Tester', $2, 'territory-rep', 'sales', '2024-01-15', 'loc-test', 'full-time', 'active', NULL) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(format!("{id}@boss.example"))
    .execute(pool)
    .await
    .unwrap();
}

fn body(
    id: &str,
    name: &str,
    account_type: &str,
    contacts: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": name,
        "director": "Dr. Test",
        "city": "Austin",
        "state": "TX",
        "tier": "gold",
        "customer_since": "2025-06-01",
        "territory_rep_id": "emp-rep-001",
        "account_type": account_type,
        "contacts": contacts,
    })
}

fn contact(id: &str, account: &str, role: &str, primary: bool) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "account_id": account,
        "name": format!("Contact {id}"),
        "role": role,
        "email": format!("{id}@example.com"),
        "phone": "555-0100",
        "is_primary": primary,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_accounts_and_contacts() {
    let db = TestDb::new().await;
    seed_employee(&db.pool, "emp-rep-001").await;
    let app = build_app(db.pool.clone()).await;

    // 1. Create two accounts, each with two contacts.
    TestRequest::post("/api/people/accounts")
        .json(&body(
            "acc-001",
            "Hopswell Brewing",
            "wholesale-distributor",
            serde_json::json!([
                contact("contact-001", "acc-001", "buyer", true),
                contact("contact-002", "acc-001", "accounts-payable", false),
            ]),
        ))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::post("/api/people/accounts")
        .json(&body(
            "acc-002",
            "Maltworks Co.",
            "bar-restaurant",
            serde_json::json!([contact("contact-003", "acc-002", "buyer", true)]),
        ))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // 2. Update acc-001 — change name and contact list.
    TestRequest::put("/api/people/accounts/acc-001")
        .json(&body(
            "acc-001",
            "Hopswell Brewing (Renamed)",
            "wholesale-distributor",
            serde_json::json!([contact("contact-099", "acc-001", "buyer", true)]),
        ))
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // 3. Snapshot.
    let accounts_before = snapshot_accounts(&db.pool).await;
    let contacts_before = snapshot_contacts(&db.pool).await;
    assert_eq!(accounts_before.len(), 2);
    assert_eq!(contacts_before.len(), 2, "1 from acc-001 + 1 from acc-002");
    assert_eq!(accounts_before[0].name, "Hopswell Brewing (Renamed)");
    assert_eq!(accounts_before[0].account_type, "wholesale-distributor");

    // 4. Verify audit_log has the events.
    let event_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE kind LIKE 'accounts.account.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    // 2 created + 1 updated = 3 account events, plus 3 territory-rep
    // mirror events (one per create/update via mirror_territory_rep)
    // = 6 total. The team-assigned events ship with each account
    // mutation as of 2026-05-04 to keep `account_team_members`
    // rebuildable from audit_log alone.
    assert_eq!(event_count.0, 6, "got {} events", event_count.0);

    // 5. Wipe + rebuild.
    sqlx::query("DELETE FROM account_contacts")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM accounts")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = rebuild_accounts(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.accounts_upserted, 3, "2 created + 1 updated");

    // 6. Reconstructed projections must match originals exactly.
    let accounts_after = snapshot_accounts(&db.pool).await;
    let contacts_after = snapshot_contacts(&db.pool).await;
    assert_eq!(accounts_before, accounts_after, "accounts mismatch");
    assert_eq!(contacts_before, contacts_after, "account_contacts mismatch");
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_handles_account_delete() {
    let db = TestDb::new().await;
    seed_employee(&db.pool, "emp-rep-001").await;
    let app = build_app(db.pool.clone()).await;

    TestRequest::post("/api/people/accounts")
        .json(&body(
            "acc-doomed",
            "Doomed Inc",
            "unspecified",
            serde_json::json!([]),
        ))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::delete("/api/people/accounts/acc-doomed")
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM accounts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);

    let report = rebuild_accounts(&db.pool).await.unwrap();
    assert!(report.accounts_upserted >= 1);
    assert!(report.accounts_deleted >= 1);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM accounts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0, "rebuild should reproduce post-delete state");
}
