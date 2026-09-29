//! Postgres adapter for [`SubjectKindRepository`]. Reads the
//! `subject_kinds` table, and writes one kind's metadata with its fact
//! on the outbox in the same transaction.

use async_trait::async_trait;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::PgPool;

use crate::port::{
    SubjectKind, SubjectKindError, SubjectKindRepository, apply_change, metadata_change,
    updated_event,
};

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

    async fn patch_metadata(
        &self,
        kind: &str,
        patch: &Map<String, Value>,
        stamp: &EventStamp,
    ) -> Result<Option<SubjectKind>, SubjectKindError> {
        // One transaction for the read, the UPDATE and its fact
        // (backlog abc2e9d5, the class.updated shape): the row is
        // locked while its metadata is read, so the change the fact
        // names is exactly the change this write made, and the row and
        // the fact commit or roll back together.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| SubjectKindError::Storage(e.to_string()))?;
        let sql = format!("SELECT {SELECT_COLUMNS} FROM subject_kinds WHERE kind = $1 FOR UPDATE");
        let Some(mut held) = sqlx::query_as::<_, Row>(&sql)
            .bind(kind)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| SubjectKindError::Storage(e.to_string()))?
            .map(SubjectKind::from)
        else {
            return Ok(None);
        };
        let change = metadata_change(&held.metadata, patch)?;
        // A patch that moves nothing writes nothing and records nothing
        // (idempotence): the read's transaction just ends.
        let Some(event) = updated_event(stamp, kind, &change)? else {
            return Ok(Some(held));
        };
        // The metadata is computed here by the same function a replay
        // runs, and written whole — never a jsonb `||` in SQL, which
        // would be a second merge rule the fact does not describe.
        held.metadata = apply_change(&held.metadata, &change);
        sqlx::query("UPDATE subject_kinds SET metadata = $2 WHERE kind = $1")
            .bind(kind)
            .bind(&held.metadata)
            .execute(&mut *tx)
            .await
            .map_err(|e| SubjectKindError::Storage(e.to_string()))?;
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(SubjectKindError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| SubjectKindError::Storage(e.to_string()))?;
        Ok(Some(held))
    }
}
