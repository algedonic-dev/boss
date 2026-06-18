//! File-references — first-class attachments on Subjects, Jobs,
//! Steps, and Events. See `docs/architecture-decisions.md`
//! §Content, files, knowledge.
//!
//! Two ports (`FileStorage` for bytes, `FileRepository` for rows)
//! sit behind the same HTTP service as bulletins/manual. Bytes live
//! in object storage; sha256 chains them into `audit_log`.
//!
//! Session 1 ships the ports + schema + Pg adapter + in-memory
//! adapters (test-only). The S3-compatible bytes adapter lands with
//! the HTTP service in Session 2 — that's the first consumer that
//! actually needs to write bytes; pulling `aws-sdk-s3` into the
//! workspace before then would be a build-time tax for no consumer.

pub mod error;
pub mod http;
pub mod in_memory;
pub mod port;
pub mod types;

#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;

#[cfg(feature = "s3")]
pub mod s3;

pub use error::FileError;
pub use in_memory::{InMemoryFileRepository, InMemoryFileStorage};
pub use port::{FileRepository, FileStorage};
pub use types::{FileRef, FileRefDraft, ResourceKind, ResourceRef};

#[cfg(feature = "postgres")]
pub use postgres::PgFileRepository;

#[cfg(feature = "postgres")]
pub use rebuild::{
    AuditMismatch, AuditMismatchKind, AuditReport, DEFAULT_GC_GRACE_DAYS, GcReport, RebuildError,
    RebuildReport, audit_sample, gc_orphan_objects, rebuild_file_refs,
};

#[cfg(feature = "s3")]
pub use s3::S3Storage;
