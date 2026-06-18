//! End-to-end tests for `replay_check_from_audit_log`.
//!
//! Synthesizes a real-world event in `audit_log`, runs `rebuild_facts`
//! to project it into `financial_facts`, then runs `rebuild` to
//! project the fact into `gl_journal_entries`, then runs the deep
//! replay-check inside an aborted transaction and asserts no
//! divergences. Live state is unchanged after the check completes.

#![cfg(feature = "postgres")]

use boss_ledger::{rebuild, rebuild_facts, replay_check_from_audit_log};
use boss_testing::TestDb;
use chrono::{DateTime, Utc};
use serde_json::Value;
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

async fn ensure_open_period(db: &TestDb, year: i32, month: u32) {
    let starts_on = chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    let ends_on = if month == 12 {
        chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .unwrap()
    .pred_opt()
    .unwrap();
    sqlx::query(
        "INSERT INTO gl_periods (id, kind, starts_on, ends_on, status) \
         VALUES ($1, 'month', $2, $3, 'open') \
         ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(starts_on)
    .bind(ends_on)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn deep_check_passes_when_audit_log_facts_and_entries_agree() {
    let db = TestDb::new().await;
    ensure_open_period(&db, 2026, 4).await;

    insert_audit_event(
        &db,
        "commerce.invoice.created",
        "2026-04-15T12:00:00Z".parse().unwrap(),
        "commerce",
        &serde_json::json!({
            "id": "inv-AUD-1",
            "issued_on": "2026-04-15",
            "amount_cents": 100000,
            "account_id": "acct-A",
            "currency": "USD",
            "line_items": [
                {"description": "Setup", "amount_cents": 100000, "category": "service"},
            ],
        }),
    )
    .await;

    rebuild_facts(&db.pool).await.unwrap();
    rebuild(&db.pool).await.unwrap();

    let report = replay_check_from_audit_log(&db.pool).await.unwrap();

    assert!(
        report.is_ok(),
        "deep check should pass — got {} fact divergences, {} entry divergences",
        report.fact_divergences.len(),
        report.entry_divergences.len(),
    );
    assert_eq!(report.facts_in_live, 1);
    assert_eq!(report.facts_in_replay, 1);
    assert_eq!(report.events_scanned, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn deep_check_does_not_mutate_live_state() {
    let db = TestDb::new().await;
    ensure_open_period(&db, 2026, 5).await;

    insert_audit_event(
        &db,
        "commerce.invoice.created",
        "2026-05-10T08:00:00Z".parse().unwrap(),
        "commerce",
        &serde_json::json!({
            "id": "inv-AUD-2",
            "issued_on": "2026-05-10",
            "amount_cents": 50000,
            "account_id": "acct-B",
            "currency": "USD",
            "line_items": [
                {"description": "Subs", "amount_cents": 50000, "category": "service"},
            ],
        }),
    )
    .await;
    rebuild_facts(&db.pool).await.unwrap();
    rebuild(&db.pool).await.unwrap();

    let pre_facts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM financial_facts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let pre_entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_journal_entries")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let pre_lines: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_journal_lines")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    replay_check_from_audit_log(&db.pool).await.unwrap();

    let post_facts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM financial_facts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let post_entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_journal_entries")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let post_lines: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_journal_lines")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    assert_eq!(post_facts, pre_facts, "fact count unchanged");
    assert_eq!(post_entries, pre_entries, "entry count unchanged");
    assert_eq!(post_lines, pre_lines, "line count unchanged");
}

#[tokio::test(flavor = "multi_thread")]
async fn deep_check_flags_fact_only_in_live_when_audit_log_lacks_event() {
    let db = TestDb::new().await;
    ensure_open_period(&db, 2026, 6).await;

    // Write a fact directly, no audit_log event. The replay would
    // produce zero facts; live has one. Expect OnlyInLive.
    let fact_id = Uuid::new_v4();
    let payload = serde_json::json!({
        "id": "inv-DRIFT-1",
        "issued_on": "2026-06-01",
        "amount_cents": 1000,
        "account_id": "acct-C",
        "currency": "USD",
        "line_items": [
            {"description": "drifted", "amount_cents": 1000, "category": "service"},
        ],
    });
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, 'finance.invoice.issued', '2026-06-01', $2, 'invoices', 'inv-DRIFT-1', 'commerce')",
    )
    .bind(fact_id)
    .bind(&payload)
    .execute(&db.pool)
    .await
    .unwrap();

    let report = replay_check_from_audit_log(&db.pool).await.unwrap();

    assert!(!report.fact_divergences.is_empty());
    let only_in_live = report
        .fact_divergences
        .iter()
        .filter(|d| matches!(d, boss_ledger::FactDivergence::OnlyInLive { .. }))
        .count();
    assert_eq!(only_in_live, 1, "the orphan live fact should surface");

    // Verify the rebuilt row was rolled back — drift fact still
    // present in live.
    let live_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM financial_facts WHERE source_id = 'inv-DRIFT-1'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(live_count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn deep_check_flags_fact_only_in_replay_when_live_was_wiped() {
    let db = TestDb::new().await;
    ensure_open_period(&db, 2026, 7).await;

    insert_audit_event(
        &db,
        "commerce.invoice.created",
        "2026-07-15T12:00:00Z".parse().unwrap(),
        "commerce",
        &serde_json::json!({
            "id": "inv-MISSING-1",
            "issued_on": "2026-07-15",
            "amount_cents": 5000,
            "account_id": "acct-D",
            "currency": "USD",
            "line_items": [
                {"description": "Test", "amount_cents": 5000, "category": "service"},
            ],
        }),
    )
    .await;
    // Notice: NO rebuild_facts / rebuild here. financial_facts is
    // empty; replay produces one fact. Expect OnlyInReplay.

    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM financial_facts WHERE source_id = 'inv-MISSING-1'")
            .fetch_optional(&db.pool)
            .await
            .unwrap();
    assert!(row.is_none(), "live has no fact for this event");

    let report = replay_check_from_audit_log(&db.pool).await.unwrap();

    let only_in_replay = report
        .fact_divergences
        .iter()
        .filter(|d| matches!(d, boss_ledger::FactDivergence::OnlyInReplay { .. }))
        .count();
    assert_eq!(only_in_replay, 1);
    assert_eq!(report.events_scanned, 1);
}
