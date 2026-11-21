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

    // The events half reads event_facts (backlog 832386eb), so the
    // projection is built first, by its own rebuilder.
    boss_views::rebuild_event_facts(pool).await.unwrap();
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

/// b6e1d969: loss of the disposable index is recoverable from its
/// originals. A truncate models the empty-cache outcome, not a PG crash.
#[tokio::test]
async fn an_empty_unlogged_search_cache_rebuilds_without_losing_its_originals() {
    use boss_search::query::SearchScope;
    let db = TestDb::new().await;
    let pool = &db.pool;
    sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES(gen_random_uuid(),now(),'test-search-cache','assets.asset.tagged','{\"subject_kind\":\"asset\",\"subject_id\":\"cacheproof\"}')")
        .execute(pool).await.unwrap();
    let originals: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(a)::text FROM audit_log a WHERE source='test-search-cache' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(originals.len(), 1);
    let scope = SearchScope {
        job_owners: None,
        may_read_events: true,
        may_read_subjects: true,
    };
    boss_views::rebuild_event_facts(pool).await.unwrap();
    rebuild_search(pool).await.unwrap();
    let expected = boss_search::search(pool, "cacheproof", &[], &scope)
        .await
        .unwrap();
    assert_eq!(expected.events.len(), 1);
    sqlx::query("TRUNCATE search_index")
        .execute(pool)
        .await
        .unwrap();
    let empty = boss_search::search(pool, "cacheproof", &[], &scope)
        .await
        .unwrap();
    assert!(empty.subjects.is_empty() && empty.jobs.is_empty() && empty.events.is_empty());
    rebuild_search(pool).await.unwrap();
    assert_eq!(
        boss_search::search(pool, "cacheproof", &[], &scope)
            .await
            .unwrap(),
        expected
    );
    rebuild_search(pool).await.unwrap();
    assert_eq!(
        boss_search::search(pool, "cacheproof", &[], &scope)
            .await
            .unwrap(),
        expected
    );
    let after: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(a)::text FROM audit_log a WHERE source='test-search-cache' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(after, originals, "cache loss never loses an original fact");
    let persistence: String = sqlx::query_scalar(
        "SELECT relpersistence::text FROM pg_class WHERE oid='search_index'::regclass",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        persistence, "u",
        "the rebuilt disposable cache must not WAL-log every replay"
    );
}
