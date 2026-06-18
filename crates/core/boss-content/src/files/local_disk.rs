//! Local-filesystem storage adapter.
//!
//! Stores object bytes under a configured root directory, keyed by the
//! same `sha256/<hex>` object keys every other adapter uses. This is
//! the default backend for single-VM / local-disk deployments — no
//! cloud object store, no AWS SDK.
//!
//! Large files stream through the content-api rather than redirecting
//! to a presigned URL (there's no object store to offload to), so
//! `sign_get_url` / `sign_put_url` return [`FileError::Unsupported`].
//! The HTTP layer treats that as "fall back to streaming" on download
//! and the SPA uploads everything via multipart `POST /api/files`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use uuid::Uuid;

use crate::files::error::FileError;
use crate::files::port::FileStorage;

/// Filesystem-backed object storage rooted at one directory per
/// deployment (created on startup if absent).
pub struct LocalDiskStorage {
    root: PathBuf,
}

impl LocalDiskStorage {
    /// Bind to `root`, creating it (and parents) if missing.
    pub async fn new(root: impl Into<PathBuf>) -> Result<Self, FileError> {
        let root = root.into();
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|e| FileError::Storage(format!("create root {}: {e}", root.display())))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve an object key to a path under the root, rejecting any
    /// key that would escape it. Keys are app-minted (`sha256/<hex>`),
    /// but this never trusts that — path traversal is a storage-layer
    /// invariant, not the caller's responsibility.
    fn path_for(&self, key: &str) -> Result<PathBuf, FileError> {
        if key.is_empty()
            || key.starts_with('/')
            || key
                .split('/')
                .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        {
            return Err(FileError::Validation(format!("invalid object key: {key}")));
        }
        Ok(self.root.join(key))
    }
}

#[async_trait]
impl FileStorage for LocalDiskStorage {
    async fn put(&self, key: &str, bytes: Bytes, _mime: &str) -> Result<(), FileError> {
        // mime is recorded on the file_refs row (and replayed as the
        // Content-Type on download), so the bytes layer doesn't need to
        // persist it.
        let path = self.path_for(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| FileError::Storage(format!("mkdir {}: {e}", parent.display())))?;
        }
        // Write to a unique temp file then atomically rename, so a
        // concurrent PUT of the same key (same content → same sha key)
        // never exposes a half-written object.
        let tmp = path.with_file_name(format!(
            "{}.{}.tmp",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("obj"),
            Uuid::new_v4()
        ));
        if let Err(e) = tokio::fs::write(&tmp, &bytes).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(FileError::Storage(format!("write {}: {e}", tmp.display())));
        }
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| FileError::Storage(format!("rename into {}: {e}", path.display())))?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Bytes, FileError> {
        let path = self.path_for(key)?;
        match tokio::fs::read(&path).await {
            Ok(v) => Ok(Bytes::from(v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(FileError::NotFound(key.to_string()))
            }
            Err(e) => Err(FileError::Storage(e.to_string())),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), FileError> {
        let path = self.path_for(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            // Idempotent: deleting a missing key is OK (matches the port
            // contract + the S3 adapter it replaces).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(FileError::Storage(e.to_string())),
        }
    }

    async fn sign_get_url(&self, _key: &str, _ttl: Duration) -> Result<String, FileError> {
        Err(FileError::Unsupported(
            "local-disk storage has no presigned GET; downloads stream through the content-api"
                .to_string(),
        ))
    }

    async fn sign_put_url(
        &self,
        _key: &str,
        _mime: &str,
        _ttl: Duration,
    ) -> Result<String, FileError> {
        Err(FileError::Unsupported(
            "local-disk storage has no presigned PUT; uploads use multipart POST /api/files"
                .to_string(),
        ))
    }

    async fn head(&self, key: &str) -> Result<u64, FileError> {
        let path = self.path_for(key)?;
        match tokio::fs::metadata(&path).await {
            Ok(m) => Ok(m.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(FileError::NotFound(key.to_string()))
            }
            Err(e) => Err(FileError::Storage(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn tmp_storage() -> (LocalDiskStorage, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("boss-localdisk-{}", Uuid::new_v4()));
        let s = LocalDiskStorage::new(&dir).await.unwrap();
        (s, dir)
    }

    #[tokio::test]
    async fn put_get_head_delete_round_trip() {
        let (s, dir) = tmp_storage().await;
        let key = "sha256/abc123";
        s.put(key, Bytes::from_static(b"hello"), "text/plain")
            .await
            .unwrap();
        assert_eq!(s.get(key).await.unwrap(), Bytes::from_static(b"hello"));
        assert_eq!(s.head(key).await.unwrap(), 5);
        s.delete(key).await.unwrap();
        assert!(matches!(s.get(key).await, Err(FileError::NotFound(_))));
        // delete is idempotent
        s.delete(key).await.unwrap();
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn missing_object_is_not_found() {
        let (s, dir) = tmp_storage().await;
        assert!(matches!(
            s.get("sha256/nope").await,
            Err(FileError::NotFound(_))
        ));
        assert!(matches!(
            s.head("sha256/nope").await,
            Err(FileError::NotFound(_))
        ));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn rejects_path_traversal_keys() {
        let (s, dir) = tmp_storage().await;
        for bad in ["../escape", "/abs/path", "sha256/../../etc/passwd", ""] {
            assert!(matches!(
                s.put(bad, Bytes::from_static(b"x"), "text/plain").await,
                Err(FileError::Validation(_))
            ));
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn presign_is_unsupported() {
        let (s, dir) = tmp_storage().await;
        assert!(matches!(
            s.sign_get_url("sha256/x", Duration::from_secs(60)).await,
            Err(FileError::Unsupported(_))
        ));
        assert!(matches!(
            s.sign_put_url("sha256/x", "text/plain", Duration::from_secs(60))
                .await,
            Err(FileError::Unsupported(_))
        ));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
