//! Locking or reopening an accounting period asks policy for it
//! (backlog 25a4f7f9, 2026-09-28).
//!
//! Measured at origin/main 07977fe7: `POST /api/ledger/periods/{id}/lock`
//! and `/unlock` checked `reject_if_auditor` and nothing else, so the
//! only grant they needed was the router-wide `ledger` READ — every
//! bookkeeper, AP and AR clerk and analyst could close a month or reopen
//! one. Closing a month is a mutation of record (it freezes a checksum
//! over the period's entries), and reopening one lets history be
//! rewritten, so each is now asked of policy on its own resource,
//! `ledger-period`, through the registry-write ladder
//! (`boss_policy_client::writes::require_registry_write`): no caller is
//! 401, a deny or a grant narrower than `all` is 403. Lock is `close`,
//! reopen is `update` — two actions, so a tenant can let one role close
//! months while reopening one stays with another.
//!
//! The refusals need no database — the gate answers before the handler
//! touches the pool, so the pool is a lazy one that is never connected,
//! and a call that PASSES the gate is seen as the storage error that
//! pool gives (500), never as a 401 or 403. The admitted path, with the
//! row actually locked and reopened, is the last test, on a TestDb.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_ledger::http::{LedgerApiState, router};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use serde_json::{Value, json};
use tower::ServiceExt;

const PERIOD: &str = "00000000-0000-0000-0000-000000000000";

fn surface(policy: Arc<dyn PolicyClient>) -> axum::Router {
    // Never connected: a refusal answers before a handler touches it,
    // and a call the gate admits gives up on it in a second rather than
    // the pool's default thirty.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(1))
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap();
    router(LedgerApiState {
        pool,
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        policy,
    })
}

fn signed(id: &str, role: &str) -> Value {
    json!({
        "id": id,
        "role": role,
        "access_tier": "user",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "finance",
    })
}

async fn post(app: axum::Router, path: &str, user: Option<Value>) -> (StatusCode, String) {
    post_body(app, path, user, json!({})).await
}

async fn post_body(
    app: axum::Router,
    path: &str,
    user: Option<Value>,
    body: Value,
) -> (StatusCode, String) {
    let mut req = Request::post(path).header("content-type", "application/json");
    if let Some(u) = user {
        req = req.header("x-boss-user", u.to_string());
    }
    let resp = app
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn lock() -> String {
    format!("/api/ledger/periods/{PERIOD}/lock")
}

fn unlock() -> String {
    format!("/api/ledger/periods/{PERIOD}/unlock")
}

/// The year-end close locks a year, so it asks what locking a month
/// asks (the review of this car: it checked `reject_if_auditor` alone).
fn close() -> String {
    format!("/api/ledger/periods/{PERIOD}/close")
}

/// A call the gate ADMITTED meets the never-connected pool: the exact
/// answer is its 500 and the pool's timeout, not merely "not 401/403",
/// which a 400 or a 404 from some other refusal would also satisfy.
fn assert_passed_the_gate(status: StatusCode, body: &str, what: &str) {
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{what}: expected the gate to admit and the lazy pool to fail, got {status}: {body}"
    );
    assert!(
        body.contains("pool timed out"),
        "{what}: expected the lazy pool's storage error, got: {body}"
    );
}

/// A finance reader: `ledger` read, which is what every finance role in
/// the brewery seed holds — and all the two doors asked for until now.
fn reader_policy() -> FakePolicyClient {
    FakePolicyClient::builder()
        .allow("bookkeeper", Action::Read, Resource::ledger(), Scope::All)
        .build()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ledger_reader_may_not_close_or_reopen_a_period() {
    let app = surface(Arc::new(reader_policy()));
    for path in [lock(), unlock(), close()] {
        let (status, body) =
            post(app.clone(), &path, Some(signed("emp-books", "bookkeeper"))).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{path} admitted a caller holding only a ledger read grant: {body}"
        );
    }
}

/// No caller, no write: the lock names its locker and the event its
/// actor, and an unsigned request has neither. It used to lock as
/// `automation:platform`.
#[tokio::test(flavor = "multi_thread")]
async fn an_unsigned_close_or_reopen_is_refused() {
    let app = surface(Arc::new(boss_policy_client::PermissivePolicyClient));
    for path in [lock(), unlock(), close()] {
        let (status, body) = post(app.clone(), &path, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {body}");
    }
}

/// A period belongs to no person and no department, so a grant narrower
/// than `all` cannot be shown to reach it.
#[tokio::test(flavor = "multi_thread")]
async fn a_department_grant_on_periods_reaches_no_period() {
    let dept = Scope::Department("finance".into());
    let policy = FakePolicyClient::builder()
        .allow("controller", Action::Read, Resource::ledger(), Scope::All)
        .allow(
            "controller",
            Action::Close,
            Resource::ledger_period(),
            dept.clone(),
        )
        .allow(
            "controller",
            Action::Update,
            Resource::ledger_period(),
            dept,
        )
        .build();
    let app = surface(Arc::new(policy));
    for path in [lock(), unlock(), close()] {
        let (status, body) = post(app.clone(), &path, Some(signed("emp-ctl", "controller"))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
    }
}

/// Lock is `close` and reopen is `update`: a role granted closing alone
/// passes the lock's gate (and meets the never-connected pool) while the
/// reopen is refused.
#[tokio::test(flavor = "multi_thread")]
async fn closing_a_month_and_reopening_one_are_two_grants() {
    let policy = FakePolicyClient::builder()
        .allow("bookkeeper", Action::Read, Resource::ledger(), Scope::All)
        .allow(
            "bookkeeper",
            Action::Close,
            Resource::ledger_period(),
            Scope::All,
        )
        .build();
    let app = surface(Arc::new(policy));
    let who = || Some(signed("emp-books", "bookkeeper"));
    for path in [lock(), close()] {
        let (status, body) = post(app.clone(), &path, who()).await;
        assert_passed_the_gate(status, &body, &format!("a close grant on {path}"));
    }
    let (status, body) = post(app, &unlock(), who()).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "reopening needs update: {body}"
    );
}

/// Judged against the grants that SHIP, not against ones written for
/// the test: the deploy superuser passes both gates, and the external
/// auditor role — which reads the whole ledger — passes neither.
#[tokio::test(flavor = "multi_thread")]
async fn the_shipped_defaults_admit_platform_admin_and_not_the_auditor() {
    let app = surface(Arc::new(
        FakePolicyClient::builder().with_default_rules().build(),
    ));
    for path in [lock(), unlock(), close()] {
        let (status, body) = post(
            app.clone(),
            &path,
            Some(signed("emp-david", "platform-admin")),
        )
        .await;
        assert_passed_the_gate(status, &body, &format!("platform-admin on {path}"));
        let (status, body) = post(
            app.clone(),
            &path,
            Some(signed("emp-audit", "audit-readonly")),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
    }
}

/// The admitted path end to end: a caller granted both actions locks
/// the period and reopens it, and the row says so each time.
#[tokio::test(flavor = "multi_thread")]
async fn a_granted_caller_closes_and_reopens_a_period() {
    let db = boss_testing::TestDb::new().await;
    let policy = FakePolicyClient::builder()
        .allow("controller", Action::Read, Resource::ledger(), Scope::All)
        .allow(
            "controller",
            Action::Create,
            Resource::ledger_period(),
            Scope::All,
        )
        .allow(
            "controller",
            Action::Close,
            Resource::ledger_period(),
            Scope::All,
        )
        .allow(
            "controller",
            Action::Update,
            Resource::ledger_period(),
            Scope::All,
        )
        .build();
    let app = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        policy: Arc::new(policy),
    });
    let who = || Some(signed("emp-ctl", "controller"));
    // Creating the year is Create on `ledger-period` too (backlog
    // 34f0a954), granted above: one resource covers a year's whole life.
    let (status, body) = post_body(
        app.clone(),
        "/api/ledger/periods",
        who(),
        json!({"year": 2091}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let status_of = || async {
        sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, locked_by FROM gl_periods WHERE id = $1::uuid",
        )
        .bind(&id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
    };

    let (status, body) = post(
        app.clone(),
        &format!("/api/ledger/periods/{id}/lock"),
        who(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "lock: {body}");
    assert_eq!(
        status_of().await,
        ("locked".to_string(), Some("emp-ctl".to_string()))
    );

    let (status, body) = post(app, &format!("/api/ledger/periods/{id}/unlock"), who()).await;
    assert_eq!(status, StatusCode::OK, "unlock: {body}");
    assert_eq!(status_of().await.0, "open");
}
