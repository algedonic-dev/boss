//! Postgres `ViewsRepo`.

use async_trait::async_trait;
use sqlx::{PgPool, Row, postgres::PgRow};

use crate::error::ViewsError;
use crate::filter;
use crate::port::ViewsRepo;
use crate::types::{View, ViewInput, ViewLayout, ViewSource, Visibility};

pub struct PgViewsRepo {
    pool: PgPool,
}

impl PgViewsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const COLS: &str = "id, owner_id, title, source, filter, columns, layout, visibility, \
                    created_at, updated_at";

fn storage(e: sqlx::Error) -> ViewsError {
    ViewsError::Storage(e.to_string())
}

fn row_to_view(r: &PgRow) -> Result<View, ViewsError> {
    let source_raw: String = r.try_get("source").map_err(storage)?;
    let layout_raw: String = r.try_get("layout").map_err(storage)?;
    let vis_raw: String = r.try_get("visibility").map_err(storage)?;
    // A stored value the enums don't know is a schema/code mismatch,
    // not a row to guess at. Say which value and which column.
    let source = ViewSource::parse(&source_raw)
        .ok_or_else(|| ViewsError::Storage(format!("unknown view source {source_raw:?}")))?;
    let layout = ViewLayout::parse(&layout_raw)
        .ok_or_else(|| ViewsError::Storage(format!("unknown view layout {layout_raw:?}")))?;
    let visibility = Visibility::parse(&vis_raw)
        .ok_or_else(|| ViewsError::Storage(format!("unknown visibility {vis_raw:?}")))?;
    Ok(View {
        id: r.try_get("id").map_err(storage)?,
        owner_id: r.try_get("owner_id").map_err(storage)?,
        title: r.try_get("title").map_err(storage)?,
        source,
        filter: r.try_get("filter").map_err(storage)?,
        columns: r.try_get("columns").map_err(storage)?,
        layout,
        visibility,
        created_at: r.try_get("created_at").map_err(storage)?,
        updated_at: r.try_get("updated_at").map_err(storage)?,
    })
}

#[async_trait]
impl ViewsRepo for PgViewsRepo {
    async fn list_for_viewer(&self, viewer_id: &str) -> Result<Vec<View>, ViewsError> {
        let sql = format!(
            "SELECT {COLS} FROM views \
             WHERE owner_id = $1 OR visibility = 'shared' \
             ORDER BY updated_at DESC"
        );
        let rows = sqlx::query(&sql)
            .bind(viewer_id)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter().map(row_to_view).collect()
    }

    async fn get_for_viewer(&self, id: &str, viewer_id: &str) -> Result<View, ViewsError> {
        // Ownership is in the WHERE clause, not a post-fetch check: a
        // row the caller may not see never leaves the database, and
        // the miss is indistinguishable from a bad id.
        let sql = format!(
            "SELECT {COLS} FROM views \
             WHERE id = $1 AND (owner_id = $2 OR visibility = 'shared')"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(viewer_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?
            .ok_or_else(|| ViewsError::NotFound(id.to_string()))?;
        row_to_view(&row)
    }

    async fn create(&self, owner_id: &str, input: &ViewInput) -> Result<View, ViewsError> {
        // Reject a malformed filter before it reaches storage, so it
        // fails for its author rather than for whoever opens it later.
        filter::compile(&input.filter)?;
        let sql = format!(
            "INSERT INTO views (id, owner_id, title, source, filter, columns, layout, visibility) \
             VALUES (gen_random_uuid()::text, $1, $2, $3, $4, $5, $6, $7) RETURNING {COLS}"
        );
        let row = sqlx::query(&sql)
            .bind(owner_id)
            .bind(&input.title)
            .bind(input.source.as_str())
            .bind(&input.filter)
            .bind(input.columns.as_slice())
            .bind(input.layout.as_str())
            .bind(input.visibility.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
        row_to_view(&row)
    }

    async fn replace(
        &self,
        id: &str,
        owner_id: &str,
        input: &ViewInput,
    ) -> Result<View, ViewsError> {
        filter::compile(&input.filter)?;
        // `owner_id` is a WHERE term, never a SET term: it scopes the
        // update to a View this caller owns and cannot transfer
        // ownership. Shared means readable, not writable.
        let sql = format!(
            "UPDATE views SET title = $3, source = $4, filter = $5, \
                    columns = $6, layout = $7, visibility = $8, updated_at = NOW() \
             WHERE id = $1 AND owner_id = $2 RETURNING {COLS}"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(owner_id)
            .bind(&input.title)
            .bind(input.source.as_str())
            .bind(&input.filter)
            .bind(input.columns.as_slice())
            .bind(input.layout.as_str())
            .bind(input.visibility.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?
            .ok_or_else(|| ViewsError::NotFound(id.to_string()))?;
        row_to_view(&row)
    }

    async fn delete(&self, id: &str, owner_id: &str) -> Result<(), ViewsError> {
        let res = sqlx::query("DELETE FROM views WHERE id = $1 AND owner_id = $2")
            .bind(id)
            .bind(owner_id)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if res.rows_affected() == 0 {
            return Err(ViewsError::NotFound(id.to_string()));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::os_map::OsMapRepo for PgViewsRepo {
    async fn os_map(&self, limit: i64) -> Result<crate::os_map::OsMap, ViewsError> {
        use crate::os_map::{OsMap, OsMapEdge, classify, nodes_from_edges};

        // One pass: take the most recent `limit` step completions,
        // pair each with the previous completion on the SAME Job
        // (that pairing IS the handoff), resolve both actors to a
        // department, and aggregate.
        //
        // `lag` partitions by job_id so a handoff never spans two
        // Jobs. Automation collapses to one `dispatcher` node rather
        // than one node per rule — `/it/dispatcher` is the drill-down
        // for what is inside it.
        let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "WITH recent AS (
                 SELECT audit_id,
                        payload->>'job_id'   AS job_id,
                        payload->>'_actor'   AS actor
                 FROM event_facts
                 WHERE kind = 'jobs.step.completed'
                   AND payload->>'job_id' IS NOT NULL
                 ORDER BY audit_id DESC
                 LIMIT $1
             ),
             paired AS (
                 -- Sim-ness comes from the JOB, not the event. A Job is
                 -- simulated or real from creation and immutably so, so a
                 -- real operator clicking a simulated Job does not make
                 -- that handoff real. Reading the event's own marker had
                 -- the map disagreeing with the epoch trim about the same
                 -- traffic — two surfaces answering one question two ways.
                 SELECT r.actor,
                        COALESCE(j.simulated, false) AS simulated,
                        LAG(r.actor) OVER (PARTITION BY r.job_id ORDER BY r.audit_id)
                            AS prev_actor
                 FROM recent r
                 LEFT JOIN jobs j ON j.id::text = r.job_id
             ),
             resolved AS (
                 SELECT
                     COALESCE(ep.department,
                              CASE WHEN p.prev_actor LIKE 'automation:%'
                                   THEN 'dispatcher' ELSE 'unresolved' END) AS src,
                     COALESCE(ea.department,
                              CASE WHEN p.actor LIKE 'automation:%'
                                   THEN 'dispatcher' ELSE 'unresolved' END) AS dst,
                     p.simulated
                 FROM paired p
                 LEFT JOIN employees ep ON ep.id = p.prev_actor
                 LEFT JOIN employees ea ON ea.id = p.actor
                 WHERE p.prev_actor IS NOT NULL
             )
             SELECT src, dst,
                    COUNT(*)::bigint AS handoffs,
                    COUNT(*) FILTER (WHERE simulated)::bigint AS simulated
             FROM resolved
             GROUP BY src, dst
             ORDER BY handoffs DESC",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        let edges: Vec<OsMapEdge> = rows
            .into_iter()
            .map(|(source, target, handoffs, simulated)| OsMapEdge {
                source,
                target,
                handoffs,
                simulated,
            })
            .collect();

        // Labels come from the Class registry, which already owns the
        // tenant's department vocabulary — `it` is "IT" and `qa` is
        // "QA" there. Humanising the code here instead produced "It"
        // and "Qa": a second, worse copy of a fact the registry
        // already holds (CLAUDE.md §9a). `classify` stays as the
        // fallback for the reserved ids and for a department with no
        // Class row.
        let labels: Vec<(String, String)> = sqlx::query_as(
            "SELECT code, display_name FROM classes
             WHERE subject_kind = 'employee' AND member_attribute = 'department'",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;
        let labels: std::collections::HashMap<String, String> = labels.into_iter().collect();

        let handoffs_considered = edges.iter().map(|e| e.handoffs).sum();
        let high_water: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(audit_id), 0) FROM event_facts")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| ViewsError::Storage(e.to_string()))?;

        Ok(OsMap {
            nodes: nodes_from_edges(&edges, |id| match labels.get(id) {
                Some(display) => (display.clone(), crate::os_map::NodeKind::Department),
                None => classify(id),
            }),
            edges,
            handoffs_considered,
            high_water,
        })
    }
}
