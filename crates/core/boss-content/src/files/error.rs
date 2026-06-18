//! File-references errors.
//!
//! Kept distinct from `ContentError` because the file flow has
//! genuinely different failure modes (storage I/O, dedup conflicts,
//! sha256 mismatch on re-fetch). Sharing the enum would force
//! every bulletin/manual handler to match on file-only variants.

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("validation failure: {0}")]
    Validation(String),

    /// A file_refs row exists but its sha256/object_key already
    /// belongs to a different live row. Indicates a programming bug
    /// (caller should reuse the existing object_key for dedup, not
    /// re-insert) or a sha256 collision (operationally impossible).
    #[error("duplicate object_key for sha256 {0}")]
    DuplicateObject(String),

    /// Row metadata-layer failure (sqlx, in-memory map, …).
    #[error("repository failure: {0}")]
    Repository(String),

    /// Bytes-layer failure (local-disk I/O, etc.).
    #[error("storage failure: {0}")]
    Storage(String),

    /// Operation the active storage backend doesn't support — e.g.
    /// presigned URLs on the local-disk adapter, which streams bytes
    /// through the content-api instead. Callers degrade gracefully
    /// (download falls back to streaming; upload uses multipart).
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}
