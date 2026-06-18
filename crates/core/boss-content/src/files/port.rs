//! Ports — what the file-references layer needs from infrastructure.
//!
//! Two ports, not one: the metadata layer (Postgres rows) and the
//! bytes layer (S3-compatible object storage) are genuinely different
//! adapters. Forcing them through one trait would punish either
//! adapter author. See design doc § "Two ports" for the rationale.

use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use uuid::Uuid;

use crate::files::error::FileError;
use crate::files::types::{FileRef, FileRefDraft, ResourceRef};

/// Metadata persistence — the `file_refs` table behind a trait.
///
/// `insert` is the only write. Updates aren't a concept (file_refs
/// are append-only); replacement is detach-then-attach, which lands
/// as two events and two rows. `soft_delete` flips `deleted_at` on
/// the row but never touches bytes (Session 3 GC handles that).
#[async_trait]
pub trait FileRepository: Send + Sync {
    /// Insert one new row. The draft's `id` is preserved so callers
    /// can mint it ahead of time and use it as both the row PK and
    /// the audit_log event id (per the design's identity choice).
    /// Returns the persisted row with `deleted_at: None`.
    async fn insert(&self, draft: FileRefDraft) -> Result<FileRef, FileError>;

    /// Lookup by id. Returns `Some` even for soft-deleted rows so
    /// the audit page can render historical attachments; callers that
    /// only want live rows filter on `deleted_at.is_none()`.
    async fn get(&self, id: Uuid) -> Result<Option<FileRef>, FileError>;

    /// All live (deleted_at IS NULL) attachments for one resource,
    /// newest first by `uploaded_at`. The Session 2 HTTP layer calls
    /// this from the `<FileAttachments target_*>` Svelte component.
    async fn list_for(&self, target: &ResourceRef) -> Result<Vec<FileRef>, FileError>;

    /// All rows (live + soft-deleted) sharing this sha256. Used by
    /// the Session 3 GC sweep: an object is safe to delete only when
    /// every ref pointing at it is soft-deleted past the grace
    /// window. Returns rows newest-first.
    async fn list_for_sha256(&self, sha256: &str) -> Result<Vec<FileRef>, FileError>;

    /// Mark a row soft-deleted at the given timestamp. Idempotent —
    /// re-deleting an already-deleted row is a no-op (the original
    /// timestamp is preserved for audit). Returns `NotFound` if the
    /// row id doesn't exist.
    async fn soft_delete(
        &self,
        id: Uuid,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), FileError>;
}

/// Object-storage interface — the bytes layer behind a trait.
///
/// Implementations: `S3Storage` (production — speaks the AWS S3 wire,
/// works against GCS/MinIO/R2/B2/AWS), `InMemoryFileStorage` (tests).
/// A `LocalDiskStorage` could land for dev later but isn't part of
/// session 1 — `S3Storage` against MinIO covers the dev case.
#[async_trait]
pub trait FileStorage: Send + Sync {
    /// Write bytes at `key`. Idempotent — re-PUTting the same key
    /// with the same content is a no-op (S3 + GCS both overwrite
    /// silently; that's the contract here). `mime` lands as
    /// `Content-Type` so direct streaming to a browser sets the
    /// right header without a separate metadata fetch.
    async fn put(&self, key: &str, bytes: Bytes, mime: &str) -> Result<(), FileError>;

    /// Read bytes back. v1 reads the whole object into memory — the
    /// design's >8MiB-signed-URL path (Session 4) avoids ever calling
    /// this on large files. A streaming variant can land later if a
    /// genuine 100MiB+ small-file case shows up.
    async fn get(&self, key: &str) -> Result<Bytes, FileError>;

    /// Remove bytes. Idempotent — deleting a missing key is OK.
    /// Called by the Session 3 GC sweep, never by the synchronous
    /// soft-delete path (the design keeps row-soft-delete and
    /// bytes-GC decoupled to support the 30-day grace window).
    async fn delete(&self, key: &str) -> Result<(), FileError>;

    /// Mint a time-bounded URL the client can GET directly. Session 4
    /// uses this for the >8MiB download path (browser fetches from
    /// the bucket, not through the gateway). `ttl` should be small
    /// (minutes, not hours) — the URL leaks scope by construction.
    async fn sign_get_url(&self, key: &str, ttl: Duration) -> Result<String, FileError>;

    /// Mint a time-bounded URL the client can PUT to directly. Session
    /// 4's large-file upload path uses this so >50MiB files don't
    /// stream through the gateway. `mime` is bound into the signature
    /// (the client must send the same Content-Type at PUT time) so a
    /// signed URL can't be used to upload a Trojan disguised as a
    /// declared mime.
    ///
    /// The URL alone is the auth — anyone holding it within `ttl` can
    /// PUT. Keep the TTL short (minutes) and treat leakage as the
    /// equivalent of leaking a one-shot upload token.
    async fn sign_put_url(&self, key: &str, mime: &str, ttl: Duration)
    -> Result<String, FileError>;

    /// Cheap existence + size check. The Session 4 finalize endpoint
    /// uses this after a presigned-URL upload to verify the bytes
    /// actually landed before INSERTing the row. Returns the recorded
    /// size in bytes, or `NotFound` if the object doesn't exist.
    async fn head(&self, key: &str) -> Result<u64, FileError>;
}
