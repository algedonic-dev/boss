//! File-references — first-class attachments on Subjects, Jobs,
//! Steps, and Events. See `docs/architecture-decisions.md`
//! §Content, files, knowledge.
//!
//! Two ports (`FileStorage` for bytes, `FileRepository` for rows)
//! sit behind the same HTTP service as bulletins/manual. Bytes live
//! under a configured local-disk `root` (see `LocalDiskStorage`),
//! keyed by sha256; sha256 chains them into `audit_log`.
//!
//! `PgFileRepository` persists the metadata rows; `LocalDiskStorage`
//! is the default bytes backend (no cloud object store / SDK).
//! In-memory adapters back the tests. The `FileStorage` port keeps
//! the bytes backend swappable — a cloud object-store adapter could
//! slot in later behind the same trait.

pub mod error;
pub mod http;
pub mod in_memory;
pub mod port;
pub mod types;

#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;

pub mod local_disk;

pub use error::FileError;
pub use in_memory::{InMemoryFileRepository, InMemoryFileStorage};
pub use local_disk::LocalDiskStorage;
pub use port::{FileRepository, FileStorage};
pub use types::{FileRef, FileRefDraft, ResourceKind, ResourceRef};

#[cfg(feature = "postgres")]
pub use postgres::PgFileRepository;

#[cfg(feature = "postgres")]
pub use rebuild::{
    AuditMismatch, AuditMismatchKind, AuditReport, DEFAULT_GC_GRACE_DAYS, GcReport, RebuildError,
    RebuildReport, audit_sample, gc_orphan_objects, rebuild_file_refs,
};
