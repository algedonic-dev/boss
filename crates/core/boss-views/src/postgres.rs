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
