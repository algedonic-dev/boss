//! The replay checks must never block live writers (TODO item from
//! the 2026-07-10 incident: the deep check's in-tx TRUNCATE held an
//! ACCESS EXCLUSIVE lock on financial_facts for its full runtime —
//! ~2 min of stalled fact writes nightly, and a concurrent year-regen
//! hard-failed on a 30s client timeout).
//!
//! Two layers of proof:
//! 1. Mechanism: inside a shadowing transaction, an unqualified
//!    TRUNCATE + INSERT resolve to the TEMP clones — a concurrent
//!    writer on another connection inserts into the live table with a
//!    2s statement_timeout and succeeds, and the live table is
//!    byte-untouched after rollback.
//! 2. End-to-end: the real deep check runs WHILE a concurrent writer
//!    inserts, and the writer never times out — the lock-freedom
//!    race, against the actual check. (Divergence semantics under
//!    load are covered by the e2e suite with real periods; this DB
//!    has no open periods, so the diff scope here is empty by
//!    construction.)

#![cfg(feature = "postgres")]

use boss_ledger::{FactWrite, record_fact_in_tx};
use boss_testing::TestDb;

async fn insert_live_fact(pool: &sqlx::PgPool, source_id: &str) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL statement_timeout = '2s'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let payload = serde_json::json!({
        "total_cost_cents": 1234,
        "debit_account": "1300",
        "credit_account": "3000",
        "source_table": "lock_free_test",
        "source_id": source_id,
        "happened_on": "2025-04-01",
    });
    record_fact_in_tx(
        &mut tx,
        FactWrite {
            kind: "finance.inventory.transferred",
            happened_on: "2025-04-01".parse().unwrap(),
            payload: &payload,
            source_table: Some("lock_free_test"),
            source_id: Some(source_id),
            created_by: "test",
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn shadowed_truncate_never_locks_the_live_table() {
    let db = TestDb::new().await;
    insert_live_fact(&db.pool, "pre-existing").await;

    // Open the shadowing tx and TRUNCATE/INSERT through the temp clone.
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "CREATE TEMP TABLE financial_facts \
         (LIKE public.financial_facts INCLUDING ALL) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("TRUNCATE financial_facts CASCADE")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO financial_facts \
            (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES (gen_random_uuid(), 'shadow.only', '2025-04-01', '{}', 'shadow', 's1', 'test')",
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // A concurrent live writer must get through in well under 2s —
    // before the shadow fix, the equivalent live TRUNCATE would block
    // this until the tx ended (statement timeout → test failure).
    insert_live_fact(&db.pool, "concurrent-during-shadow").await;

    tx.rollback().await.unwrap();

    // Live table byte-untouched by the shadow session: both live rows
    // present, the shadow row nowhere.
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM financial_facts WHERE source_table = 'lock_free_test'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n, 2);
    let (shadow_rows,): (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM financial_facts WHERE kind = 'shadow.only'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(shadow_rows, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn deep_check_runs_clean_while_a_writer_writes() {
    let db = TestDb::new().await;
    insert_live_fact(&db.pool, "baseline").await;

    // Race the real deep check against a stream of live writes. Every
    // write carries a 2s statement_timeout: if the check ever takes
    // the old exclusive lock, a write times out and the test fails.
    let writer_pool = db.pool.clone();
    let writer = tokio::spawn(async move {
        for i in 0..20 {
            insert_live_fact(&writer_pool, &format!("during-{i}")).await;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });
    let report = boss_ledger::replay_check_from_audit_log(&db.pool)
        .await
        .unwrap();
    writer.await.unwrap();

    // The writer finishing without a statement timeout IS the
    // assertion that matters; the clean report confirms the check
    // completed normally alongside it.
    assert!(
        report.fact_divergences.is_empty() && report.entry_divergences.is_empty(),
        "check must complete clean alongside concurrent writes: {:?} {:?}",
        report.fact_divergences,
        report.entry_divergences
    );
}
