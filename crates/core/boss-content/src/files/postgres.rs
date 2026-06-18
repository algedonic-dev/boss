//! Postgres adapter for `FileRepository`.
//!
//! Schema lives in `infra/postgres/schema/07-content.sql` (table `file_refs`).
//! This adapter is a straight CRUD mapping; the only non-obvious bit
//! is that `(bucket, object_key)` UNIQUE means two live refs sharing
//! the same `sha256/<hash>` key would conflict at INSERT — which is
//! the dedup contract: the caller should `list_for_sha256` first and
//! reuse the existing object_key when uploading bytes that already
//! exist.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::files::error::FileError;
use crate::files::port::FileRepository;
use crate::files::types::{FileRef, FileRefDraft, ResourceKind, ResourceRef};

pub struct PgFileRepository {
    pool: PgPool,
}

impl PgFileRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn store(e: sqlx::Error) -> FileError {
    // Map the unique-constraint violation on (bucket, object_key) to
    // the structured DuplicateObject variant so callers can branch on
    // it without parsing error strings.
    if let sqlx::Error::Database(db_err) = &e
        && db_err.constraint() == Some("file_refs_bucket_object_key_key")
    {
        return FileError::DuplicateObject(db_err.message().to_string());
    }
    FileError::Repository(e.to_string())
}

fn row_to_file_ref(row: &sqlx::postgres::PgRow) -> Result<FileRef, FileError> {
    let kind_str: String = row.get("target_kind");
    let kind = ResourceKind::parse(&kind_str)
        .ok_or_else(|| FileError::Repository(format!("unknown target_kind in row: {kind_str}")))?;
    Ok(FileRef {
        id: row.get("id"),
        target: ResourceRef {
            kind,
            id: row.get("target_id"),
        },
        bucket: row.get("bucket"),
        object_key: row.get("object_key"),
        sha256: row.get("sha256"),
        size_bytes: row.get("size_bytes"),
        mime: row.get("mime"),
        filename: row.get("filename"),
        uploaded_by: row.get("uploaded_by"),
        uploaded_at: row.get("uploaded_at"),
        deleted_at: row.get("deleted_at"),
    })
}

#[async_trait]
impl FileRepository for PgFileRepository {
    async fn insert(&self, draft: FileRefDraft) -> Result<FileRef, FileError> {
        sqlx::query(
            "INSERT INTO file_refs (
                id, target_kind, target_id, bucket, object_key,
                sha256, size_bytes, mime, filename,
                uploaded_by, uploaded_at, deleted_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,NULL)",
        )
        .bind(draft.id)
        .bind(draft.target.kind.as_str())
        .bind(&draft.target.id)
        .bind(&draft.bucket)
        .bind(&draft.object_key)
        .bind(&draft.sha256)
        .bind(draft.size_bytes)
        .bind(&draft.mime)
        .bind(&draft.filename)
        .bind(&draft.uploaded_by)
        .bind(draft.uploaded_at)
        .execute(&self.pool)
        .await
        .map_err(store)?;
        Ok(draft.into_ref())
    }

    async fn get(&self, id: Uuid) -> Result<Option<FileRef>, FileError> {
        let row = sqlx::query(
            "SELECT id, target_kind, target_id, bucket, object_key,
                    sha256, size_bytes, mime, filename,
                    uploaded_by, uploaded_at, deleted_at
             FROM file_refs WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store)?;
        row.as_ref().map(row_to_file_ref).transpose()
    }

    async fn list_for(&self, target: &ResourceRef) -> Result<Vec<FileRef>, FileError> {
        let rows = sqlx::query(
            "SELECT id, target_kind, target_id, bucket, object_key,
                    sha256, size_bytes, mime, filename,
                    uploaded_by, uploaded_at, deleted_at
             FROM file_refs
             WHERE target_kind = $1 AND target_id = $2
               AND deleted_at IS NULL
             ORDER BY uploaded_at DESC",
        )
        .bind(target.kind.as_str())
        .bind(&target.id)
        .fetch_all(&self.pool)
        .await
        .map_err(store)?;
        rows.iter().map(row_to_file_ref).collect()
    }

    async fn list_for_sha256(&self, sha256: &str) -> Result<Vec<FileRef>, FileError> {
        let rows = sqlx::query(
            "SELECT id, target_kind, target_id, bucket, object_key,
                    sha256, size_bytes, mime, filename,
                    uploaded_by, uploaded_at, deleted_at
             FROM file_refs
             WHERE sha256 = $1
             ORDER BY uploaded_at DESC",
        )
        .bind(sha256)
        .fetch_all(&self.pool)
        .await
        .map_err(store)?;
        rows.iter().map(row_to_file_ref).collect()
    }

    async fn soft_delete(&self, id: Uuid, at: DateTime<Utc>) -> Result<(), FileError> {
        // COALESCE preserves the original deleted_at on a re-delete,
        // matching the in-memory adapter's "first timestamp wins"
        // contract. The RETURNING clause + check_count distinguishes
        // not-found from no-op.
        let res = sqlx::query(
            "UPDATE file_refs
             SET deleted_at = COALESCE(deleted_at, $2)
             WHERE id = $1",
        )
        .bind(id)
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(store)?;
        if res.rows_affected() == 0 {
            return Err(FileError::NotFound(id.to_string()));
        }
        Ok(())
    }
}
