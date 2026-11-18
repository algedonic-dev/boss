//! Rebuild `search_index` from the core identity tables and
//! `event_facts`, which are themselves projections of `audit_log`.
//!
//! TRUNCATE-then-replay, one transaction, advisory-locked — the same
//! shape as every other rebuilder, and registered in `boss-rebuild-all`
//! after them so it reads their replayed output.
//!
//! Why this exists rather than a query over the domain tables: the
//! previous `search_all()` UNION-ed seven Tier-2 tables live. That is
//! cheaper and it is what most systems do, but it makes search a view
//! of whatever the domain projections currently say. Q2 in
//! docs/architecture-decisions.md §Search chose a projection instead, so search
//! reproduces from the log rather than drifting from it, and a thing
//! absent from a domain table is still findable.

use sqlx::PgPool;

use crate::error::SearchError;

const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("search-index");

#[derive(Debug, Clone, Default)]
pub struct RebuildSearchReport {
    pub subjects_indexed: u64,
    pub jobs_indexed: u64,
    pub events_indexed: u64,
}

/// How many recent events to index per Subject.
///
/// The log is the largest table in the system — 450k rows in three sim
/// months, millions across a lap — and indexing all of it would make
/// the index bigger than the thing it indexes for no search value: an
/// operator looking for "what happened to this account" wants the
/// recent tail, not every row since the epoch. The cap is per-Subject
/// rather than global so a quiet account keeps its full history while a
/// busy one keeps its most recent.
const EVENTS_PER_SUBJECT: i64 = 50;

pub async fn rebuild_search(pool: &PgPool) -> Result<RebuildSearchReport, SearchError> {
    let mut tx = pool.begin().await.map_err(SearchError::storage)?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(SearchError::storage)?;

    // No log-complete check (design b046f510): search_index has no live
    // writer, so an undrained fact lands one ten-minute reindex late,
    // never lost — and locking event_outbox every ten minutes would
    // stall every writer for nothing.
    sqlx::query("TRUNCATE search_index")
        .execute(&mut *tx)
        .await
        .map_err(SearchError::storage)?;

    // --- Subjects -------------------------------------------------
    // `label` is the human-readable name where a kind carries one
    // (accounts, employees, vendors, products all do); id is always
    // matchable, which is what makes an invoice number or asset tag
    // findable for the kinds that carry no label.
    let subjects = sqlx::query(
        "INSERT INTO search_index \
            (ref_kind, ref_id, subject_kind, subject_id, title, body, occurred_at) \
         SELECT 'subject', s.kind || ':' || s.id, s.kind, s.id, \
                COALESCE(NULLIF(s.label, ''), s.id), \
                s.kind || ' ' || s.id, \
                s.created_at \
         FROM subjects s \
         WHERE s.retired_at IS NULL",
    )
    .execute(&mut *tx)
    .await
    .map_err(SearchError::storage)?;

    // --- Jobs -----------------------------------------------------
    let jobs = sqlx::query(
        "INSERT INTO search_index \
            (ref_kind, ref_id, subject_kind, subject_id, title, body, occurred_at) \
         SELECT 'job', j.id, j.subject_kind, j.subject_id, \
                COALESCE(NULLIF(j.title, ''), j.kind), \
                j.kind || ' ' || j.status, \
                j.created_at \
         FROM jobs j",
    )
    .execute(&mut *tx)
    .await
    .map_err(SearchError::storage)?;

    // --- Events ---------------------------------------------------
    // Capped per Subject (see EVENTS_PER_SUBJECT). Events whose payload
    // names no subject are skipped rather than indexed subject-less:
    // an event you cannot trace to a thing is not a search result, it
    // is noise.
    //
    // A COMPLETE (kind, id) pair is required. A row with an id and no
    // kind (a payload carrying `subject_id` alone) would make
    // `NULL || ' ' || id` NULL, and one such event nulled `body` and
    // aborted the whole rebuild on the NOT NULL constraint — measured
    // 2026-08-19, a reindex timer crash-looping while the index fell
    // behind.
    //
    // Read from `event_facts`, not `audit_log` (backlog 832386eb,
    // 2026-11-11). Until then this statement resolved every log row's
    // Subject itself and ranked the whole log with ROW_NUMBER() OVER
    // (PARTITION BY subject ORDER BY id DESC) to keep 50 each: a sort of
    // the entire log, every hour, which wrote 13.0 GB of temp files per
    // run (47.6 M temp blocks over 30 calls, queryid
    // 3112194067547126450) onto a pgdata volume with 5.7 GiB free.
    // `event_facts` already holds that resolution — flat keys, then the
    // `subject_edges` registry, then the Job the event names — and its
    // `event_facts_subject (subject_kind, subject_id, audit_id DESC)`
    // index IS the order the cap needs. So the Subjects are walked off
    // that index one at a time (each step a LIMIT 1 past the last), and
    // each one's newest 50 are a LIMIT read off the same index. Nothing
    // log-sized is sorted or held: the planner may still top-N a single
    // Subject's rows in memory under its LIMIT, and the one thing kept
    // is the Subject list, one narrow row per Subject. Pinned by
    // tests/rebuild_events_plan_pg.rs.
    //
    // Two differences, both accepted. Freshness: `event_facts` trails
    // the log by one `boss-views-catchup` (every five minutes), inside
    // the hour this index already lags. Resolution: event_facts takes
    // the first candidate with an id, where this took the first with a
    // kind AND an id, so an event whose first id-bearing candidate has
    // no kind is skipped here rather than falling through to a later
    // source.
    let events = sqlx::query(
        "WITH RECURSIVE subject AS ( \
             (SELECT f.subject_kind, f.subject_id FROM event_facts f \
              WHERE f.subject_kind IS NOT NULL AND f.subject_id IS NOT NULL \
              ORDER BY f.subject_kind, f.subject_id LIMIT 1) \
             UNION ALL \
             SELECT next.subject_kind, next.subject_id \
             FROM subject previous CROSS JOIN LATERAL ( \
                 SELECT f.subject_kind, f.subject_id FROM event_facts f \
                 WHERE f.subject_kind IS NOT NULL AND f.subject_id IS NOT NULL \
                   AND (f.subject_kind, f.subject_id) \
                       > (previous.subject_kind, previous.subject_id) \
                 ORDER BY f.subject_kind, f.subject_id LIMIT 1 \
             ) next \
         ) \
         INSERT INTO search_index \
            (ref_kind, ref_id, subject_kind, subject_id, title, body, occurred_at) \
         SELECT 'event', newest.audit_id::text, s.subject_kind, s.subject_id, \
                newest.kind, s.subject_kind || ' ' || s.subject_id, \
                newest.occurred_at \
         FROM subject s CROSS JOIN LATERAL ( \
             SELECT f.audit_id, f.kind, f.occurred_at FROM event_facts f \
             WHERE f.subject_kind = s.subject_kind AND f.subject_id = s.subject_id \
             ORDER BY f.audit_id DESC LIMIT $1 \
         ) newest",
    )
    .bind(EVENTS_PER_SUBJECT)
    .execute(&mut *tx)
    .await
    .map_err(SearchError::storage)?;

    tx.commit().await.map_err(SearchError::storage)?;

    Ok(RebuildSearchReport {
        subjects_indexed: subjects.rows_affected(),
        jobs_indexed: jobs.rows_affected(),
        events_indexed: events.rows_affected(),
    })
}
