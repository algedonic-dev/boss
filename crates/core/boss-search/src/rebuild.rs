//! Rebuild `search_index` from the core identity tables, which are
//! themselves projections of `audit_log`.
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
    // The candidate filter requires the COMPLETE (kind, id) pair. A
    // candidate can resolve an id with no kind (a payload carrying
    // `subject_id` alone, or a subject_edges row whose target_kind and
    // target_kind_path both come up empty) — and `NULL || ' ' || id`
    // is NULL, so one such event nulled `body` and aborted the whole
    // rebuild on the NOT NULL constraint. Measured 2026-08-19: the
    // reindex timer had been crash-looping on exactly this while the
    // index it was rebuilding fell behind.
    // Subject resolution matches boss-views' event_facts exactly:
    // flat keys, then the `subject_edges` registry, then the Job the
    // event names — with kind and id taken as a pair from whichever
    // source produced the id. Reading only the flat keys left this
    // index holding 3,694 event rows against 776,629 in the log, so
    // "what happened to this Subject" was answerable for almost
    // nothing.
    let events = sqlx::query(
        "INSERT INTO search_index \
            (ref_kind, ref_id, subject_kind, subject_id, title, body, occurred_at) \
         SELECT 'event', ranked.id::text, ranked.subject_kind, ranked.subject_id, \
                ranked.kind, ranked.subject_kind || ' ' || ranked.subject_id, \
                ranked.timestamp \
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
         WHERE ranked.rn <= $1",
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
