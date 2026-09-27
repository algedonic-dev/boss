//! A ledger write names its author from the actor that SIGNED it, never
//! from the request body.
//!
//! Two backlog items, one class (7bf42e2b, 975c228f). Measured 2026-09-27
//! at origin/main 756da393: the manual-entry handler read `created_by`
//! from its body (defaulting `admin`), the COGS handler likewise
//! (defaulting `ledger`), the period lock read `locked_by` (the finance
//! page sent the literal `operator`, which the Periods table rendered as
//! "Locked by"), and the yearly close read `closed_by` into the same
//! column — so any caller could name any author on a financial fact or a
//! lock, and the Reverse button, which sent `created_by: null`, credited
//! every reversal to `admin`. The events those writes staged already
//! carried the true actor; the rows and the fact payloads did not.
//!
//! The rule is the one the jobs API applies to a packet's `opened_by`:
//! the author is the signed caller (`User::ambient_actor`, the actor the
//! write's own event is stamped with), a body naming ANOTHER author is
//! refused 422 before anything is written, and a body naming the signer
//! is admitted.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_ledger::http::{LedgerApiState, router};
use boss_ledger::rebuild_facts;
use boss_testing::{RecordingEventBus, TestDb};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn build_router(db: &TestDb) -> Router {
    router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        // The read gate is not this test's subject (tests/the_ledger_read_gate.rs).
        policy: Arc::new(boss_policy_client::PermissivePolicyClient),
    })
}

/// POST as `signer` — the `X-Boss-User` the gateway builds for a real
/// session — or anonymously when `signer` is `None`.
async fn post_as(
    db: &TestDb,
    signer: Option<&str>,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(id) = signer {
        let user = json!({
            "id": id,
            "role": "admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": null,
        });
        req = req.header("x-boss-user", user.to_string());
    }
    let resp = build_router(db)
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, parsed)
}

async fn drain(db: &TestDb) {
    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
}

async fn staged(db: &TestDb, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind = $1")
        .bind(kind)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

type FactRow = (String, Option<String>, Option<String>, String, Value);

async fn fact_rows(db: &TestDb, kind: &str) -> Vec<FactRow> {
    sqlx::query_as(
        "SELECT kind, source_table, source_id, created_by, payload \
         FROM financial_facts WHERE kind = $1 ORDER BY source_id",
    )
    .bind(kind)
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

fn manual_entry(extra: Value) -> Value {
    let mut body = json!({
        "posted_on": "2026-03-15",
        "memo": "Q1 rent accrual",
        "lines": [
            {"account_code": "6200", "debit_cents": 250_000},
            {"account_code": "2100", "credit_cents": 250_000},
        ],
    });
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        b.extend(e.clone());
    }
    body
}

fn cogs(source_id: &str, extra: Value) -> Value {
    let mut body = json!({
        "total_cost_cents": 12_345i64,
        "happened_on": "2026-05-12",
        "source_table": "jobs_step",
        "source_id": source_id,
    });
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        b.extend(e.clone());
    }
    body
}

async fn create_year(db: &TestDb, year: i32) -> String {
    let (status, body) = post_as(
        db,
        Some("emp-cfo"),
        "/api/ledger/periods",
        json!({"year": year}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create period: {body}");
    body["id"].as_str().expect("period id").to_string()
}

/// Revenue inside `year`, so the yearly close has something to close.
async fn book_revenue(db: &TestDb, year: i32) {
    let body = json!({
        "posted_on": format!("{year}-06-15"),
        "lines": [
            {"account_code": "1000", "debit_cents": 5_000},
            {"account_code": "4100", "credit_cents": 5_000},
        ],
    });
    let (status, resp) = post_as(db, Some("emp-cfo"), "/api/ledger/journal-entries", body).await;
    assert_eq!(status, StatusCode::OK, "book revenue: {resp}");
}

async fn period_lock(db: &TestDb, id: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT status, locked_by FROM gl_periods WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

// --- manual journal entry (7bf42e2b; the Reverse button, 975c228f) ---------

/// The author is the signer, in the fact row AND the payload the
/// `ledger.manual_entry.submitted` event carries — and a log-rooted
/// rebuild (the rule reads `/created_by`) reproduces the live fact.
#[tokio::test(flavor = "multi_thread")]
async fn a_manual_entry_is_authored_by_its_signer_and_rebuilds_so() {
    let db = TestDb::new().await;
    let (status, body) = post_as(
        &db,
        Some("emp-cfo"),
        "/api/ledger/journal-entries",
        manual_entry(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manual entry: {body}");

    let live = fact_rows(&db, "finance.manual.entry").await;
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].3, "emp-cfo", "the fact's author is the signer");
    assert_eq!(
        live[0].4["created_by"], "emp-cfo",
        "and so is the payload's"
    );

    drain(&db).await;
    rebuild_facts(&db.pool).await.unwrap();
    assert_eq!(fact_rows(&db, "finance.manual.entry").await, live);
}

/// The Reverse button's shape: `created_by: null` is no claim at all,
/// so the reversal is credited to whoever pressed it — not to `admin`.
#[tokio::test(flavor = "multi_thread")]
async fn a_reversal_sent_with_a_null_author_is_credited_to_its_signer() {
    let db = TestDb::new().await;
    let (status, body) = post_as(
        &db,
        Some("emp-david"),
        "/api/ledger/journal-entries",
        manual_entry(json!({"created_by": null, "memo": "Reverses entry x"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reversal: {body}");
    assert_eq!(
        fact_rows(&db, "finance.manual.entry").await[0].3,
        "emp-david"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manual_entry_naming_another_author_is_refused_and_writes_nothing() {
    let db = TestDb::new().await;
    let (status, body) = post_as(
        &db,
        Some("emp-cfo"),
        "/api/ledger/journal-entries",
        manual_entry(json!({"created_by": "emp-david"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["refused_keys"], json!(["created_by"]), "{body}");
    assert_eq!(body["created_by"], "emp-david");
    assert_eq!(body["signed_as"], "emp-cfo");
    assert!(fact_rows(&db, "finance.manual.entry").await.is_empty());
    assert_eq!(staged(&db, "ledger.manual_entry.submitted").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manual_entry_naming_its_own_signer_is_admitted() {
    let db = TestDb::new().await;
    let (status, body) = post_as(
        &db,
        Some("emp-cfo"),
        "/api/ledger/journal-entries",
        manual_entry(json!({"created_by": "emp-cfo"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(fact_rows(&db, "finance.manual.entry").await[0].3, "emp-cfo");
}

/// An unsigned write is credited to the same actor its event is stamped
/// with (`automation:platform`), never to a word the body chose.
#[tokio::test(flavor = "multi_thread")]
async fn an_unsigned_manual_entry_is_credited_as_its_event_is() {
    let db = TestDb::new().await;
    let (status, body) = post_as(
        &db,
        None,
        "/api/ledger/journal-entries",
        manual_entry(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        fact_rows(&db, "finance.manual.entry").await[0].3,
        "automation:platform"
    );
}

// --- COGS recognition (7bf42e2b) --------------------------------------------

/// A service caller signs as an automation; the fact, its payload and
/// the rebuild all read that name.
#[tokio::test(flavor = "multi_thread")]
async fn a_cogs_fact_is_authored_by_its_signer_and_rebuilds_so() {
    let db = TestDb::new().await;
    let (status, body) = post_as(
        &db,
        Some("brewery-sim"),
        "/api/ledger/cogs-recognized",
        cogs("step-cogs-a", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cogs: {body}");

    let live = fact_rows(&db, "finance.cogs.recognized").await;
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].3, "automation:brewery-sim");
    assert_eq!(live[0].4["created_by"], "automation:brewery-sim");

    drain(&db).await;
    rebuild_facts(&db.pool).await.unwrap();
    assert_eq!(fact_rows(&db, "finance.cogs.recognized").await, live);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cogs_post_naming_another_author_is_refused_and_writes_nothing() {
    let db = TestDb::new().await;
    let (status, body) = post_as(
        &db,
        Some("brewery-sim"),
        "/api/ledger/cogs-recognized",
        cogs("step-cogs-b", json!({"created_by": "ledger"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["signed_as"], "automation:brewery-sim");
    assert!(fact_rows(&db, "finance.cogs.recognized").await.is_empty());
    assert_eq!(staged(&db, "ledger.cogs.recognized").await, 0);
}

// --- period lock and yearly close (975c228f) --------------------------------

/// The lock the Periods table renders as "Locked by" names the signer,
/// and the `ledger.period.locked` event agrees with the row.
#[tokio::test(flavor = "multi_thread")]
async fn a_period_lock_is_recorded_as_its_signer() {
    let db = TestDb::new().await;
    let id = create_year(&db, 2097).await;
    let (status, body) = post_as(
        &db,
        Some("emp-cfo"),
        &format!("/api/ledger/periods/{id}/lock"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "lock: {body}");
    assert_eq!(
        period_lock(&db, &id).await,
        ("locked".to_string(), Some("emp-cfo".to_string()))
    );
    let payload: Value = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind = 'ledger.period.locked' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload["locked_by"], "emp-cfo");
}

/// The finance page's old shape: a literal `operator` in the body names
/// someone other than the signer, so the lock is refused and the period
/// stays open.
#[tokio::test(flavor = "multi_thread")]
async fn a_period_lock_naming_another_locker_is_refused_and_leaves_it_open() {
    let db = TestDb::new().await;
    let id = create_year(&db, 2096).await;
    let (status, body) = post_as(
        &db,
        Some("emp-cfo"),
        &format!("/api/ledger/periods/{id}/lock"),
        json!({"locked_by": "operator"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["refused_keys"], json!(["locked_by"]), "{body}");
    assert_eq!(period_lock(&db, &id).await, ("open".to_string(), None));
    assert_eq!(staged(&db, "ledger.period.locked").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_yearly_close_naming_another_closer_is_refused_and_leaves_it_open() {
    let db = TestDb::new().await;
    let id = create_year(&db, 2095).await;
    book_revenue(&db, 2095).await;
    let (status, body) = post_as(
        &db,
        Some("emp-cfo"),
        &format!("/api/ledger/periods/{id}/close"),
        json!({"closed_by": "emp-david"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["refused_keys"], json!(["closed_by"]), "{body}");
    assert_eq!(period_lock(&db, &id).await, ("open".to_string(), None));
    assert_eq!(staged(&db, "ledger.period.closed").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_yearly_close_is_recorded_as_its_signer() {
    let db = TestDb::new().await;
    let id = create_year(&db, 2094).await;
    book_revenue(&db, 2094).await;
    let (status, body) = post_as(
        &db,
        Some("emp-cfo"),
        &format!("/api/ledger/periods/{id}/close"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "close: {body}");
    assert_eq!(
        period_lock(&db, &id).await,
        ("locked".to_string(), Some("emp-cfo".to_string()))
    );
}
