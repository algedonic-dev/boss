//! A ledger write and the fact that records it commit together, or
//! neither commits.
//!
//! Two backlog items, one guarantee (both found by the design 3036296f
//! mechanism A pin, whose allowlist marked them `gap`):
//!
//! - de926d87 — `POST /api/ledger/cogs-recognized` wrote a
//!   `finance.cogs.recognized` fact and its journal entry and staged NO
//!   ledger event, where its sibling fact handlers (manual entry,
//!   inventory transferred / capitalized) each stage one. A
//!   TRUNCATE-then-replay rebuild of `financial_facts` from `audit_log`
//!   therefore lost every COGS entry the endpoint ever posted.
//! - 016a2763 — `settle_one` flipped a `bank_settlements` row to
//!   `settled`, and both payroll handlers wrote the `payroll_runs` row
//!   and its lines, ON THE POOL, committed, and only then opened the
//!   transaction that records the fact, the journal entry and the
//!   event. Any failure in that second transaction (a locked period is
//!   the ordinary one; a crash is the other) left a settled row or a
//!   payroll run with no fact behind it, which the next log-rooted
//!   rebuild silently drops.
//!
//! The failure injected between the write and its fact is a locked
//! accounting period: `post_fact_in_tx` refuses it, which is exactly
//! the step that sits between the projection write and the event.

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
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

fn build_router(db: &TestDb) -> Router {
    router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        // The read gate is not this test's subject (tests/the_ledger_read_gate.rs).
        policy: Arc::new(boss_policy_client::PermissivePolicyClient),
    })
}

/// The caller every write here is signed as: a ledger write names its
/// caller, and an unsigned one is refused 401 (backlog 34f0a954). The
/// gate is not this file's subject (tests/a_ledger_write_asks_policy.rs).
const SIGNER: &str = r#"{"id":"emp-controller","role":"controller","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"finance"}"#;

async fn post(db: &TestDb, path: &str, body: Value) -> (StatusCode, String) {
    let resp = build_router(db)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("x-boss-user", SIGNER)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

async fn drain(db: &TestDb) {
    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
}

/// Lock the month that owns `day` — the period-lock refusal in
/// `post_fact_in_tx` is the failure injected between a write and its
/// fact.
async fn lock_month(db: &TestDb, starts_on: &str, ends_on: &str) {
    sqlx::query(
        "INSERT INTO gl_periods (id, kind, starts_on, ends_on, status, locked_at, locked_by) \
         VALUES ($1, 'month', $2::date, $3::date, 'locked', NOW(), 'test')",
    )
    .bind(Uuid::new_v4())
    .bind(starts_on)
    .bind(ends_on)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn staged(db: &TestDb, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind = $1")
        .bind(kind)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn facts(db: &TestDb, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM financial_facts WHERE kind = $1")
        .bind(kind)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

type FactRow = (
    Uuid,
    String,
    chrono::NaiveDate,
    Value,
    Option<String>,
    Option<String>,
    String,
);

async fn cogs_fact_rows(db: &TestDb) -> Vec<FactRow> {
    sqlx::query_as(
        "SELECT id, kind, happened_on, payload, source_table, source_id, created_by \
         FROM financial_facts WHERE kind = 'finance.cogs.recognized' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

fn cogs_body(source_id: &str) -> Value {
    serde_json::json!({
        "total_cost_cents": 12_345i64,
        "memo": "FG drained by a sale",
        "happened_on": "2026-05-12",
        "source_table": "jobs_step",
        "source_id": source_id,
    })
}

// --- de926d87: the COGS fact rides with its ledger event --------------------

/// The whole guarantee for COGS: post through the live endpoint, rebuild
/// `financial_facts` from the log alone, and get the identical fact
/// back — same deterministic id, same provenance, same payload, same
/// author. Before the fix the endpoint staged no event, so the rebuild
/// held no COGS fact at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_recognized_cogs_fact_survives_a_log_rooted_rebuild() {
    let db = TestDb::new().await;

    let (status, body) = post(&db, "/api/ledger/cogs-recognized", cogs_body("step-cogs-1")).await;
    assert_eq!(status, StatusCode::OK, "cogs: {body}");
    assert_eq!(
        staged(&db, "ledger.cogs.recognized").await,
        1,
        "the fact and its ledger event are staged in one transaction"
    );

    let live = cogs_fact_rows(&db).await;
    assert_eq!(live.len(), 1);

    drain(&db).await;
    rebuild_facts(&db.pool).await.unwrap();

    let rebuilt = cogs_fact_rows(&db).await;
    assert_eq!(rebuilt, live, "the rebuilt COGS fact is the live one");
}

/// Idempotency doubles as the event gate, as on the siblings: a repeat
/// POST with the same provenance resolves to the existing fact and
/// appends nothing to the log.
#[tokio::test(flavor = "multi_thread")]
async fn a_repeat_cogs_post_stages_exactly_one_event() {
    let db = TestDb::new().await;
    for _ in 0..3 {
        let (status, body) =
            post(&db, "/api/ledger/cogs-recognized", cogs_body("step-cogs-2")).await;
        assert_eq!(status, StatusCode::OK, "cogs: {body}");
    }
    assert_eq!(facts(&db, "finance.cogs.recognized").await, 1);
    assert_eq!(
        staged(&db, "ledger.cogs.recognized").await,
        1,
        "three POSTs, one event"
    );
}

/// A COGS post the ledger refuses leaves neither the fact nor its event.
#[tokio::test(flavor = "multi_thread")]
async fn a_cogs_post_into_a_locked_period_leaves_neither() {
    let db = TestDb::new().await;
    lock_month(&db, "2026-05-01", "2026-05-31").await;

    let (status, body) = post(&db, "/api/ledger/cogs-recognized", cogs_body("step-cogs-3")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "locked period: {body}");
    assert_eq!(facts(&db, "finance.cogs.recognized").await, 0);
    assert_eq!(staged(&db, "ledger.cogs.recognized").await, 0);
}

// --- 0b116fb9: a movement posts by its own fact kind ------------------------

/// A capitalization posts by the capitalization's posting rule, not the
/// transfer's. The code rules map both kinds to one body, so the two are
/// indistinguishable until a posting-rule row for
/// `finance.inventory.capitalized` says otherwise: this one debits WIP
/// (1310) where the payload names raw stock (1300). Before the fix the
/// endpoint handed the posting path the kind `finance.inventory.transferred`,
/// so the row was never consulted and the entry debited 1300.
#[tokio::test(flavor = "multi_thread")]
async fn a_capitalization_posts_by_the_capitalization_rule() {
    let db = TestDb::new().await;

    let operator = r#"{"id":"automation:tenant-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;
    let resp = build_router(&db)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ledger/posting-rules/batch")
                .header("content-type", "application/json")
                .header("x-boss-user", operator)
                .body(Body::from(
                    serde_json::json!({
                        "tenant_id": "algedonic",
                        "rules": [{
                            "fact_kind": "finance.inventory.capitalized",
                            "basis": "accrual",
                            "lines": [
                                {"account_code": "1310", "side": "debit",  "amount_path": "/total_cost_cents"},
                                {"account_code": "2110", "side": "credit", "amount_path": "/total_cost_cents"},
                            ],
                        }],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "publish rule: {}",
        String::from_utf8_lossy(&bytes)
    );

    let (status, body) = post(
        &db,
        "/api/ledger/inventory-capitalized",
        serde_json::json!({
            "total_cost_cents": 70_000i64,
            "debit_account": "1300",
            "credit_account": "2110",
            "happened_on": "2026-05-12",
            "source_table": "inventory_receipts",
            "source_id": "rcpt-cap-1",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "capitalize: {body}");
    let fact_id: Uuid = serde_json::from_str::<Value>(&body).unwrap()["fact_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let lines: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT a.code, l.debit_cents, l.credit_cents \
         FROM gl_journal_lines l \
         JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         JOIN gl_accounts a ON a.id = l.account_id \
         WHERE e.fact_id = $1 ORDER BY l.sort_order",
    )
    .bind(fact_id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        lines,
        vec![
            ("1310".to_string(), 70_000, 0),
            ("2110".to_string(), 0, 70_000),
        ],
        "the entry follows the capitalization's posting-rule row"
    );
}

// --- 016a2763: the settle flip commits with its fact -----------------------

/// The settle flip and `finance.payment.settled` are one transaction:
/// when the fact cannot post (its month is locked), the settlement stays
/// `pending`. Before the fix `mark_settled` had already committed on the
/// pool, so the row read `settled` with no fact and no event behind it —
/// and the next sweep could never retry it, because it only sweeps
/// `pending` rows.
#[tokio::test(flavor = "multi_thread")]
async fn a_settle_that_cannot_record_its_fact_leaves_the_settlement_pending() {
    let db = TestDb::new().await;

    let (status, body) = post(
        &db,
        "/api/ledger/bank-settlements",
        serde_json::json!({
            "id": "bs-split-1",
            "invoice_id": "inv-split-1",
            "account_id": "acc-1",
            "amount_cents": 250_000i64,
            "currency": "USD",
            "received_on": "2026-05-06",
            "bank_provider": "chase",
            "payment_method": "ach",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {body}");

    lock_month(&db, "2026-06-01", "2026-06-30").await;
    let (status, body) = post(
        &db,
        "/api/ledger/bank-settlements/bs-split-1/settle",
        serde_json::json!({"settled_on": "2026-06-10"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "locked period: {body}");
    assert!(body.contains("locked"), "refused for the lock: {body}");

    let (row_status, settled_on): (String, Option<chrono::NaiveDate>) =
        sqlx::query_as("SELECT status, settled_on FROM bank_settlements WHERE id = 'bs-split-1'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        (row_status.as_str(), settled_on),
        ("pending", None),
        "no settle flip without its fact"
    );
    assert_eq!(facts(&db, "finance.payment.settled").await, 0);
    assert_eq!(staged(&db, "ledger.payment.settled").await, 0);
}

/// A settle that commits stages exactly one settled event, and a second
/// settle of the same row is refused and stages nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_settle_stages_one_event_and_a_repeat_stages_none() {
    let db = TestDb::new().await;
    let (status, body) = post(
        &db,
        "/api/ledger/bank-settlements",
        serde_json::json!({
            "id": "bs-split-2",
            "invoice_id": "inv-split-2",
            "account_id": "acc-1",
            "amount_cents": 90_000i64,
            "currency": "USD",
            "received_on": "2026-05-06",
            "bank_provider": "chase",
            "payment_method": "wire",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {body}");

    let (status, body) = post(
        &db,
        "/api/ledger/bank-settlements/bs-split-2/settle",
        serde_json::json!({"settled_on": "2026-05-07"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settle: {body}");
    let (status, body) = post(
        &db,
        "/api/ledger/bank-settlements/bs-split-2/settle",
        serde_json::json!({"settled_on": "2026-05-08"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "repeat settle: {body}");

    assert_eq!(facts(&db, "finance.payment.settled").await, 1);
    assert_eq!(staged(&db, "ledger.payment.settled").await, 1);
}

// --- 016a2763: the payroll run commits with its fact -----------------------

fn payroll_body(id: &str, run_date: &str) -> Value {
    serde_json::json!({
        "id": id,
        "run_date": run_date,
        "period_start": run_date,
        "period_end": run_date,
        "employer_tax_cents": 45_000i64,
        "provider": "adp",
        "lines": [
            {"employee_id": "emp-1", "gross_cents": 200_000i64, "withheld_cents": 44_000i64,
             "net_cents": 156_000i64, "department": "brewing", "role": "brewer"},
            {"employee_id": "emp-2", "gross_cents": 100_000i64, "withheld_cents": 22_000i64,
             "net_cents": 78_000i64, "department": "ops", "role": "operator"},
        ],
    })
}

/// Opening capital (DR 1000 Cash / CR 3000), so a payroll run's cash
/// credit does not trip the "1000 Cash must not go negative" guard —
/// the only refusal these tests mean to inject is the locked period.
async fn seed_opening_cash(db: &TestDb) {
    let (status, body) = post(
        db,
        "/api/ledger/journal-entries",
        serde_json::json!({
            "posted_on": "2026-01-01",
            "memo": "opening capital",
            "lines": [
                {"account_code": "1000", "debit_cents": 10_000_000i64},
                {"account_code": "3000", "credit_cents": 10_000_000i64},
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "opening cash: {body}");
}

async fn payroll_rows(db: &TestDb, id: &str) -> (i64, i64) {
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payroll_runs WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let lines: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payroll_run_lines WHERE run_id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    (runs, lines)
}

/// The payroll run, its lines, the fact, the journal entry and the event
/// are one transaction: a run whose fact cannot post writes no run.
/// Before the fix `create_run` committed the header and both lines on
/// the pool first, so the run existed with no fact behind it — and every
/// later POST of the same id short-circuited on the existing row, so the
/// fact could never be recorded at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_payroll_run_that_cannot_record_its_fact_writes_no_run() {
    let db = TestDb::new().await;
    seed_opening_cash(&db).await;
    lock_month(&db, "2026-06-01", "2026-06-30").await;

    let (status, body) = post(
        &db,
        "/api/ledger/payroll-runs",
        payroll_body("payroll-split-1", "2026-06-12"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "locked period: {body}");
    assert!(body.contains("locked"), "refused for the lock: {body}");

    assert_eq!(
        payroll_rows(&db, "payroll-split-1").await,
        (0, 0),
        "no payroll run or line without its fact"
    );
    assert_eq!(facts(&db, "finance.payroll.run").await, 0);
    assert_eq!(staged(&db, "ledger.payroll.run").await, 0);
}

/// The ordinary path still lands all of it together, once.
#[tokio::test(flavor = "multi_thread")]
async fn a_payroll_run_lands_with_its_fact_and_one_event() {
    let db = TestDb::new().await;
    seed_opening_cash(&db).await;
    for _ in 0..2 {
        let (status, body) = post(
            &db,
            "/api/ledger/payroll-runs",
            payroll_body("payroll-split-2", "2026-05-15"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "payroll: {body}");
    }
    assert_eq!(payroll_rows(&db, "payroll-split-2").await, (1, 2));
    assert_eq!(facts(&db, "finance.payroll.run").await, 1);
    assert_eq!(staged(&db, "ledger.payroll.run").await, 1);
}

/// The synthesize door shares the same write: a run it cannot post
/// writes no run either.
#[tokio::test(flavor = "multi_thread")]
async fn a_synthesized_payroll_run_that_cannot_record_its_fact_writes_no_run() {
    let db = TestDb::new().await;
    sqlx::query(
        "INSERT INTO employees (id, name, role, department, status, employment_type, \
                                annual_salary_cents) \
         VALUES ('emp-synth-1', 'Pat Brewer', 'brewer', 'brewing', 'active', 'full_time', 5200000)",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    seed_opening_cash(&db).await;
    lock_month(&db, "2026-06-01", "2026-06-30").await;

    let (status, body) = post(
        &db,
        "/api/ledger/payroll-runs/synthesize",
        serde_json::json!({
            "run_date": "2026-06-12",
            "period_start": "2026-05-30",
            "period_end": "2026-06-12",
            "periods_per_year": 26,
            "withholding_bps": 2200,
            "employer_cost_bps": 1500,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "locked period: {body}");
    assert!(body.contains("locked"), "refused for the lock: {body}");

    assert_eq!(payroll_rows(&db, "payroll-20260612").await, (0, 0));
    assert_eq!(facts(&db, "finance.payroll.run").await, 0);
    assert_eq!(staged(&db, "ledger.payroll.run").await, 0);
}
