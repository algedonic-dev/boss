//! In-memory adapters for `FileRepository` + `FileStorage`. Used by
//! the crate's own tests + by downstream-crate integration tests
//! (e.g. boss-jobs Job-page tests that don't want to spin up Postgres
//! and a file-storage root on disk).
//!
//! Production paths use `PgFileRepository` + `LocalDiskStorage`. These
//! adapters preserve the port contract exactly (dedup check, soft-
//! delete idempotence, etc) so swapping for tests doesn't hide bugs
//! that would surface against the real adapters.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::files::error::FileError;
use crate::files::port::{FileRepository, FileStorage};
use crate::files::types::{FileRef, FileRefDraft, ResourceRef};

#[derive(Default)]
pub struct InMemoryFileRepository {
    rows: Mutex<HashMap<Uuid, FileRef>>,
}

impl InMemoryFileRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl FileRepository for InMemoryFileRepository {
    async fn insert(&self, draft: FileRefDraft) -> Result<FileRef, FileError> {
        let mut rows = self.rows.lock().expect("poisoned");
        // Live-row dedup check: a different live row already pointing
        // at this object_key indicates a programming bug — callers
        // should reuse the existing row, not insert a parallel one.
        // Soft-deleted rows are exempt because the bytes GC may have
        // resurrected scenarios.
        for existing in rows.values() {
            if existing.deleted_at.is_none()
                && existing.object_key == draft.object_key
                && existing.bucket == draft.bucket
                && existing.id != draft.id
            {
                return Err(FileError::DuplicateObject(draft.sha256.clone()));
            }
        }
        let row = draft.into_ref();
        rows.insert(row.id, row.clone());
        Ok(row)
    }

    async fn get(&self, id: Uuid) -> Result<Option<FileRef>, FileError> {
        Ok(self.rows.lock().expect("poisoned").get(&id).cloned())
    }

    async fn list_for(&self, target: &ResourceRef) -> Result<Vec<FileRef>, FileError> {
        let rows = self.rows.lock().expect("poisoned");
        let mut out: Vec<FileRef> = rows
            .values()
            .filter(|r| r.deleted_at.is_none() && &r.target == target)
            .cloned()
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.uploaded_at));
        Ok(out)
    }

    async fn list_for_sha256(&self, sha256: &str) -> Result<Vec<FileRef>, FileError> {
        let rows = self.rows.lock().expect("poisoned");
        let mut out: Vec<FileRef> = rows
            .values()
            .filter(|r| r.sha256 == sha256)
            .cloned()
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.uploaded_at));
        Ok(out)
    }

    async fn soft_delete(&self, id: Uuid, at: DateTime<Utc>) -> Result<(), FileError> {
        let mut rows = self.rows.lock().expect("poisoned");
        let Some(row) = rows.get_mut(&id) else {
            return Err(FileError::NotFound(id.to_string()));
        };
        if row.deleted_at.is_none() {
            row.deleted_at = Some(at);
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct InMemoryFileStorage {
    objects: Mutex<HashMap<String, (Bytes, String)>>,
}

impl InMemoryFileStorage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test helper — count of stored objects.
    pub fn len(&self) -> usize {
        self.objects.lock().expect("poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl FileStorage for InMemoryFileStorage {
    async fn put(&self, key: &str, bytes: Bytes, mime: &str) -> Result<(), FileError> {
        self.objects
            .lock()
            .expect("poisoned")
            .insert(key.to_string(), (bytes, mime.to_string()));
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Bytes, FileError> {
        self.objects
            .lock()
            .expect("poisoned")
            .get(key)
            .map(|(b, _)| b.clone())
            .ok_or_else(|| FileError::NotFound(key.to_string()))
    }

    async fn delete(&self, key: &str) -> Result<(), FileError> {
        self.objects.lock().expect("poisoned").remove(key);
        Ok(())
    }

    async fn sign_get_url(&self, key: &str, ttl: Duration) -> Result<String, FileError> {
        // The in-memory adapter doesn't actually serve HTTP, so it
        // hands back a synthetic URL. Tests can assert on the shape
        // without spinning up a server.
        Ok(format!("memory://{key}?ttl={}s", ttl.as_secs()))
    }

    async fn sign_put_url(
        &self,
        key: &str,
        mime: &str,
        ttl: Duration,
    ) -> Result<String, FileError> {
        // Same synthetic shape as sign_get_url; tests verify the URL
        // carries the expected key + mime + ttl without standing up
        // an HTTP server.
        Ok(format!(
            "memory://{key}?op=put&mime={mime}&ttl={}s",
            ttl.as_secs()
        ))
    }

    async fn head(&self, key: &str) -> Result<u64, FileError> {
        self.objects
            .lock()
            .expect("poisoned")
            .get(key)
            .map(|(b, _)| b.len() as u64)
            .ok_or_else(|| FileError::NotFound(key.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::types::ResourceKind;

    fn draft(id: Uuid, target_id: &str, sha: &str) -> FileRefDraft {
        FileRefDraft {
            id,
            target: ResourceRef {
                kind: ResourceKind::Job,
                id: target_id.to_string(),
            },
            bucket: "test".to_string(),
            object_key: format!("sha256/{sha}"),
            sha256: sha.to_string(),
            size_bytes: 1024,
            mime: "image/png".to_string(),
            filename: "x.png".to_string(),
            uploaded_by: "emp-001".to_string(),
            uploaded_at: chrono::Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap(),
        }
    }

    use chrono::TimeZone;

    #[tokio::test]
    async fn insert_then_get_round_trips() {
        let repo = InMemoryFileRepository::new();
        let id = Uuid::new_v4();
        let row = repo.insert(draft(id, "job-001", "abc")).await.unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.target.id, "job-001");
        assert!(row.deleted_at.is_none());

        let fetched = repo.get(id).await.unwrap().expect("present");
        assert_eq!(fetched, row);
    }

    #[tokio::test]
    async fn list_for_returns_only_live_rows_for_target() {
        let repo = InMemoryFileRepository::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let other = Uuid::new_v4();
        repo.insert(draft(a, "job-001", "aaa")).await.unwrap();
        repo.insert(draft(b, "job-001", "bbb")).await.unwrap();
        repo.insert(draft(other, "job-002", "ccc")).await.unwrap();

        let target = ResourceRef {
            kind: ResourceKind::Job,
            id: "job-001".into(),
        };
        let live = repo.list_for(&target).await.unwrap();
        assert_eq!(live.len(), 2);

        // Soft-delete one — list_for hides it.
        repo.soft_delete(a, chrono::Utc::now()).await.unwrap();
        let live2 = repo.list_for(&target).await.unwrap();
        assert_eq!(live2.len(), 1);
        assert_eq!(live2[0].id, b);

        // get() still returns it (audit-page surface).
        let still = repo.get(a).await.unwrap().expect("present");
        assert!(still.deleted_at.is_some());
    }

    #[tokio::test]
    async fn list_for_sha256_includes_soft_deleted_rows() {
        let repo = InMemoryFileRepository::new();
        let live_id = Uuid::new_v4();
        let dead_id = Uuid::new_v4();
        // Two refs sharing the same object — different bucket so the
        // dedup check doesn't trip.
        repo.insert(FileRefDraft {
            bucket: "a".into(),
            ..draft(live_id, "job-001", "xyz")
        })
        .await
        .unwrap();
        repo.insert(FileRefDraft {
            bucket: "b".into(),
            ..draft(dead_id, "job-002", "xyz")
        })
        .await
        .unwrap();
        repo.soft_delete(dead_id, chrono::Utc::now()).await.unwrap();

        let all = repo.list_for_sha256("xyz").await.unwrap();
        assert_eq!(all.len(), 2, "GC sweep must see soft-deleted refs too");
    }

    #[tokio::test]
    async fn duplicate_live_object_key_is_rejected() {
        let repo = InMemoryFileRepository::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        repo.insert(draft(a, "job-001", "shared")).await.unwrap();
        let err = repo
            .insert(draft(b, "job-002", "shared"))
            .await
            .unwrap_err();
        assert!(matches!(err, FileError::DuplicateObject(_)));
    }

    #[tokio::test]
    async fn soft_delete_is_idempotent_and_preserves_first_timestamp() {
        let repo = InMemoryFileRepository::new();
        let id = Uuid::new_v4();
        repo.insert(draft(id, "job-001", "abc")).await.unwrap();
        let t1 = chrono::Utc.with_ymd_and_hms(2026, 5, 3, 10, 0, 0).unwrap();
        let t2 = chrono::Utc.with_ymd_and_hms(2026, 5, 3, 11, 0, 0).unwrap();
        repo.soft_delete(id, t1).await.unwrap();
        repo.soft_delete(id, t2).await.unwrap();
        let row = repo.get(id).await.unwrap().expect("present");
        assert_eq!(row.deleted_at, Some(t1), "second delete is a no-op");
    }

    #[tokio::test]
    async fn soft_delete_unknown_id_is_not_found() {
        let repo = InMemoryFileRepository::new();
        let err = repo
            .soft_delete(Uuid::new_v4(), chrono::Utc::now())
            .await
            .unwrap_err();
        assert!(matches!(err, FileError::NotFound(_)));
    }

    #[tokio::test]
    async fn storage_put_get_delete_round_trip() {
        let storage = InMemoryFileStorage::new();
        let bytes = Bytes::from_static(b"hello-world");
        storage
            .put("sha256/aaa", bytes.clone(), "text/plain")
            .await
            .unwrap();
        assert_eq!(storage.len(), 1);

        let got = storage.get("sha256/aaa").await.unwrap();
        assert_eq!(got, bytes);

        storage.delete("sha256/aaa").await.unwrap();
        assert!(storage.is_empty());
        assert!(matches!(
            storage.get("sha256/aaa").await.unwrap_err(),
            FileError::NotFound(_)
        ));
    }

    #[tokio::test]
    async fn storage_signed_url_carries_key_and_ttl() {
        let storage = InMemoryFileStorage::new();
        let url = storage
            .sign_get_url("sha256/abc", Duration::from_secs(300))
            .await
            .unwrap();
        assert!(url.starts_with("memory://sha256/abc?"));
        assert!(url.contains("ttl=300s"));
    }

    #[tokio::test]
    async fn storage_signed_put_url_carries_key_mime_and_ttl() {
        let storage = InMemoryFileStorage::new();
        let url = storage
            .sign_put_url("sha256/big", "video/mp4", Duration::from_secs(900))
            .await
            .unwrap();
        assert!(url.starts_with("memory://sha256/big?"));
        assert!(url.contains("op=put"));
        assert!(url.contains("mime=video/mp4"));
        assert!(url.contains("ttl=900s"));
    }

    #[tokio::test]
    async fn storage_head_returns_size_for_existing_object() {
        let storage = InMemoryFileStorage::new();
        let bytes = Bytes::from_static(b"twelve-bytes");
        storage
            .put("sha256/h", bytes.clone(), "text/plain")
            .await
            .unwrap();
        let n = storage.head("sha256/h").await.unwrap();
        assert_eq!(n, bytes.len() as u64);
    }

    #[tokio::test]
    async fn storage_head_returns_not_found_for_missing_object() {
        let storage = InMemoryFileStorage::new();
        let err = storage.head("sha256/missing").await.unwrap_err();
        assert!(matches!(err, FileError::NotFound(_)));
    }

    #[tokio::test]
    async fn storage_delete_missing_is_idempotent() {
        let storage = InMemoryFileStorage::new();
        // Just shouldn't error.
        storage.delete("sha256/never-existed").await.unwrap();
    }
}
