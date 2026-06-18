//! End-to-end coverage for the `financial_facts` supersede path.
//! Exercises three threads:
//!
//! 1. `apply_supersede_in_tx` flips `supersede_reason` + drops the
//!    fact's `gl_journal_entries` rows in one transaction.
//! 2. The rebuild path skips superseded facts when regenerating the
//!    journal (`gl_journal_entries`).
//! 3. A `ledger.fact.superseded` audit_log event is replayed inside
//!    `rebuild_facts`, so a clean rebuild from `audit_log` reproduces
//!    the supersede markers — no false-positive divergences from
//!    `replay_check_from_audit_log`.

#![cfg(feature = "postgres")]

use boss_ledger::supersede::{SupersedeOutcome, SupersedeRequest, apply_supersede_in_tx};
use boss_ledger::{rebuild, rebuild_facts};
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

async fn seed_invoice_fact(db: &TestDb, invoice_id: &str) {
    let payload = serde_json::json!({
        "id": invoice_id,
        "issued_on": "2026-04-01",
        "amount_cents": 50000,
        "account_id": "acct-A",
        "currency": "USD",
        "line_items": [
            {"description": "Setup fee", "amount_cents": 50000, "category": "service"},
        ],
    });
    insert_audit_event(
        db,
        "commerce.invoice.created",
        "2026-04-01T12:00:00Z".parse().unwrap(),
        "commerce",
        &payload,
    )
    .await;
    rebuild_facts(&db.pool).await.unwrap();
    rebuild(&db.pool).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn supersede_marks_reason_and_drops_journal_entries() {
    let db = TestDb::new().await;
    seed_invoice_fact(&db, "inv-supersede-001").await;

    let entries_before: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM gl_journal_entries e \
         JOIN financial_facts f ON f.id = e.fact_id \
         WHERE f.source_id = 'inv-supersede-001'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(entries_before.0 > 0, "rebuild should have produced entries");

    let req = SupersedeRequest {
        kind: "finance.invoice.issued".into(),
        source_table: Some("invoices".into()),
        source_id: Some("inv-supersede-001".into()),
        reason: "AR-double-credit duplicate, 2026-05-06".into(),
        superseded_by: None,
    };

    let mut tx = db.pool.begin().await.unwrap();
    let outcome = apply_supersede_in_tx(&mut tx, &req).await.unwrap();
    tx.commit().await.unwrap();

    match outcome {
        SupersedeOutcome::Applied {
            entries_dropped, ..
        } => {
            assert!(entries_dropped > 0, "should drop the existing entries");
        }
        other => panic!("expected Applied, got {other:?}"),
    }

    let reason: Option<String> = sqlx::query_scalar(
        "SELECT supersede_reason FROM financial_facts WHERE source_id = 'inv-supersede-001'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        reason.as_deref(),
        Some("AR-double-credit duplicate, 2026-05-06")
    );

    let entries_after: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM gl_journal_entries e \
         JOIN financial_facts f ON f.id = e.fact_id \
         WHERE f.source_id = 'inv-supersede-001'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        entries_after.0, 0,
        "supersede should drop dependent entries"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn second_supersede_returns_already_superseded() {
    let db = TestDb::new().await;
    seed_invoice_fact(&db, "inv-supersede-002").await;

    let req = SupersedeRequest {
        kind: "finance.invoice.issued".into(),
        source_table: Some("invoices".into()),
        source_id: Some("inv-supersede-002".into()),
        reason: "first call".into(),
        superseded_by: None,
    };

    let mut tx = db.pool.begin().await.unwrap();
    apply_supersede_in_tx(&mut tx, &req).await.unwrap();
    tx.commit().await.unwrap();

    let req2 = SupersedeRequest {
        reason: "second call".into(),
        ..req
    };
    let mut tx = db.pool.begin().await.unwrap();
    let outcome = apply_supersede_in_tx(&mut tx, &req2).await.unwrap();
    tx.commit().await.unwrap();

    match outcome {
        SupersedeOutcome::AlreadySuperseded { reason, .. } => {
            assert_eq!(reason, "first call", "preserves the original reason");
        }
        other => panic!("expected AlreadySuperseded, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn supersede_returns_not_found_for_unknown_natural_key() {
    let db = TestDb::new().await;

    let req = SupersedeRequest {
        kind: "finance.invoice.issued".into(),
        source_table: Some("invoices".into()),
        source_id: Some("does-not-exist".into()),
        reason: "should not apply".into(),
        superseded_by: None,
    };
    let mut tx = db.pool.begin().await.unwrap();
    let outcome = apply_supersede_in_tx(&mut tx, &req).await.unwrap();
    assert!(matches!(outcome, SupersedeOutcome::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_replays_supersede_event_from_audit_log() {
    let db = TestDb::new().await;
    seed_invoice_fact(&db, "inv-supersede-003").await;

    // Apply the supersede the same way the HTTP handler does:
    //   1. UPDATE the row inside a tx.
    //   2. After commit, INSERT the ledger.fact.superseded audit event.
    let req = SupersedeRequest {
        kind: "finance.invoice.issued".into(),
        source_table: Some("invoices".into()),
        source_id: Some("inv-supersede-003".into()),
        reason: "duplicate from cleanup pass".into(),
        superseded_by: None,
    };
    let mut tx = db.pool.begin().await.unwrap();
    apply_supersede_in_tx(&mut tx, &req).await.unwrap();
    tx.commit().await.unwrap();

    let supersede_payload = serde_json::json!({
        "kind": req.kind,
        "source_table": req.source_table,
        "source_id": req.source_id,
        "reason": req.reason,
        "superseded_by": null,
    });
    insert_audit_event(
        &db,
        "ledger.fact.superseded",
        "2026-05-06T08:00:00Z".parse().unwrap(),
        "ledger",
        &supersede_payload,
    )
    .await;

    // Truncate financial_facts (cascading the journal entries) and
    // rebuild from audit_log. The rebuild should:
    //   1. Re-project the original commerce.invoice.created event into
    //      a fresh financial_fact row.
    //   2. Replay the ledger.fact.superseded event, marking the row's
    //      supersede_reason.
    sqlx::query("TRUNCATE financial_facts CASCADE")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = rebuild_facts(&db.pool).await.unwrap();
    assert!(report.facts_written >= 1);
    assert!(
        report.supersedes_applied >= 1,
        "rebuild should replay the superseded event"
    );

    let reason: Option<String> = sqlx::query_scalar(
        "SELECT supersede_reason FROM financial_facts WHERE source_id = 'inv-supersede-003'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        reason.as_deref(),
        Some("duplicate from cleanup pass"),
        "rebuilt row must carry the supersede marker"
    );

    // And the journal-rebuild path must skip the superseded fact, so
    // no entries get re-created.
    rebuild(&db.pool).await.unwrap();
    let entries: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM gl_journal_entries e \
         JOIN financial_facts f ON f.id = e.fact_id \
         WHERE f.source_id = 'inv-supersede-003'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(entries.0, 0, "rebuild must not re-project superseded facts");
}
