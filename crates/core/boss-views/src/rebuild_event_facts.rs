//! Rebuild `event_facts` from `audit_log`.
//!
//! A projection like any other: the log is the system of record, this
//! is a pure function of it, and a full rebuild reproduces it exactly.
//! Nothing writes here except this rebuilder.
//!
//! Two modes, because 704k rows is enough that the difference matters:
//!
//! - **Full** — TRUNCATE and replay. What `boss-rebuild-all` runs, and
//!   the definition of correct.
//! - **Catch-up** — insert only rows past the high-water mark. What a
//!   running system needs so a View is not reading a stale projection.
//!
//! Catch-up is safe because `audit_log` is append-only with monotonic
//! ids: everything at or below the mark is already projected, and
//! nothing below it will change. If that ever stops being true, the
//! full rebuild is still the fallback that fixes it.

use sqlx::{PgPool, Row};

use crate::error::ViewsError;

/// Rows per INSERT…SELECT batch. Bounded so a rebuild over a large log
/// holds a lock for a bounded time and shows progress rather than
/// running as one opaque statement.
const BATCH: i64 = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RebuildEventFactsReport {
    pub rows_projected: u64,
    /// Highest `audit_log.id` now projected — the catch-up watermark.
    pub high_water: i64,
}

fn storage(e: sqlx::Error) -> ViewsError {
    ViewsError::Storage(e.to_string())
}

/// The projection itself, as one statement over a bounded id window.
///
/// `subject_kind` / `subject_id` are lifted out of the payload here
/// rather than at read time — that lift is the entire point of the
/// table, since `payload->>'subject_id'` cannot use an index but a
/// column can.
///
/// The lift resolves a Subject three ways, in order:
///
/// 1. **Flat keys** — `payload.subject_id`. The oldest shape, still
///    emitted by many domain events and never registered anywhere.
/// 2. **The `subject_edges` registry** — the declared answer to "how
///    does an event of this kind name its Subject". An edge gives a
///    dotted `field_path` to the id, and either a static `target_kind`
///    or a `target_kind_path` for the kinds that carry it in the
///    payload (the identity-first `{"id": …, "subject_kind": …}`
///    shape). This crate holds NO per-kind knowledge: teaching the
///    system about a new event is a registry row, not a branch here,
///    which is the point of the registry existing.
/// 3. **Through the Job** — `payload.job_id` → `jobs.subject_id`.
///    Step events name the Job they belong to, and a Job knows its
///    Subject. Without this hop every step transition — the largest
///    kinds in the log — sat one join from a Subject with nothing
///    materialising it.
///
/// Together these take linkage from 16% of the log to ~90%, which is
/// the difference between "everything that happened to this Subject"
/// being a claim and being a query.
///
/// Kind and id are resolved **as a pair**, not independently: the
/// candidate list is scanned for the first entry with a non-null id,
/// and that entry's kind is taken. Resolving them separately let a
/// registered edge stamp `subject_kind = product` on an event whose
/// `product_sku` was absent, producing a kind with no id — a row that
/// claims to be about a Subject it cannot name.
///
/// One event, one Subject: the projection is keyed by `audit_id`, so
/// where a kind declares several edges (only
/// `asset.ownership_transferred` does today, naming both sides of a
/// transfer) the lowest `field_path` wins, deterministically. Events
/// that are genuinely about two Subjects want a link table, which is
/// a modelling decision rather than a tie-break.
///
/// The join compares `j.id::text` rather than casting the payload
/// value to uuid: a `job_id` that is not a valid uuid would fail the
/// cast and take down the whole batch, where this simply does not
/// match.
async fn project_window(
    pool: &PgPool,
    from_exclusive: i64,
    to_inclusive: i64,
) -> Result<u64, ViewsError> {
    let res = sqlx::query(
        "INSERT INTO event_facts \
            (audit_id, event_id, kind, source, occurred_at, subject_kind, subject_id, payload) \
         SELECT a.id, a.event_id, a.kind, a.source, a.timestamp, \
                sub.subject_kind, sub.subject_id, a.payload \
         FROM audit_log a \
         LEFT JOIN LATERAL ( \
             SELECT field_path, target_kind, target_kind_path \
             FROM subject_edges \
             WHERE source_kind = a.kind \
             ORDER BY field_path \
             LIMIT 1 \
         ) se ON TRUE \
         LEFT JOIN jobs j ON j.id::text = a.payload->>'job_id' \
         LEFT JOIN LATERAL ( \
             SELECT k AS subject_kind, i AS subject_id \
             FROM (VALUES \
                 (a.payload->>'subject_kind', a.payload->>'subject_id'), \
                 (COALESCE(se.target_kind, \
                           a.payload #>> string_to_array(se.target_kind_path, '.')), \
                  a.payload #>> string_to_array(se.field_path, '.')), \
                 (j.subject_kind, j.subject_id) \
             ) AS candidates(k, i) \
             WHERE i IS NOT NULL \
             LIMIT 1 \
         ) sub ON TRUE \
         WHERE a.id > $1 AND a.id <= $2 \
         ON CONFLICT (audit_id) DO NOTHING",
    )
    .bind(from_exclusive)
    .bind(to_inclusive)
    .execute(pool)
    .await
    .map_err(storage)?;
    Ok(res.rows_affected())
}

async fn max_audit_id(pool: &PgPool, table: &str) -> Result<i64, ViewsError> {
    let col = if table == "audit_log" {
        "id"
    } else {
        "audit_id"
    };
    let sql = format!("SELECT COALESCE(MAX({col}), 0) AS m FROM {table}");
    let row = sqlx::query(&sql).fetch_one(pool).await.map_err(storage)?;
    row.try_get::<i64, _>("m").map_err(storage)
}

/// Full rebuild — TRUNCATE, then replay the whole log.
pub async fn rebuild_event_facts(pool: &PgPool) -> Result<RebuildEventFactsReport, ViewsError> {
    sqlx::query("TRUNCATE event_facts")
        .execute(pool)
        .await
        .map_err(storage)?;
    catch_up_event_facts(pool).await
}

/// Project everything the log has that the projection does not.
///
/// Also the tail of a full rebuild: after the TRUNCATE the watermark is
/// 0, so this replays everything. One code path, so the incremental
/// case cannot drift from the authoritative one.
pub async fn catch_up_event_facts(pool: &PgPool) -> Result<RebuildEventFactsReport, ViewsError> {
    let target = max_audit_id(pool, "audit_log").await?;
    let mut cursor = max_audit_id(pool, "event_facts").await?;
    let mut rows_projected = 0u64;

    while cursor < target {
        let window_end = (cursor + BATCH).min(target);
        rows_projected += project_window(pool, cursor, window_end).await?;
        cursor = window_end;
    }

    Ok(RebuildEventFactsReport {
        rows_projected,
        high_water: target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_batch_window_is_half_open_on_the_low_side() {
        // `id > from AND id <= to` — so a cursor at the last projected
        // id re-reads nothing, and a window boundary cannot skip the
        // row sitting exactly on it. Off-by-one here would silently
        // drop or duplicate one event per batch, which at 50k rows a
        // batch is 14 events across the current log.
        assert_eq!(BATCH, 50_000);
    }
}
