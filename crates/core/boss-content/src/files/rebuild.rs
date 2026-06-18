//! Rebuild + GC for the `file_refs` projection.
//!
//! `rebuild_file_refs` reconstructs the table from `audit_log`
//! events `content.file.attached` + `content.file.detached`. Per the
//! design's identity choice (line 96), the file_ref row id IS the
//! event id of the `attached` event — so re-applying the rebuild is
//! idempotent (an INSERT collides on PK; UPDATE on detach is a flip).
//!
//! `gc_orphan_objects` is the bytes-side garbage collector. A file_ref
//! soft-deleted past the 30-day grace window is eligible for byte
//! deletion only when no other live ref shares its sha256. Per
//! design Q3 this is the refcount-at-GC strategy — cheap, eventually
//! consistent, gives operator-error recovery within the window.

use boss_events::replay::{Applied, replay_projection};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::files::error::FileError;
use crate::files::port::FileStorage;
use crate::files::types::FileRef;

/// Distinct from the bulletins lock key (line 20 of rebuild.rs) so the
/// two rebuilds can run concurrently. Same well-known-constant pattern.
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("content-files");

/// Default grace window before bytes are eligible for GC. The design
/// (Q3) calls 30 days; expose the override so tests can use a short
/// horizon without fast-forwarding the clock.
pub const DEFAULT_GC_GRACE_DAYS: i64 = 30;

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub refs_inserted: u64,
    pub refs_soft_deleted: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcReport {
    /// Soft-deleted rows examined in this sweep.
    pub examined: u64,
    /// Rows whose sha256 was still referenced by a live ref — bytes
    /// kept; nothing to do.
    pub kept_dedup: u64,
    /// Rows whose bytes were deleted (no other live ref shared the
    /// sha256). Multiple rows pointing at the same deleted-orphan
    /// sha256 share one delete call.
    pub bytes_deleted: u64,
    /// Storage-side delete failures. Bytes may still exist; the row
    /// stays soft-deleted so the next sweep retries.
    pub delete_failures: u64,
}

#[derive(Debug, Deserialize)]
struct DetachedPayload {
    file_id: Uuid,
    deleted_at: DateTime<Utc>,
}

pub async fn rebuild_file_refs(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &["DELETE FROM file_refs"],
        "kind IN ('content.file.attached', 'content.file.detached')",
        async |conn, ev| {
            match ev.kind.as_str() {
                "content.file.attached" => {
                    let row: FileRef = match serde_json::from_value(ev.payload.clone()) {
                        Ok(r) => r,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, error = %e, "skipping bad FileRef payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    insert_ref(&mut *conn, &row).await.map_err(|e| e.to_string())?;
                    report.refs_inserted += 1;
                    Ok(Applied::Yes)
                }
                "content.file.detached" => {
                    let p: DetachedPayload = match serde_json::from_value(ev.payload.clone()) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, error = %e, "skipping bad detach payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    let n = sqlx::query(
                        "UPDATE file_refs \
                         SET deleted_at = COALESCE(deleted_at, $2) \
                         WHERE id = $1",
                    )
                    .bind(p.file_id)
                    .bind(p.deleted_at)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| e.to_string())?
                    .rows_affected();
                    if n > 0 {
                        report.refs_soft_deleted += 1;
                        Ok(Applied::Yes)
                    } else {
                        // Detach event arrived before its attach — events
                        // arrive in audit_log id order, so this means a
                        // bug or a manual data-load. Skip; the projection
                        // wouldn't have a row to flip anyway.
                        Ok(Applied::Skipped)
                    }
                }
                _ => unreachable!("query filter pinned the kinds"),
            }
        },
    )
    .await
    .map_err(RebuildError::Storage)?;

    report.events_processed = stats.processed;
    report.events_skipped = stats.skipped;
    Ok(report)
}

async fn insert_ref(tx: &mut sqlx::PgConnection, row: &FileRef) -> Result<(), RebuildError> {
    sqlx::query(
        "INSERT INTO file_refs (
            id, target_kind, target_id, bucket, object_key,
            sha256, size_bytes, mime, filename,
            uploaded_by, uploaded_at, deleted_at
         ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(row.id)
    .bind(row.target.kind.as_str())
    .bind(&row.target.id)
    .bind(&row.bucket)
    .bind(&row.object_key)
    .bind(&row.sha256)
    .bind(row.size_bytes)
    .bind(&row.mime)
    .bind(&row.filename)
    .bind(&row.uploaded_by)
    .bind(row.uploaded_at)
    .bind(row.deleted_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}

/// Scan soft-deleted rows older than `grace`, group by `(bucket,
/// object_key)`, and delete bytes for any group whose sha256 has no
/// live references left. Returns a report; never panics on a single
/// storage failure (the row stays soft-deleted, the next sweep
/// retries).
///
/// Operator runs this nightly via a systemd timer (Session 3
/// follow-up) or on-demand via the rebuild binary.
pub async fn gc_orphan_objects(
    pool: &PgPool,
    storage: Arc<dyn FileStorage>,
    now: DateTime<Utc>,
    grace: Duration,
) -> Result<GcReport, FileError> {
    let cutoff = now - grace;

    // Soft-deleted candidates past the grace window. Group by the
    // bytes pointer so two refs sharing the same sha each get one
    // delete attempt rather than N.
    let candidates: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT id, sha256, bucket, object_key FROM file_refs \
         WHERE deleted_at IS NOT NULL AND deleted_at <= $1",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await
    .map_err(|e| FileError::Repository(e.to_string()))?;

    let mut report = GcReport {
        examined: candidates.len() as u64,
        ..Default::default()
    };
    if candidates.is_empty() {
        return Ok(report);
    }

    // Walk in (bucket, object_key) order so we make one delete
    // attempt per object even when multiple soft-deleted refs point
    // at it. Keep the keys we've already settled in a small set.
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

    for (id, sha, bucket, object_key) in candidates {
        let key = (bucket.clone(), object_key.clone());
        if !seen.insert(key) {
            continue;
        }
        let live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM file_refs \
             WHERE sha256 = $1 AND deleted_at IS NULL",
        )
        .bind(&sha)
        .fetch_one(pool)
        .await
        .map_err(|e| FileError::Repository(e.to_string()))?;
        if live > 0 {
            // Another live ref still points at these bytes — keep them.
            report.kept_dedup += 1;
            continue;
        }
        match storage.delete(&object_key).await {
            Ok(()) => {
                report.bytes_deleted += 1;
                info!(
                    file_id = %id, bucket = %bucket, key = %object_key, sha = %sha,
                    "gc: deleted orphan object",
                );
            }
            Err(e) => {
                report.delete_failures += 1;
                warn!(
                    file_id = %id, bucket = %bucket, key = %object_key, error = %e,
                    "gc: storage delete failed; retry next sweep",
                );
            }
        }
    }

    Ok(report)
}

/// Audit a sample of live refs against actual storage: re-fetch each
/// object and recompute its sha256. Returns a list of mismatches +
/// counts so the operator-facing endpoint can render a status badge.
///
/// Sampling because hashing all objects on a large bucket is too
/// expensive for an HTTP endpoint; the operator's question is "is
/// the chain intact?" not "show me every byte."
pub async fn audit_sample(
    pool: &PgPool,
    storage: Arc<dyn FileStorage>,
    sample_size: i64,
) -> Result<AuditReport, FileError> {
    use sha2::{Digest, Sha256};
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT id, sha256, bucket, object_key FROM file_refs \
         WHERE deleted_at IS NULL \
         ORDER BY uploaded_at DESC \
         LIMIT $1",
    )
    .bind(sample_size)
    .fetch_all(pool)
    .await
    .map_err(|e| FileError::Repository(e.to_string()))?;

    let mut report = AuditReport {
        sampled: rows.len() as u64,
        ..Default::default()
    };
    for (id, expected_sha, _bucket, key) in rows {
        match storage.get(&key).await {
            Ok(bytes) => {
                let mut h = Sha256::new();
                h.update(&bytes);
                let got = hex::encode(h.finalize());
                if got == expected_sha {
                    report.matched += 1;
                } else {
                    report.mismatched += 1;
                    report.mismatches.push(AuditMismatch {
                        file_id: id,
                        expected_sha256: expected_sha,
                        actual_sha256: got,
                        kind: AuditMismatchKind::HashMismatch,
                    });
                }
            }
            Err(FileError::NotFound(_)) => {
                report.missing += 1;
                report.mismatches.push(AuditMismatch {
                    file_id: id,
                    expected_sha256: expected_sha,
                    actual_sha256: String::new(),
                    kind: AuditMismatchKind::BytesMissing,
                });
            }
            Err(e) => {
                report.errors += 1;
                warn!(file_id = %id, key = %key, error = %e, "audit: storage error");
            }
        }
    }
    Ok(report)
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct AuditReport {
    pub sampled: u64,
    pub matched: u64,
    pub mismatched: u64,
    pub missing: u64,
    pub errors: u64,
    pub mismatches: Vec<AuditMismatch>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditMismatch {
    pub file_id: Uuid,
    pub expected_sha256: String,
    pub actual_sha256: String,
    pub kind: AuditMismatchKind,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditMismatchKind {
    /// Bytes exist but the hash doesn't match — corruption or
    /// bucket-replacement vector.
    HashMismatch,
    /// `file_refs` row says the bytes should exist; storage GET
    /// returns 404. The Session 3 GC sweep is the legitimate way
    /// for this to happen — but only for soft-deleted rows. A live
    /// ref pointing at missing bytes is a bug.
    BytesMissing,
}
