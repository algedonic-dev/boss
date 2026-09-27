//! Hexagonal port: `ClassRepository` defines what the domain needs
//! from the Class registry's persistence layer.

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::primitives::{Class, ClassRef};
use boss_core::publisher::EventStamp;

/// The fact a tenant's declaration leaves: one per Class row the
/// batch INSERTED (backlog d9409039, 2026-09-17). Never per kept row
/// — a row the registry already held changed nothing, so there is
/// nothing to record — and never per batch: the rebuilders reproduce
/// rows, not requests.
pub const CLASS_DECLARED: &str = "class.declared";

/// The fact an edit leaves: one per `update` that CHANGED the row's
/// editable body (backlog 10dabe13, 2026-09-27). Until then the edit
/// door and the retire door each ran a bare UPDATE, so the three rows
/// `boss tenant publish --take classes` edited on 2026-09-26 left no
/// fact and no rebuilder could reproduce the edited labels. A PUT that
/// restates what the row holds changed nothing and records nothing.
pub const CLASS_UPDATED: &str = "class.updated";

/// The fact a withdrawal leaves: one per `retire` that actually set
/// `retired_at` — never for the idempotent repeat, whose stamp did not
/// move (backlog 10dabe13).
pub const CLASS_RETIRED: &str = "class.retired";

/// Build the `class.declared` event for one inserted row: the row as
/// inserted, plus `declared_by` — the actor the request signed with,
/// read from the stamp so it is the same value `_actor` carries. One
/// builder for both adapters, so the in-memory double records exactly
/// what the Pg adapter stages on the outbox.
pub fn declared_event(stamp: &EventStamp, row: &Class) -> Result<Event, ClassError> {
    let mut payload = serde_json::to_value(row).map_err(|e| ClassError::Storage(e.to_string()))?;
    if let serde_json::Value::Object(map) = &mut payload {
        map.insert(
            "declared_by".to_string(),
            serde_json::Value::String(stamp.actor().to_string()),
        );
    }
    Ok(stamp.event(CLASS_DECLARED, payload))
}

/// The declared fields a held Class disagrees with the declaration on,
/// by the input's field names (the chart door's `differs_from`, for
/// the registry's editable body — the key itself cannot differ, it is
/// what matched). `retired_at` is the table's own and not compared: a
/// retired code the file still declares is kept retired, which is the
/// retire door's decision, not the seed's. The batch names a kept row
/// with it, and the edit door decides with it whether an edit changed
/// anything worth a `class.updated` — one comparison for both.
pub fn class_differs(held: &Class, declared: &Class) -> Vec<String> {
    let mut out = Vec::new();
    if held.display_name != declared.display_name {
        out.push("display_name".to_string());
    }
    if held.parent_code != declared.parent_code {
        out.push("parent_code".to_string());
    }
    if held.member_attribute != declared.member_attribute {
        out.push("member_attribute".to_string());
    }
    if held.metadata != declared.metadata {
        out.push("metadata".to_string());
    }
    if held.sort_order != declared.sort_order {
        out.push("sort_order".to_string());
    }
    out
}

/// Build the `class.updated` event for one edit, or `None` when the
/// edit changes nothing (backlog 10dabe13). The payload is the key,
/// `changed` (the field names, in [`class_differs`] order), `before`
/// and `after` holding ONLY those fields — enough for a rebuilder to
/// replay the edit onto the row `class.declared` made — and
/// `updated_by`, read from the stamp so it is the value `_actor`
/// carries. One builder for both adapters, as with [`declared_event`].
pub fn updated_event(
    stamp: &EventStamp,
    before: &Class,
    after: &Class,
) -> Result<Option<Event>, ClassError> {
    let changed = class_differs(before, after);
    if changed.is_empty() {
        return Ok(None);
    }
    let pick = |c: &Class| -> Result<serde_json::Value, ClassError> {
        let full = serde_json::to_value(c).map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(serde_json::Value::Object(
            changed
                .iter()
                .map(|f| (f.clone(), full.get(f).cloned().unwrap_or_default()))
                .collect(),
        ))
    };
    let payload = serde_json::json!({
        "subject_kind": before.subject_kind,
        "code": before.code,
        "changed": changed,
        "before": pick(before)?,
        "after": pick(after)?,
        "updated_by": stamp.actor().to_string(),
    });
    Ok(Some(stamp.event(CLASS_UPDATED, payload)))
}

/// Build the `class.retired` event for the row a retire just stamped:
/// the key, the `retired_at` the row now holds, and `retired_by`, read
/// from the stamp (backlog 10dabe13). Only the call that MOVED the
/// stamp builds one; the adapters decide that, not this function.
pub fn retired_event(stamp: &EventStamp, row: &Class) -> Event {
    stamp.event(
        CLASS_RETIRED,
        serde_json::json!({
            "subject_kind": row.subject_kind,
            "code": row.code,
            "retired_at": row.retired_at,
            "retired_by": stamp.actor().to_string(),
        }),
    )
}

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
/// Reads, plus three writes — `batch_upsert`, `update`, `retire` —
/// each of which records its fact on the outbox with the row. The
/// read methods cover what downstream services and the UI need:
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
    /// Every row inserted records one [`CLASS_DECLARED`] event built
    /// from `stamp` ([`declared_event`]) in the same transaction as
    /// the insert; a kept row records nothing. Until backlog d9409039
    /// (2026-09-17) this write left no audit-log fact at all, on the
    /// reasoning that `classes` is a reference table the rebuilders
    /// never touch — but "every state-changing operation publishes an
    /// event" (CLAUDE.md §Events) has no reference-table exception,
    /// and a tenant's declarations are exactly the state an operator
    /// later asks "when did this appear, and who put it there" about.
    async fn batch_upsert(&self, rows: &[Class], stamp: &EventStamp) -> Result<u64, ClassError>;

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
    ///
    /// An edit that changes the body records one [`CLASS_UPDATED`]
    /// ([`updated_event`], signed by `stamp`) in the same transaction
    /// as the UPDATE; a body identical to the held row writes nothing
    /// and records nothing (backlog 10dabe13 — this was a bare UPDATE
    /// that left no fact until 2026-09-27).
    async fn update(&self, class: &Class, stamp: &EventStamp) -> Result<bool, ClassError>;

    /// Withdraw a Class from active use by stamping `retired_at`. The
    /// row STAYS — existing rows point at the code (`employees.role`,
    /// step metadata), so retirement removes it from `list` and
    /// `exists_active` without orphaning anything. Idempotent: the
    /// first call stamps the timestamp, a repeat is a no-op that keeps
    /// the original stamp (when it was withdrawn is a fact). Returns
    /// `false` only when no row matches the composite key.
    ///
    /// The call that sets the stamp records one [`CLASS_RETIRED`]
    /// ([`retired_event`]) in the same transaction; the repeat moved
    /// nothing and records nothing (backlog 10dabe13).
    async fn retire(&self, class_ref: &ClassRef, stamp: &EventStamp) -> Result<bool, ClassError>;
}
