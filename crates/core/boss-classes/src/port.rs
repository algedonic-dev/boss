//! Hexagonal port: `ClassRepository` defines what the domain needs
//! from the Class registry's persistence layer.

use async_trait::async_trait;
use boss_core::primitives::{Class, ClassRef};

#[derive(Debug, thiserror::Error)]
pub enum ClassError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0:?}")]
    NotFound(ClassRef),
    #[error("conflict: {0}")]
    Conflict(String),
    /// A class row named a subject_kind the SubjectKind registry
    /// doesn't carry — "Classes are typed reference data each
    /// Subject kind owns", so the kind must exist first. Enforced by
    /// the classes.subject_kind FK.
    #[error("unregistered subject kind: {0} — register it in the SubjectKind registry first")]
    UnregisteredKind(String),
}

/// Persistence port for the Class registry.
///
/// v1 is read-only: the registry seed lives in the migration, and
/// authoring (CRUD) lands when the admin UI does. The methods here
/// cover what downstream services and the read-side UI need today:
///
/// - `list_for_subject_kind` powers the admin list view and the
///   "what roles exist?" dropdown the create-employee form will eventually
///   replace its hardcoded options with.
/// - `get` returns a single Class by its composite key (including
///   retired rows), so audit / history surfaces can resolve old codes.
/// - `exists_active` is the fast write-time validation primitive that
///   replaces today's CHECK constraints (e.g. `employees.role IN (...)`).
#[async_trait]
pub trait ClassRepository: Send + Sync {
    /// All non-retired Classes for a given `subject_kind`, ordered by
    /// `sort_order` ascending then `code` ascending.
    async fn list_for_subject_kind(&self, subject_kind: &str) -> Result<Vec<Class>, ClassError>;

    /// Fetch a single Class by its composite (`subject_kind`, `code`)
    /// key. Returns `None` if no row matches. **Retired rows are
    /// returned** — callers that want to refuse retired codes should
    /// prefer `exists_active`.
    async fn get(&self, class_ref: &ClassRef) -> Result<Option<Class>, ClassError>;

    /// True iff a non-retired Class with the given `(subject_kind,
    /// code)` exists. The hot-path validation primitive.
    async fn exists_active(&self, class_ref: &ClassRef) -> Result<bool, ClassError>;

    /// Idempotent batch upsert of Class rows. Each row inserts with
    /// `ON CONFLICT (subject_kind, code) DO NOTHING`, mirroring the
    /// seed `classes.sql`'s semantics: re-running is a no-op, existing
    /// rows are left untouched. Returns the number of rows that were
    /// newly inserted (conflicts excluded).
    ///
    /// `classes` is a plain reference table — it is *not* event-sourced
    /// and `boss-rebuild-all` never rebuilds or truncates it — so this
    /// write goes straight to the table without emitting an event.
    async fn batch_upsert(&self, rows: &[Class]) -> Result<u64, ClassError>;

    /// Replace an existing Class's editable body — display name,
    /// parent, member attribute, metadata, sort order. Returns `false`
    /// if no row matches the composite key; the key itself is never
    /// rewritten, because a code is an identity that other rows point
    /// at (`employees.role`, `subject_edges.target_kind`) and renaming
    /// it in place would silently orphan them.
    ///
    /// Distinct from `batch_upsert` on purpose. That one is
    /// insert-if-absent by design — re-running a seed must not clobber
    /// operator edits — which also meant nothing could edit a Class at
    /// all once seeded. A registry the tenant cannot change is not
    /// data, it is a hardcoded list with extra steps, and the whole
    /// point of the Class registry (CLAUDE.md §9) is that taxonomies
    /// are tenant-editable without forking core.
    async fn update(&self, class: &Class) -> Result<bool, ClassError>;

    /// Withdraw a Class from active use by stamping `retired_at`. The
    /// row STAYS — existing rows point at the code (`employees.role`,
    /// step metadata), so retirement removes it from `list` and
    /// `exists_active` without orphaning anything. Idempotent: the
    /// first call stamps the timestamp, a repeat is a no-op that keeps
    /// the original stamp (when it was withdrawn is a fact). Returns
    /// `false` only when no row matches the composite key.
    async fn retire(&self, class_ref: &ClassRef) -> Result<bool, ClassError>;
}
