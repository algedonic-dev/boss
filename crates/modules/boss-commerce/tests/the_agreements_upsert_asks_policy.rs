//! `POST /api/commerce/agreements` asks policy, and its fact is signed
//! by the caller (backlog d2bea664, 2026-09-27).
//!
//! The adversarial review of 6c0f6547 found the agreements router
//! taking no `CurrentUser` and asking no policy: any caller reaching
//! the port could write any account's service agreement, and the
//! `commerce.service_agreement.upserted` fact it staged was signed by
//! the publisher's default actor rather than whoever sent it. The door
//! is an upsert — a new id creates, a known one moves status, end date
//! and value — so it asks Create AND Update on `agreement`, at scope
//! `all`, before it writes anything.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_commerce::agreements::agreements_router;
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{TestDb, TestRequest};
use sqlx::PgPool;

const ROLE: &str = "contracts-desk";

fn app(pool: PgPool, policy: Arc<dyn PolicyClient>) -> Router {
    agreements_router(
        pool,
        None,
        Arc::new(boss_clock_client::WallClockClient),
        policy,
    )
}

fn grant(rules: &[(Action, Resource, Scope)]) -> Arc<dyn PolicyClient> {
    Arc::new(
        rules
            .iter()
            .fold(FakePolicyClient::builder(), |b, r| {
                b.allow(ROLE, r.0, r.1.clone(), r.2.clone())
            })
            .build(),
    )
}

fn agreement(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "account_id": "acc-001",
        "agreement_type": "full-service",
        "status": "active",
        "start_date": "2026-01-01",
        "end_date": "2026-12-31",
        "annual_value_cents": 1_200_000,
        "currency": "USD",
        "billing_frequency": "monthly",
        "auto_renew": true,
        "covers_parts": true,
        "covers_labor": true,
        "covers_travel": false,
        "pm_visits_per_year": 4,
        "response_sla_hours": 8,
        "owner_id": "emp-rep-001",
    })
}

/// (rows in service_agreements, facts staged on the outbox) for `id`.
async fn left_behind(pool: &PgPool, id: &str) -> (i64, i64) {
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM service_agreements WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox \
         WHERE kind = 'commerce.service_agreement.upserted' AND payload->>'id' = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    (rows, facts)
}

async fn post_as_desk(app: &Router, id: &str) -> StatusCode {
    TestRequest::post("/api/commerce/agreements")
        .as_user("emp-desk", ROLE)
        .json(&agreement(id))
        .send(app)
        .await
        .status
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_caller_writes_no_agreement_and_stages_no_fact() {
    let db = TestDb::new().await;
    let app = app(db.pool.clone(), Arc::new(FakePolicyClient::deny_all()));
    assert_eq!(post_as_desk(&app, "sa-denied").await, StatusCode::FORBIDDEN);
    assert_eq!(left_behind(&db.pool, "sa-denied").await, (0, 0));
}

/// Create alone does not open an upsert: the same door moves a known
/// agreement's status and value, which is an Update.
#[tokio::test(flavor = "multi_thread")]
async fn create_alone_does_not_open_the_upsert() {
    let db = TestDb::new().await;
    let app = app(
        db.pool.clone(),
        grant(&[(Action::Create, Resource::agreement(), Scope::All)]),
    );
    assert_eq!(post_as_desk(&app, "sa-create").await, StatusCode::FORBIDDEN);
    assert_eq!(left_behind(&db.pool, "sa-create").await, (0, 0));
}

/// Both verbs on the WRONG resource, or on agreement below scope all,
/// open nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_grant_on_invoice_or_below_scope_all_does_not_open_the_upsert() {
    let db = TestDb::new().await;
    for (n, rules) in [
        vec![
            (Action::Create, Resource::invoice(), Scope::All),
            (Action::Update, Resource::invoice(), Scope::All),
        ],
        vec![
            (Action::Create, Resource::agreement(), Scope::Territory),
            (Action::Update, Resource::agreement(), Scope::Territory),
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("sa-wrong-{n}");
        let app = app(db.pool.clone(), grant(&rules));
        assert_eq!(
            post_as_desk(&app, &id).await,
            StatusCode::FORBIDDEN,
            "{rules:?}"
        );
        assert_eq!(left_behind(&db.pool, &id).await, (0, 0), "{rules:?}");
    }
}

/// Granted both verbs at scope all, the upsert lands, and its fact is
/// signed by the caller — not the publisher's default actor.
#[tokio::test(flavor = "multi_thread")]
async fn a_granted_caller_writes_the_agreement_and_signs_its_fact() {
    let db = TestDb::new().await;
    let app = app(
        db.pool.clone(),
        grant(&[
            (Action::Create, Resource::agreement(), Scope::All),
            (Action::Update, Resource::agreement(), Scope::All),
        ]),
    );
    assert_eq!(post_as_desk(&app, "sa-granted").await, StatusCode::CREATED);
    assert_eq!(left_behind(&db.pool, "sa-granted").await, (1, 1));
    let actor: String = sqlx::query_scalar(
        "SELECT payload->>'_actor' FROM event_outbox \
         WHERE kind = 'commerce.service_agreement.upserted' AND payload->>'id' = 'sa-granted'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(actor, "emp-desk");
}
