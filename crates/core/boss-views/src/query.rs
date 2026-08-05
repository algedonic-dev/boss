//! Resolving a View to rows.
//!
//! The shape is deliberately dull: pull candidate rows from the same
//! projections every other surface reads, hand each to the filter,
//! keep what matches. No query planner, no generated SQL, no operator
//! string reaching the database.
//!
//! Filtering happens in this process rather than in SQL. That costs a
//! wider scan, and it buys the property the design cares about: the
//! filter is a `boss-expr` predicate over a JSON row, identical to the
//! predicates in dispatcher rules and step `ready_when`, and no part
//! of an operator's text is ever concatenated into a statement.

use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::{PgPool, Row, postgres::PgRow};

use crate::error::ViewsError;
use crate::filter;
use crate::port::ViewResolver;
use crate::types::{View, ViewResults, ViewSource};

/// How many candidate rows a single View may scan before it stops.
///
/// The filter runs in this process, so the scan has to be bounded by
/// something. When the ceiling is hit the result says so
/// (`truncated`) rather than presenting a short answer as a complete
/// one.
pub const SCAN_CEILING: i64 = 5_000;

/// A column that would not decode means this code and the schema
/// disagree. That is worth an error naming the column, never a panic
/// inside a request handler.
fn dec(e: sqlx::Error) -> ViewsError {
    ViewsError::Storage(format!("decoding view source row: {e}"))
}

fn text(r: &PgRow, col: &str) -> Result<String, ViewsError> {
    r.try_get::<String, _>(col).map_err(dec)
}

fn opt_text(r: &PgRow, col: &str) -> Result<Option<String>, ViewsError> {
    r.try_get::<Option<String>, _>(col).map_err(dec)
}

/// UUID columns (`jobs.id`, `audit_log.event_id`) render as strings so
/// a filter can compare them the way an operator writes them.
fn uuid_text(r: &PgRow, col: &str) -> Result<String, ViewsError> {
    r.try_get::<uuid::Uuid, _>(col)
        .map(|u| u.to_string())
        .map_err(dec)
}

fn ts(r: &PgRow, col: &str) -> Result<String, ViewsError> {
    r.try_get::<chrono::DateTime<chrono::Utc>, _>(col)
        .map(|t| t.to_rfc3339())
        .map_err(dec)
}

fn opt_ts(r: &PgRow, col: &str) -> Result<Option<String>, ViewsError> {
    r.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(col)
        .map(|t| t.map(|t| t.to_rfc3339()))
        .map_err(dec)
}

fn opt_date(r: &PgRow, col: &str) -> Result<Option<String>, ViewsError> {
    r.try_get::<Option<chrono::NaiveDate>, _>(col)
        .map(|d| d.map(|d| d.to_string()))
        .map_err(dec)
}

pub struct PgViewResolver {
    pool: PgPool,
}

impl PgViewResolver {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Candidate rows for a source, newest first, as JSON objects.
    ///
    /// Newest-first is the only ordering offered: it is the one an
    /// operator can predict without being told, and a View whose row
    /// order depends on an unstated rule is a View whose results
    /// change for reasons nobody can see.
    async fn candidates(&self, source: ViewSource) -> Result<Vec<Value>, ViewsError> {
        let storage = |e: sqlx::Error| ViewsError::Storage(e.to_string());
        match source {
            ViewSource::Subjects => {
                let rows = sqlx::query(
                    "SELECT kind, id, label, created_at, retired_at \
                     FROM subjects ORDER BY created_at DESC LIMIT $1",
                )
                .bind(SCAN_CEILING)
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
                rows.iter()
                    .map(|r| {
                        Ok(json!({
                            "kind": text(r, "kind")?,
                            "id": text(r, "id")?,
                            "label": opt_text(r, "label")?,
                            "created_at": ts(r, "created_at")?,
                            "retired_at": opt_ts(r, "retired_at")?,
                        }))
                    })
                    .collect()
            }
            ViewSource::Jobs => {
                let rows = sqlx::query(
                    "SELECT id, kind, subject_kind, subject_id, title, owner_id, status, \
                            priority, opened_on, closed_on, tags, created_at \
                     FROM jobs ORDER BY created_at DESC LIMIT $1",
                )
                .bind(SCAN_CEILING)
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
                rows.iter()
                    .map(|r| {
                        Ok(json!({
                            // jobs.id is a UUID column, not text.
                            "id": uuid_text(r, "id")?,
                            "kind": text(r, "kind")?,
                            "subject_kind": opt_text(r, "subject_kind")?,
                            "subject_id": opt_text(r, "subject_id")?,
                            "title": opt_text(r, "title")?,
                            "owner_id": opt_text(r, "owner_id")?,
                            "status": text(r, "status")?,
                            "priority": opt_text(r, "priority")?,
                            "opened_on": opt_date(r, "opened_on")?,
                            "closed_on": opt_date(r, "closed_on")?,
                            "tags": r
                                .try_get::<Option<Vec<String>>, _>("tags")
                                .map_err(dec)?,
                            "created_at": ts(r, "created_at")?,
                        }))
                    })
                    .collect()
            }
            ViewSource::Events => {
                let rows = sqlx::query(
                    "SELECT id, event_id, kind, source, timestamp, payload \
                     FROM audit_log ORDER BY id DESC LIMIT $1",
                )
                .bind(SCAN_CEILING)
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
                rows.iter()
                    .map(|r| {
                        Ok(json!({
                            "id": r.try_get::<i64, _>("id").map_err(dec)?,
                            // audit_log.event_id is a UUID column.
                            "event_id": uuid_text(r, "event_id")?,
                            "kind": text(r, "kind")?,
                            "source": opt_text(r, "source")?,
                            "timestamp": ts(r, "timestamp")?,
                            "payload": r
                                .try_get::<Option<Value>, _>("payload")
                                .map_err(dec)?,
                        }))
                    })
                    .collect()
            }
        }
    }
}

/// Keep only the columns a View asked for. An empty column list means
/// the whole row — the source's own shape is the default, so a View
/// author who does not care about columns does not have to name them.
fn project(row: &Value, columns: &[String]) -> Value {
    if columns.is_empty() {
        return row.clone();
    }
    let mut out = serde_json::Map::new();
    for c in columns {
        // A column naming a field the row lacks yields null rather
        // than vanishing: a table whose columns appear and disappear
        // per row is unreadable, and the null is the honest answer.
        out.insert(c.clone(), row.get(c).cloned().unwrap_or(Value::Null));
    }
    Value::Object(out)
}

#[async_trait]
impl ViewResolver for PgViewResolver {
    async fn resolve(&self, view: &View, limit: usize) -> Result<ViewResults, ViewsError> {
        let compiled = filter::compile(&view.filter)?;
        let candidates = self.candidates(view.source).await?;
        let scanned = candidates.len();

        let matching: Vec<Value> = candidates
            .into_iter()
            .filter(|row| match &compiled {
                Some(expr) => filter::matches(expr, row),
                None => true,
            })
            .collect();

        let matched = matching.len();
        let rows = matching
            .iter()
            .take(limit)
            .map(|r| project(r, &view.columns))
            .collect();

        Ok(ViewResults {
            view_id: view.id.clone(),
            source: view.source,
            layout: view.layout,
            rows,
            matched,
            truncated: scanned as i64 >= SCAN_CEILING,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_columns_returns_the_whole_row() {
        let row = json!({"id": "j1", "status": "open"});
        assert_eq!(project(&row, &[]), row);
    }

    #[test]
    fn named_columns_are_kept_in_order_and_others_dropped() {
        let row = json!({"id": "j1", "status": "open", "noise": 1});
        let out = project(&row, &["id".into(), "status".into()]);
        assert_eq!(out, json!({"id": "j1", "status": "open"}));
    }

    #[test]
    fn a_column_the_row_lacks_becomes_null_rather_than_disappearing() {
        // Guards a table whose column set changes row to row.
        let row = json!({"id": "j1"});
        let out = project(&row, &["id".into(), "owner_id".into()]);
        assert_eq!(out, json!({"id": "j1", "owner_id": null}));
    }
}
