//! Postgres adapter for [`SubjectKindRepository`]. Reads the
//! `subject_kinds` table.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::port::{SubjectKind, SubjectKindError, SubjectKindRepository};

pub struct PgSubjectKinds {
    pool: PgPool,
}

impl PgSubjectKinds {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    kind: String,
    label: String,
    parent_kind: Option<String>,
    description: Option<String>,
    owning_team: String,
    metadata: Value,
    sort_order: i32,
    retired_at: Option<DateTime<Utc>>,
}

impl From<Row> for SubjectKind {
    fn from(r: Row) -> Self {
        Self {
            kind: r.kind,
            label: r.label,
            parent_kind: r.parent_kind,
            description: r.description,
            owning_team: r.owning_team,
            metadata: r.metadata,
            sort_order: r.sort_order,
            retired_at: r.retired_at,
        }
    }
}

const SELECT_COLUMNS: &str = "kind, label, parent_kind, description, owning_team, metadata, \
                              sort_order, retired_at";

#[async_trait]
impl SubjectKindRepository for PgSubjectKinds {
    async fn get(&self, kind: &str) -> Result<Option<SubjectKind>, SubjectKindError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM subject_kinds WHERE kind = $1");
        let row: Option<Row> = sqlx::query_as(&sql)
            .bind(kind)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| SubjectKindError::Storage(e.to_string()))?;
        Ok(row.map(Into::into))
    }

    async fn exists_active(&self, kind: &str) -> Result<bool, SubjectKindError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM subject_kinds \
             WHERE kind = $1 AND retired_at IS NULL)",
        )
        .bind(kind)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| SubjectKindError::Storage(e.to_string()))?;
        Ok(exists)
    }

    async fn list_active(&self) -> Result<Vec<SubjectKind>, SubjectKindError> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM subject_kinds \
             WHERE retired_at IS NULL \
             ORDER BY sort_order ASC, kind ASC"
        );
        let rows: Vec<Row> = sqlx::query_as(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| SubjectKindError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn children_of(&self, parent_kind: &str) -> Result<Vec<SubjectKind>, SubjectKindError> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM subject_kinds \
             WHERE retired_at IS NULL AND parent_kind = $1 \
             ORDER BY kind ASC"
        );
        let rows: Vec<Row> = sqlx::query_as(&sql)
            .bind(parent_kind)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| SubjectKindError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
}
