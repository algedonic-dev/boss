//! Invariant: deleting an account is blocked when any of its systems
//! have open service tickets.
//!
//! These tests run against a real Postgres database via TestDb. The
//! account delete handler lives in `accounts.rs` which talks to the DB
//! directly (no repository trait), so we can't substitute an
//! in-memory adapter — we need a real schema to exercise the full
//! path. The AssetsClient is swapped for a `FakeAssetsClient` so the
//! tests don't need a live assets service.
//!
//! Contract enforced by these tests:
//!   - DELETE /api/people/accounts/{id} → 204 when assets reports 0 tickets
//!   - DELETE /api/people/accounts/{id} → 409 when assets reports any
//!   - DELETE /api/people/accounts/{id} → 503 when assets is unreachable
//!     (fail closed — never delete without verified safety)
//!   - DELETE /api/people/accounts/{id} → 404 for unknown account id
//!   - The guard fires BEFORE the SQL delete: we verify the account row
//!     is still present on 409, and absent on 204.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_accounts::accounts::accounts_router;
use boss_assets_client::{AssetsClient, FakeAssetsClient};
use boss_testing::{TestDb, TestRequest};
use chrono::NaiveDate;
use sqlx::PgPool;

async fn seed_account(pool: &PgPool, id: &str) {
    sqlx::query(
        "INSERT INTO accounts (id, name, director, city, state, tier, customer_since, territory_rep_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(format!("{id} Account"))
    .bind("Dr. Tester")
    .bind("Testville")
    .bind("TX")
    .bind("gold")
    .bind(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
    .bind("emp-001")
    .execute(pool)
    .await
    .expect("insert account");
}

async fn account_exists(pool: &PgPool, id: &str) -> bool {
    let (exists,): (bool,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM accounts WHERE id = $1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("exists query");
    exists
}

fn build_router(pool: PgPool, client: Arc<dyn AssetsClient>) -> Router {
    accounts_router(
        pool,
        None,
        client,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    )
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_account_with_no_open_tickets_returns_204_and_removes_row() {
    let db = TestDb::new().await;
    seed_account(&db.pool, "account-happy").await;

    let assets: Arc<dyn AssetsClient> = Arc::new(FakeAssetsClient::with_count(0));
    let router = build_router(db.pool.clone(), assets);

    let resp = TestRequest::delete("/api/people/accounts/account-happy")
        .send(&router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
    assert!(
        !account_exists(&db.pool, "account-happy").await,
        "account row should be removed after successful delete"
    );
}

#[tokio::test]
async fn delete_account_with_open_tickets_returns_409_and_preserves_row() {
    let db = TestDb::new().await;
    seed_account(&db.pool, "account-busy").await;

    let fake = Arc::new(FakeAssetsClient::with_count(3));
    let router = build_router(db.pool.clone(), fake.clone());

    let resp = TestRequest::delete("/api/people/accounts/account-busy")
        .send(&router)
        .await;

    resp.assert_status(StatusCode::CONFLICT);
    assert!(
        account_exists(&db.pool, "account-busy").await,
        "account row must be preserved when guard rejects the delete"
    );

    let body = resp.body_text();
    assert!(
        body.contains("open service ticket"),
        "conflict message should explain why; got: {body}"
    );

    assert_eq!(
        fake.calls(),
        vec!["account-busy".to_string()],
        "guard should have queried assets exactly once with the right account id"
    );
}

#[tokio::test]
async fn delete_account_when_assets_unreachable_returns_503_and_preserves_row() {
    let db = TestDb::new().await;
    seed_account(&db.pool, "account-isolated").await;

    let assets: Arc<dyn AssetsClient> =
        Arc::new(FakeAssetsClient::unreachable("connection refused"));
    let router = build_router(db.pool.clone(), assets);

    let resp = TestRequest::delete("/api/people/accounts/account-isolated")
        .send(&router)
        .await;

    resp.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        account_exists(&db.pool, "account-isolated").await,
        "account row must be preserved when the guard can't verify safety"
    );

    let body = resp.body_text();
    assert!(
        body.contains("assets unreachable"),
        "503 response should say why; got: {body}"
    );
}

#[tokio::test]
async fn delete_account_that_does_not_exist_returns_404() {
    let db = TestDb::new().await;
    // No seed — the account simply doesn't exist.
    let assets: Arc<dyn AssetsClient> = Arc::new(FakeAssetsClient::with_count(0));
    let router = build_router(db.pool.clone(), assets);

    let resp = TestRequest::delete("/api/people/accounts/account-nope")
        .send(&router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guard_queries_assets_before_touching_database() {
    // This test drives home the ordering: even if the DB delete would
    // succeed (the account exists), the guard MUST run first. We prove
    // this by reporting tickets from assets — the account should stay
    // put because the guard rejected before the DELETE statement ran.
    let db = TestDb::new().await;
    seed_account(&db.pool, "account-order-check").await;

    let fake = Arc::new(FakeAssetsClient::with_count(1));
    let router = build_router(db.pool.clone(), fake.clone());

    TestRequest::delete("/api/people/accounts/account-order-check")
        .send(&router)
        .await
        .assert_status(StatusCode::CONFLICT);

    assert!(
        account_exists(&db.pool, "account-order-check").await,
        "account must still exist after guard-blocked delete"
    );
    assert_eq!(fake.calls().len(), 1, "guard should have been called once");
}
