//! Regression: an event whose subject resolution finds an id but no
//! kind must be SKIPPED, not abort the rebuild.
//!
//! `NULL || ' ' || id` is NULL in Postgres, so one such event nulled
//! the `body` column and the whole TRUNCATE-and-replay died on the
//! NOT NULL constraint — measured 2026-08-19 as a crash-looping
//! reindex timer and an index quietly going stale behind it.

#![cfg(feature = "postgres")]

use boss_search::rebuild_search;
use boss_testing::TestDb;

#[tokio::test]
async fn an_event_with_an_id_but_no_kind_is_skipped_not_fatal() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    // A healthy, fully-resolvable event: names its subject as a pair.
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) VALUES \
         (gen_random_uuid(), now(), 'test', 'assets.asset.tagged', \
          '{\"subject_kind\":\"asset\",\"subject_id\":\"SYS-1\"}')",
    )
    .execute(pool)
    .await
    .unwrap();

    // The poison row: an id with NO kind, from any resolution source.
    // Before the pair filter this nulled `body` and aborted the run.
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) VALUES \
         (gen_random_uuid(), now(), 'test', 'orphan.signal', '{\"subject_id\":\"SYS-2\"}')",
    )
    .execute(pool)
    .await
    .unwrap();

    let report = rebuild_search(pool)
        .await
        .expect("a half-resolvable event must not abort the rebuild");

    // The healthy event indexed; the kindless one skipped as noise —
    // the same treatment an entirely subject-less event already gets.
    assert_eq!(report.events_indexed, 1);
    let bodies: i64 =
        sqlx::query_scalar("SELECT count(*) FROM search_index WHERE ref_kind = 'event'")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(bodies, 1);
}
