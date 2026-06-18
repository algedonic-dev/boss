//! Domain types for file references.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What primitive a file is attached to.
///
/// Per design Q6, all four are first-class targets — a file can be
/// attached to a Subject (KB artifact), Job (deliverable), Step (proof
/// of work), or Event (audit-log evidence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    Subject,
    Job,
    Step,
    Event,
}

impl ResourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Job => "job",
            Self::Step => "step",
            Self::Event => "event",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "subject" => Some(Self::Subject),
            "job" => Some(Self::Job),
            "step" => Some(Self::Step),
            "event" => Some(Self::Event),
            _ => None,
        }
    }
}

/// Pointer to the resource a file is attached to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceRef {
    pub kind: ResourceKind,
    pub id: String,
}

/// A persisted file reference. Returned by reads + by inserts after
/// the row + bytes both committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    /// Mirrors the audit_log event id of the `content.file.attached`
    /// event that created this row. Per the design's identity choice
    /// (line 96): rebuild reuses the event id as the row id, giving
    /// idempotent re-application of the rebuild.
    pub id: Uuid,
    pub target: ResourceRef,
    pub bucket: String,
    pub object_key: String,
    pub sha256: String,
    pub size_bytes: i64,
    pub mime: String,
    pub filename: String,
    pub uploaded_by: String,
    pub uploaded_at: DateTime<Utc>,
    /// Soft-delete marker. When `Some`, the row is hidden from
    /// `list_for` but still exists for audit. Bytes get GC'd
    /// separately (see Session 3).
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Pre-insert shape — the caller (Session 2's HTTP handler) fills
/// this after streaming the body, computing sha256, and writing the
/// object. Repository turns it into a `FileRef` row.
///
/// Carries `id` so the HTTP layer can mint a Uuid first, emit the
/// `content.file.attached` event with that id, and then call
/// `insert` — keeping row id ≡ event id (design line 96).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRefDraft {
    pub id: Uuid,
    pub target: ResourceRef,
    pub bucket: String,
    pub object_key: String,
    pub sha256: String,
    pub size_bytes: i64,
    pub mime: String,
    pub filename: String,
    pub uploaded_by: String,
    pub uploaded_at: DateTime<Utc>,
}

impl FileRefDraft {
    /// Convenience for tests + HTTP layer: turn a draft into a row
    /// shape with `deleted_at: None`. Repository implementations are
    /// free to use this or build the FileRef themselves.
    pub fn into_ref(self) -> FileRef {
        FileRef {
            id: self.id,
            target: self.target,
            bucket: self.bucket,
            object_key: self.object_key,
            sha256: self.sha256,
            size_bytes: self.size_bytes,
            mime: self.mime,
            filename: self.filename,
            uploaded_by: self.uploaded_by,
            uploaded_at: self.uploaded_at,
            deleted_at: None,
        }
    }
}
