//! End-to-end tests for `boss-ledger::rebuild_payroll`.
//!
//! Proves payroll is a pure projection of the `finance.payroll.run`
//! facts — and therefore of `audit_log`. A directly-written stale row
//! from a "prior epoch" (no backing fact/event) is wiped, and
//! `payroll_runs` + `payroll_run_lines` are reconstructed from the log
//! alone. This is the property the demo epoch-loop relies on:
//! `boss-rebuild-all` now reproduces payroll from the trimmed log
//! instead of letting prior-cycle rows accumulate and collide with the
//! synthesize idempotency key.

#![cfg(feature = "postgres")]

use boss_ledger::{rebuild_facts, rebuild_payroll};
use boss_testing::TestDb;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

async fn insert_audit_event(
    db: &TestDb,
    kind: &str,
    timestamp: DateTime<Utc>,
    source: &str,
    payload: &Value,
) -> Uuid {
    let event_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(event_id)
    .bind(timestamp)
    .bind(source)
    .bind(kind)
    .bind(payload)
    .execute(&db.pool)
    .await
    .unwrap();
    event_id
}

/// A `finance.payroll.run` payload exactly as the synthesize/create
/// handlers emit it: header totals + the per-employee `lines` array.
/// Two employees; totals are the column-wise sums so every CHECK on
/// `payroll_runs`/`payroll_run_lines` holds.
fn payroll_payload(run_id: &str, run_date: &str) -> Value {
    serde_json::json!({
        "run_id": run_id,
        "run_date": run_date,
        "period_start": run_date,
        "period_end": run_date,
        "gross_cents": 300_000i64,
        "withheld_cents": 66_000i64,
        "employer_tax_cents": 45_000i64,
        "net_cents": 234_000i64,
        "employee_count": 2,
        "provider": "adp",
        "lines": [
            {"employee_id": "emp-1", "gross_cents": 200_000i64, "withheld_cents": 44_000i64,
             "net_cents": 156_000i64, "department": "brewing", "role": "brewer"},
            {"employee_id": "emp-2", "gross_cents": 100_000i64, "withheld_cents": 22_000i64,
             "net_cents": 78_000i64, "department": "ops", "role": "operator"},
        ],
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reconstructs_payroll_from_log_and_wipes_stale_rows() {
    let db = TestDb::new().await;

    // A stale directly-written run from a "prior epoch" — the exact
    // bug class this fix closes. It has NO backing fact/event, so a
    // log-rooted rebuild must drop it.
    sqlx::query(
        "INSERT INTO payroll_runs \
            (id, run_date, period_start, period_end, gross_cents, employer_tax_cents, \
             withheld_cents, net_cents, employee_count, provider, status) \
         VALUES ('payroll-stale','2099-01-01','2099-01-01','2099-01-01',1,0,0,1,1,'adp','posted')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO payroll_run_lines \
            (run_id, employee_id, gross_cents, withheld_cents, net_cents, department, role) \
         VALUES ('payroll-stale','ghost',1,0,1,'x','y')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // Two current-epoch payroll runs, recorded as audit_log events
    // exactly as the synthesize handler publishes them.
    insert_audit_event(
        &db,
        "ledger.payroll.run",
        "2026-04-10T09:00:00Z".parse().unwrap(),
        "ledger",
        &payroll_payload("payroll-20260410", "2026-04-10"),
    )
    .await;
    insert_audit_event(
        &db,
        "ledger.payroll.run",
        "2026-04-24T09:00:00Z".parse().unwrap(),
        "ledger",
        &payroll_payload("payroll-20260424", "2026-04-24"),
    )
    .await;

    // audit_log -> financial_facts -> payroll_runs/lines: the two
    // stages boss-rebuild-all runs back-to-back.
    rebuild_facts(&db.pool).await.unwrap();
    let report = rebuild_payroll(&db.pool).await.unwrap();

    assert_eq!(
        report.runs_written, 2,
        "two runs reconstructed from the log"
    );
    assert_eq!(report.lines_written, 4, "two lines per run");

    // The stale prior-epoch row is gone — payroll is now exactly the
    // set backed by the log.
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM payroll_runs ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        ids,
        vec![
            "payroll-20260410".to_string(),
            "payroll-20260424".to_string()
        ]
    );
    let stale_lines: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM payroll_run_lines WHERE run_id = 'payroll-stale'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(stale_lines, 0, "stale prior-epoch lines wiped");

    // Header totals + per-employee detail reconstructed faithfully.
    let header = sqlx::query(
        "SELECT gross_cents, withheld_cents, employer_tax_cents, net_cents, employee_count \
         FROM payroll_runs WHERE id = 'payroll-20260410'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(header.get::<i64, _>("gross_cents"), 300_000);
    assert_eq!(header.get::<i64, _>("withheld_cents"), 66_000);
    assert_eq!(header.get::<i64, _>("employer_tax_cents"), 45_000);
    assert_eq!(header.get::<i64, _>("net_cents"), 234_000);
    assert_eq!(header.get::<i32, _>("employee_count"), 2);

    let lines: Vec<(String, i64)> = sqlx::query_as(
        "SELECT employee_id, gross_cents FROM payroll_run_lines \
         WHERE run_id = 'payroll-20260410' ORDER BY employee_id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        lines,
        vec![
            ("emp-1".to_string(), 200_000),
            ("emp-2".to_string(), 100_000)
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_payroll_is_idempotent() {
    let db = TestDb::new().await;
    insert_audit_event(
        &db,
        "ledger.payroll.run",
        "2026-04-10T09:00:00Z".parse().unwrap(),
        "ledger",
        &payroll_payload("payroll-20260410", "2026-04-10"),
    )
    .await;
    rebuild_facts(&db.pool).await.unwrap();
    rebuild_payroll(&db.pool).await.unwrap();

    // Second pass re-derives from the same facts — TRUNCATE-then-replay
    // means no duplication.
    let report = rebuild_payroll(&db.pool).await.unwrap();
    assert_eq!(report.runs_written, 1);

    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payroll_runs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let lines: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payroll_run_lines")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(runs, 1, "no run duplication across rebuilds");
    assert_eq!(lines, 2, "no line duplication across rebuilds");
}
