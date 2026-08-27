//! The event-kind drift guard (registry 108, af1586e1 — Q3: warn +
//! a guard that NAMES the offender).
//!
//! Contracts: an exact row matches; a family pattern (`x.y.*`)
//! matches every suffix; an emitted kind nothing declares is
//! returned by name; the seeded registry covers a representative
//! sample of the live vocabulary.

use boss_events::integrity::unregistered_kinds;
use boss_testing::TestDb;

async fn emit(pool: &sqlx::PgPool, kind: &str) {
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES (gen_random_uuid(), NOW(), 'test', $1, '{}')",
    )
    .bind(kind)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn exact_rows_families_and_strangers() {
    let db = TestDb::new().await;
    // The 108 seed is applied by TestDb. Emit: a seeded static kind,
    // two family members, and a stranger.
    emit(&db.pool, "jobs.job.created").await;
    emit(&db.pool, "step.done.brand-new-step-type").await;
    emit(&db.pool, "step.ready.task").await;
    emit(&db.pool, "totally.unknown.kind").await;

    let missing = unregistered_kinds(&db.pool).await.unwrap();
    assert_eq!(
        missing,
        vec!["totally.unknown.kind".to_string()],
        "exact + family matches pass; only the stranger is named"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_seed_is_not_empty_and_carries_the_families() {
    let db = TestDb::new().await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM event_kinds")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(n >= 50, "the harvest seeded the live vocabulary, got {n}");
    let fams: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_kinds WHERE suffix_domain = 'step_types'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        fams, 3,
        "the three step families declare their suffix domain"
    );
}
