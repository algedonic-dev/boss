//! The `subject_edges` referential guard (R2 — the one relationship
//! registry). The BEFORE-INSERT trigger on `event_outbox` (and
//! `audit_log`) reads `subject_edges` and resolves every declared
//! edge against the `subjects` identity table, aborting the write
//! when the referenced subject is missing — inside the domain
//! transaction, so a dangling reference never becomes state (the
//! 2026-07-13 phantom-account incident class, now closed for every
//! subject-referential edge, not just the hand-picked ones).

#![cfg(feature = "postgres")]

use boss_core::event::Event;
use boss_events::outbox::record_event_in_tx;
use boss_testing::TestDb;
use chrono::{TimeZone, Utc};
use uuid::Uuid;

async fn declare_edge(db: &TestDb, source_kind: &str, field: &str, target: &str, on_missing: &str) {
    sqlx::query(
        "INSERT INTO subject_edges (source_kind, field_path, target_kind, on_missing) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(source_kind)
    .bind(field)
    .bind(target)
    .bind(on_missing)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn seed_subject(db: &TestDb, kind: &str, id: &str) {
    sqlx::query("INSERT INTO subjects (kind, id) VALUES ($1, $2)")
        .bind(kind)
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
}

fn event(kind: &str, payload: serde_json::Value) -> Event {
    Event {
        id: Uuid::new_v4(),
        timestamp: Utc.with_ymd_and_hms(2026, 7, 17, 12, 0, 0).unwrap(),
        source: "subject-edges-test".to_string(),
        kind: kind.to_string(),
        payload,
    }
}

// TestDb disables ref-checks database-wide (test sessions write
// events without the full projection pipeline that populates parent
// tables); re-enable for exactly the transaction under test.
// SET LOCAL cannot leak through the pool.
async fn enable_ref_check(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) {
    sqlx::query("SET LOCAL audit_log.ref_check = 'on'")
        .execute(&mut **tx)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn edge_resolves_present_subject_and_aborts_a_missing_one() {
    let db = TestDb::new().await;
    declare_edge(&db, "test.thing.happened", "account_id", "account", "abort").await;
    seed_subject(&db, "account", "acc-real").await;

    // Present reference → the write lands.
    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    record_event_in_tx(
        &mut tx,
        &event(
            "test.thing.happened",
            serde_json::json!({"account_id": "acc-real"}),
        ),
    )
    .await
    .expect("present subject must pass the edge guard");
    tx.commit().await.unwrap();

    // Missing reference → the guard aborts the domain write.
    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    let err = record_event_in_tx(
        &mut tx,
        &event(
            "test.thing.happened",
            serde_json::json!({"account_id": "acc-ghost"}),
        ),
    )
    .await;
    assert!(err.is_err(), "missing subject must abort the write");
    drop(tx);

    // Only the committed (valid) row survives.
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'test.thing.happened'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 1, "the aborted write must leave no row");
}

#[tokio::test(flavor = "multi_thread")]
async fn null_or_empty_reference_is_skipped() {
    let db = TestDb::new().await;
    declare_edge(&db, "test.optional.ref", "account_id", "account", "abort").await;

    // Field absent entirely → no check.
    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    record_event_in_tx(
        &mut tx,
        &event("test.optional.ref", serde_json::json!({"other": 1})),
    )
    .await
    .expect("absent optional ref must skip the check");
    // Field present but empty string → no check.
    record_event_in_tx(
        &mut tx,
        &event("test.optional.ref", serde_json::json!({"account_id": ""})),
    )
    .await
    .expect("empty optional ref must skip the check");
    tx.commit().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn on_missing_warn_allows_the_write() {
    let db = TestDb::new().await;
    declare_edge(&db, "test.soft.ref", "account_id", "account", "warn").await;

    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    record_event_in_tx(
        &mut tx,
        &event(
            "test.soft.ref",
            serde_json::json!({"account_id": "acc-ghost"}),
        ),
    )
    .await
    .expect("on_missing=warn must allow a missing reference");
    tx.commit().await.unwrap();

    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'test.soft.ref'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn seeded_module_edges_are_present() {
    // The migration moved the invoice→account and products→product
    // rows off audit_log_ref_checks and onto subject_edges; the
    // part_sku non-subject residual stays behind. Pin both halves so
    // a botched migration fails here.
    let db = TestDb::new().await;
    let subject_edges: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM subject_edges WHERE target_kind = 'account' \
         AND source_kind LIKE 'commerce.invoice.%'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        subject_edges, 4,
        "invoice→account edges must be in subject_edges"
    );

    let residual: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log_ref_checks WHERE ref_table = 'inventory_items'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        residual, 2,
        "part_sku checks stay on the non-subject residual"
    );

    // And nothing subject-referential was left behind on the old
    // mechanism.
    let stragglers: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log_ref_checks WHERE ref_table IN ('accounts', 'products')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        stragglers, 0,
        "no subject-referential rows remain on audit_log_ref_checks"
    );
}
