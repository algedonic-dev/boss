//! The events half of the search rebuild must not sort the log.
//!
//! Backlog 832386eb (2026-11-11): the hourly `boss-search-reindex` ranked
//! every resolved `audit_log` row with `ROW_NUMBER() OVER (PARTITION BY
//! subject ORDER BY id DESC)` to keep 50 per Subject. That is a sort of
//! the whole log, and on the system of record it wrote 13.0 GB of temp
//! files per run (queryid 3112194067547126450, 47.6 M temp blocks over
//! 30 calls) onto a pgdata volume with 5.7 GiB free.
//!
//! This pin runs the rebuild's ACTUAL prepared statement, read back from
//! `pg_prepared_statements` on the one connection that ran it, under a
//! 64 kB `work_mem` so a sort of this modest fixture is forced to spill
//! the way the production one does. It asserts the plan never reads
//! `audit_log`, reads `event_facts` only through its subject index, sorts
//! nothing but a per-Subject top-N under its LIMIT (in memory), and
//! writes temp only for the Subject list — so nothing log-sized can
//! spill. The red run on the old SQL was a WindowAgg over an
//! external-merge Disk Sort, 760 temp blocks. It keeps the old query as a
//! semantic oracle on the same rows: the cap and the subject resolution
//! are conserved, only the shape changed. TestDb only, never a live
//! database.

#![cfg(feature = "postgres")]

use std::collections::BTreeSet;

use boss_search::rebuild_search;
use boss_testing::TestDb;
use serde_json::Value;
use sqlx::Row;

/// The ranked query this car replaced, kept ONLY as an oracle: what
/// rows the index held before. It reads `audit_log` and resolves the
/// Subject itself, exactly as `rebuild.rs` did until 832386eb.
const OLD_RANKED_EVENTS: &str = "\
    SELECT ranked.id::text AS ref_id, ranked.subject_kind, ranked.subject_id, \
           ranked.kind AS title, ranked.timestamp AS occurred_at \
    FROM ( \
        SELECT a.id, a.kind, a.timestamp, sub.subject_kind, sub.subject_id, \
               ROW_NUMBER() OVER ( \
                   PARTITION BY sub.subject_kind, sub.subject_id \
                   ORDER BY a.id DESC \
               ) AS rn \
        FROM audit_log a \
        LEFT JOIN LATERAL ( \
            SELECT field_path, target_kind, target_kind_path \
            FROM subject_edges WHERE source_kind = a.kind \
            ORDER BY field_path LIMIT 1 \
        ) se ON TRUE \
        LEFT JOIN jobs j \
          ON j.id::text = COALESCE(a.payload->>'job_id', a.payload->>'id') \
        LEFT JOIN LATERAL ( \
            SELECT k AS subject_kind, i AS subject_id \
            FROM (VALUES \
                (a.payload->>'subject_kind', a.payload->>'subject_id'), \
                (COALESCE(se.target_kind, \
                          a.payload #>> string_to_array(se.target_kind_path, '.')), \
                 a.payload #>> string_to_array(se.field_path, '.')), \
                (j.subject_kind, j.subject_id) \
            ) AS candidates(k, i) \
            WHERE k IS NOT NULL AND i IS NOT NULL LIMIT 1 \
        ) sub ON TRUE \
        WHERE sub.subject_id IS NOT NULL \
    ) ranked \
    WHERE ranked.rn <= 50";

type EventRow = (String, String, String, String, String);

fn children(plan: &Value) -> &[Value] {
    plan["Plans"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// Every plan node, each with its parent's node type.
fn nodes<'a>(plan: &'a Value, parent: &'a str, out: &mut Vec<(&'a str, &'a Value)>) {
    out.push((parent, plan));
    let me = plan["Node Type"].as_str().unwrap_or("");
    for child in children(plan) {
        nodes(child, me, out);
    }
}

/// Temp blocks this node wrote itself: EXPLAIN's buffer counts include
/// the node's children, so theirs are subtracted.
fn own_temp_written(plan: &Value) -> u64 {
    let total = plan["Temp Written Blocks"].as_u64().unwrap_or(0);
    let below: u64 = children(plan)
        .iter()
        .map(|c| c["Temp Written Blocks"].as_u64().unwrap_or(0))
        .sum();
    total.saturating_sub(below)
}

#[tokio::test]
async fn the_events_rebuild_ranks_from_the_subject_index_without_a_sort() {
    let db = TestDb::new().await;

    // 30,000 events: 24,000 across 1,200 quiet Subjects (20 each, under
    // the cap), 6,000 across 8 busy ones (about 720 each, so the cap of
    // 50 bites), and every 25th event names no Subject at all.
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         SELECT gen_random_uuid(), \
                TIMESTAMPTZ '2026-11-11 00:00:00Z' + i * INTERVAL '1 second', \
                'test-search-plan', 'test.search.signal', \
                CASE WHEN i % 25 = 0 THEN '{}'::jsonb \
                     WHEN i <= 24000 THEN jsonb_build_object( \
                         'subject_kind', 'asset', 'subject_id', 'quiet-' || (i % 1200)) \
                     ELSE jsonb_build_object( \
                         'subject_kind', 'account', 'subject_id', 'busy-' || (i % 8)) END \
         FROM generate_series(1, 30000) AS i",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    // The projection the rebuild reads, built by its own rebuilder.
    boss_views::rebuild_event_facts(&db.pool).await.unwrap();
    sqlx::raw_sql("ANALYZE audit_log; ANALYZE event_facts")
        .execute(&db.pool)
        .await
        .unwrap();

    // One connection, so the rebuild's prepared statements and the
    // EXPLAIN below share a session. Only the sort budget is reduced;
    // planner settings stay at their defaults.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*db.pool.connect_options()).clone())
        .await
        .unwrap();
    sqlx::raw_sql("SET work_mem = '64kB'")
        .execute(&pool)
        .await
        .unwrap();

    let report = rebuild_search(&pool).await.unwrap();

    // Conservation: the index holds exactly the rows the old ranked
    // query chose from the same log.
    let new_rows: BTreeSet<EventRow> = sqlx::query(
        "SELECT ref_id, subject_kind, subject_id, title, occurred_at::text AS occurred_at \
         FROM search_index WHERE ref_kind = 'event'",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| {
        (
            r.get("ref_id"),
            r.get("subject_kind"),
            r.get("subject_id"),
            r.get("title"),
            r.get("occurred_at"),
        )
    })
    .collect();
    let old_rows: BTreeSet<EventRow> = sqlx::query(&format!(
        "SELECT ref_id, subject_kind, subject_id, title, occurred_at::text AS occurred_at \
         FROM ({OLD_RANKED_EVENTS}) old"
    ))
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| {
        (
            r.get("ref_id"),
            r.get("subject_kind"),
            r.get("subject_id"),
            r.get("title"),
            r.get("occurred_at"),
        )
    })
    .collect();
    // 1,200 quiet Subjects keep all 20 minus the subject-less 25ths
    // (24,000 - 960), and 8 busy ones keep their newest 50 each.
    assert_eq!(old_rows.len(), 23_040 + 8 * 50, "the oracle's own count");
    assert_eq!(report.events_indexed, old_rows.len() as u64);
    assert_eq!(new_rows, old_rows, "the cap and resolution are conserved");

    // The statement that actually ran, read back from this session.
    // This lookup is itself a prepared statement whose text matches both
    // patterns, so it excludes itself by the view it reads.
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name::text FROM pg_prepared_statements \
         WHERE statement LIKE '%INSERT INTO search_index%' \
           AND statement LIKE '%''event''%' \
           AND statement NOT LIKE '%pg_prepared_statements%'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(names.len(), 1, "inspect the rebuild's one events statement");
    let name = &names[0];
    assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    sqlx::raw_sql("DELETE FROM search_index WHERE ref_kind = 'event'")
        .execute(&pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(&format!(
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) EXECUTE \"{name}\"(50)"
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    eprintln!("search events rebuild plan: {plan}");
    pool.close().await;

    let mut all = Vec::new();
    nodes(&plan[0]["Plan"], "", &mut all);

    // The log itself is never read: the resolution is event_facts'.
    assert!(
        all.iter()
            .all(|(_, n)| n["Relation Name"].as_str() != Some("audit_log")),
        "the events rebuild reads audit_log: {plan}"
    );
    // event_facts is read only through its subject index — a Subject
    // walk and a per-Subject newest-first read — never scanned whole.
    for (_, n) in &all {
        let scans_facts = n["Relation Name"].as_str() == Some("event_facts")
            || n["Index Name"]
                .as_str()
                .is_some_and(|i| i.starts_with("event_facts"));
        if scans_facts && n["Node Type"].as_str() != Some("Bitmap Heap Scan") {
            assert_eq!(
                n["Index Name"].as_str(),
                Some("event_facts_subject"),
                "event_facts read off its subject index: {n}"
            );
        }
    }
    // The only sort allowed is a top-N directly under the per-Subject
    // LIMIT: at most the cap's rows, held in memory. A Sort anywhere
    // else — the old WindowAgg's input — is a sort of the log.
    for (parent, n) in &all {
        if matches!(n["Node Type"].as_str(), Some("Sort" | "Incremental Sort")) {
            assert_eq!(*parent, "Limit", "a sort that is not a top-N: {plan}");
            assert_eq!(
                n["Sort Space Type"].as_str(),
                Some("Memory"),
                "a top-N that spilled: {n}"
            );
        }
    }
    // Nothing log-sized reaches a temp file. The one thing held is the
    // Subject list (one narrow row per Subject, the recursive CTE's
    // tuplestore), which may spill at this test's 64 kB work_mem; no
    // other node may write a temp block.
    for (_, n) in &all {
        if n["Node Type"].as_str() != Some("CTE Scan") {
            assert_eq!(
                own_temp_written(n),
                0,
                "a node other than the Subject list writes temp: {n}"
            );
        }
    }
}
