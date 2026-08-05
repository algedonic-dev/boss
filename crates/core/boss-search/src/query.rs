//! The query — one round trip that returns a Subject with the Jobs
//! about it and the events behind those.
//!
//! Q3 in docs/design/global-search.md refused a Subjects-only v1: it
//! ships sooner and demonstrates nothing a conventional search box
//! does not, while setting the expectation that search IS a name
//! lookup. So the unified shape is here from the first release.

use sqlx::{PgPool, Row};

use crate::error::SearchError;
use crate::types::{RefKind, SearchResults, SearchRow, SubjectHit};

/// Cap on each result group. A dropdown shows a handful; the full
/// results surface pages. Deliberately not configurable per-caller yet
/// — one number is easier to reason about than a knob nobody tunes.
const GROUP_LIMIT: i64 = 10;
/// Event preview per Subject hit. The count comes back separately, so
/// the UI can say "3 of 214" rather than implying this is all of them.
const EVENT_PREVIEW: i64 = 5;

fn row_to_search_row(r: &sqlx::postgres::PgRow) -> Result<SearchRow, SearchError> {
    let kind: String = r.try_get("ref_kind").map_err(SearchError::storage)?;
    Ok(SearchRow {
        ref_kind: RefKind::parse(&kind)
            .ok_or_else(|| SearchError::Storage(format!("unknown ref_kind `{kind}`")))?,
        ref_id: r.try_get("ref_id").map_err(SearchError::storage)?,
        subject_kind: r.try_get("subject_kind").map_err(SearchError::storage)?,
        subject_id: r.try_get("subject_id").map_err(SearchError::storage)?,
        title: r.try_get("title").map_err(SearchError::storage)?,
        body: r.try_get("body").map_err(SearchError::storage)?,
        occurred_at: r.try_get("occurred_at").map_err(SearchError::storage)?,
    })
}

/// Run a query.
///
/// `app_subject_kinds`, when non-empty, floats Subjects of those kinds
/// to the top — Q4's "prioritise results from the app you are in". It
/// filters nothing: the whole point of a global box is that it still
/// finds the thing when you are looking in the wrong place.
pub async fn search(
    pool: &PgPool,
    query: &str,
    app_subject_kinds: &[String],
) -> Result<SearchResults, SearchError> {
    let q = query.trim();
    if q.is_empty() {
        return Err(SearchError::BadRequest("query is empty".into()));
    }

    // Full-text for prose, prefix for ids — an operator pasting
    // `inv-step-c9cd8f…` is not writing English, and `to_tsquery`
    // tokenises that into something that matches nothing.
    let sql = "\
        SELECT ref_kind, ref_id, subject_kind, subject_id, title, body, occurred_at, \
               ts_rank(tsv, plainto_tsquery('english', $1)) AS rank \
        FROM search_index \
        WHERE (tsv @@ plainto_tsquery('english', $1) \
               OR ref_id ILIKE '%' || $1 || '%' \
               OR title ILIKE '%' || $1 || '%') \
          AND ref_kind = $2 \
        ORDER BY \
          CASE WHEN $3::text[] IS NULL OR cardinality($3::text[]) = 0 THEN 0 \
               WHEN subject_kind = ANY($3::text[]) THEN 0 ELSE 1 END, \
          rank DESC, occurred_at DESC NULLS LAST \
        LIMIT $4";

    let mut out = SearchResults {
        query: q.to_string(),
        ..Default::default()
    };

    // --- Subjects, each with its work and its history -------------
    let subject_rows = sqlx::query(sql)
        .bind(q)
        .bind(RefKind::Subject.as_str())
        .bind(app_subject_kinds)
        .bind(GROUP_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(SearchError::storage)?;

    for r in &subject_rows {
        let row = row_to_search_row(r)?;
        let (sk, si) = match (row.subject_kind.clone(), row.subject_id.clone()) {
            (Some(k), Some(i)) => (k, i),
            _ => continue,
        };

        let jobs = sqlx::query(
            "SELECT ref_kind, ref_id, subject_kind, subject_id, title, body, occurred_at \
             FROM search_index \
             WHERE ref_kind = 'job' AND subject_kind = $1 AND subject_id = $2 \
             ORDER BY occurred_at DESC NULLS LAST LIMIT $3",
        )
        .bind(&sk)
        .bind(&si)
        .bind(GROUP_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(SearchError::storage)?;

        let events = sqlx::query(
            "SELECT ref_kind, ref_id, subject_kind, subject_id, title, body, occurred_at \
             FROM search_index \
             WHERE ref_kind = 'event' AND subject_kind = $1 AND subject_id = $2 \
             ORDER BY occurred_at DESC NULLS LAST LIMIT $3",
        )
        .bind(&sk)
        .bind(&si)
        .bind(EVENT_PREVIEW)
        .fetch_all(pool)
        .await
        .map_err(SearchError::storage)?;

        let event_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM search_index \
             WHERE ref_kind = 'event' AND subject_kind = $1 AND subject_id = $2",
        )
        .bind(&sk)
        .bind(&si)
        .fetch_one(pool)
        .await
        .map_err(SearchError::storage)?;

        out.subjects.push(SubjectHit {
            subject_kind: sk,
            subject_id: si,
            title: row.title,
            jobs: jobs
                .iter()
                .map(row_to_search_row)
                .collect::<Result<Vec<_>, _>>()?,
            events: events
                .iter()
                .map(row_to_search_row)
                .collect::<Result<Vec<_>, _>>()?,
            event_count,
        });
    }

    // --- Jobs and events that matched on their own ----------------
    // A Job whose Subject also matched is already shown under that
    // Subject; repeating it here would make one thing look like two.
    let seen: Vec<String> = out
        .subjects
        .iter()
        .flat_map(|s| s.jobs.iter().map(|j| j.ref_id.clone()))
        .collect();

    for r in sqlx::query(sql)
        .bind(q)
        .bind(RefKind::Job.as_str())
        .bind(app_subject_kinds)
        .bind(GROUP_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(SearchError::storage)?
        .iter()
    {
        let row = row_to_search_row(r)?;
        if !seen.contains(&row.ref_id) {
            out.jobs.push(row);
        }
    }

    for r in sqlx::query(sql)
        .bind(q)
        .bind(RefKind::Event.as_str())
        .bind(app_subject_kinds)
        .bind(GROUP_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(SearchError::storage)?
        .iter()
    {
        out.events.push(row_to_search_row(r)?);
    }

    Ok(out)
}
