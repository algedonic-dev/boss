//! Hexagonal port: `SubjectKindRepository` defines what the domain
//! needs from the SubjectKind persistence layer.

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One row of the `subject_kinds` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectKind {
    pub kind: String,
    pub label: String,
    /// Self-referential parent — Account can have parent='account',
    /// Recipe parent='product', etc. Lets tenants declare hierarchies
    /// without forking core. `None` for top-level kinds.
    pub parent_kind: Option<String>,
    pub description: Option<String>,
    /// `system` for core kinds shipped by Boss; tenant id (or
    /// 'brewery' / 'used-device-shop') for tenant-extended kinds.
    pub owning_team: String,
    pub metadata: serde_json::Value,
    pub sort_order: i32,
    /// Set when a kind is retired. Retired kinds are still readable
    /// (`get`) so audit / migration code can resolve old ids, but
    /// `exists_active` returns false.
    pub retired_at: Option<DateTime<Utc>>,
}

/// The fact a metadata edit leaves: one per `patch_metadata` that
/// CHANGED the row's metadata (backlog abc2e9d5, 2026-09-28). Until then
/// the registry had no write door at all, so setting `metadata.module`
/// on eight kinds reached for a migration UPDATE that no event records
/// — the shape `migrations-declare-schema-only.sh` refuses. A patch
/// that restates what the row holds changed nothing and records nothing
/// (the `class.updated` rule).
pub const SUBJECT_KIND_UPDATED: &str = "subject_kind.updated";

/// Metadata keys this door refuses to touch, because code reads them as
/// the kind's SEMANTICS rather than as description (the adversarial
/// review of car abc2e9d5, 2026-09-28). `birth` is boss-jobs'
/// fail-closed subject-existence switch: `{"birth":"job"}` on a domain
/// kind would let a job mint ghost subjects of it, and `{"birth":null}`
/// on `workflow` / `custom` would refuse every new design packet.
/// `calendar_reservable` decides which kinds the calendar lets hold a
/// reservation. Changing either is a decision about what a kind IS, and
/// belongs to the kind-semantics door that does not exist yet
/// (ca7bf46c) — not to a merge any platform-admin can send.
pub const RESERVED_METADATA_KEYS: &[&str] = &["birth", "calendar_reservable"];

/// `module` names the product area a kind belongs to (the subjects page
/// groups by it), so it is a non-empty string or absent.
pub const MODULE_KEY: &str = "module";

/// What one metadata patch changes, by key — the payload of
/// [`SUBJECT_KIND_UPDATED`] minus its key and signer, and the one thing
/// both the write and a replay apply ([`apply_change`]), so the row a
/// write leaves is by construction the row its fact rebuilds.
///
/// `before` and `after` hold a changed key only on the side where the
/// key is PRESENT: a key in `changed` missing from `after` was deleted,
/// one missing from `before` was added. That keeps a deleted key apart
/// from a key the row holds as JSON `null` (a migration may have
/// written one), which a null-for-absent encoding would conflate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataChange {
    pub changed: Vec<String>,
    pub before: Map<String, Value>,
    pub after: Map<String, Value>,
}

impl MetadataChange {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }
}

/// The change a merge patch makes to `held`: each key the patch names
/// replaces the held value, and a key sent as `null` is deleted — the
/// jobs API's metadata-PATCH semantics, so an operator learns the door
/// once. A key whose value the patch restates, or a delete of a key the
/// row does not hold, is not a change. `changed` is in the patch's own
/// key order (serde_json's map is sorted), so the fact is deterministic.
///
/// Refuses, before looking at the row, a patch that names a
/// [`RESERVED_METADATA_KEYS`] key (even to restate it), an empty key,
/// or a `module` that is neither `null` nor a non-empty string — each
/// as [`SubjectKindError::InvalidPatch`]. Both adapters call this, so
/// neither can write what the other refuses.
///
/// Refuses a held value that is not a JSON object: the column defaults
/// to `{}` and nothing writes anything else, so a non-object is a row
/// this door cannot merge into without inventing what it meant.
pub fn metadata_change(
    held: &Value,
    patch: &Map<String, Value>,
) -> Result<MetadataChange, SubjectKindError> {
    for (key, value) in patch {
        if key.trim().is_empty() {
            return Err(SubjectKindError::InvalidPatch(
                "a metadata key must not be empty".into(),
            ));
        }
        if RESERVED_METADATA_KEYS.contains(&key.as_str()) {
            return Err(SubjectKindError::InvalidPatch(format!(
                "`{key}` is reserved: it decides what the kind is, not how it is described, \
                 and this door does not change it (reserved: {})",
                RESERVED_METADATA_KEYS.join(", ")
            )));
        }
        if key == MODULE_KEY
            && !value.is_null()
            && value.as_str().is_none_or(|s| s.trim().is_empty())
        {
            return Err(SubjectKindError::InvalidPatch(format!(
                "`{MODULE_KEY}` is a non-empty string, or null to remove it; got {value}"
            )));
        }
    }
    let held = held.as_object().ok_or_else(|| {
        SubjectKindError::Storage(format!(
            "held metadata is not a JSON object ({held}); refusing to merge into it"
        ))
    })?;
    let mut change = MetadataChange {
        changed: Vec::new(),
        before: Map::new(),
        after: Map::new(),
    };
    for (key, value) in patch {
        let was = held.get(key);
        let will = (!value.is_null()).then_some(value);
        if was == will {
            continue;
        }
        change.changed.push(key.clone());
        if let Some(v) = was {
            change.before.insert(key.clone(), v.clone());
        }
        if let Some(v) = will {
            change.after.insert(key.clone(), v.clone());
        }
    }
    Ok(change)
}

/// Apply a [`MetadataChange`] to `metadata`: each changed key takes its
/// `after` value, or is removed when `after` does not hold it. The
/// write path calls this, and so does anything that replays a
/// [`SUBJECT_KIND_UPDATED`] onto the row the migrations seeded — one
/// function, so the projection cannot disagree with its log. A
/// non-object `metadata` is treated as empty, because `metadata_change`
/// has already refused to produce a change against one.
pub fn apply_change(metadata: &Value, change: &MetadataChange) -> Value {
    Value::Object(apply_to_map(
        metadata.as_object().cloned().unwrap_or_default(),
        change,
    ))
}

fn apply_to_map(mut out: Map<String, Value>, change: &MetadataChange) -> Map<String, Value> {
    for key in &change.changed {
        match change.after.get(key) {
            Some(v) => {
                out.insert(key.clone(), v.clone());
            }
            None => {
                out.remove(key);
            }
        }
    }
    out
}

/// Whether `metadata` holds each changed key exactly as `before` says:
/// present with that value, or absent when `before` does not hold it.
fn holds_before(metadata: &Map<String, Value>, change: &MetadataChange) -> bool {
    change
        .changed
        .iter()
        .all(|k| metadata.get(k) == change.before.get(k))
}

/// Replay one kind's [`SUBJECT_KIND_UPDATED`] facts, oldest first, onto
/// the metadata its row holds — the rebuild's pure half (review of car
/// abc2e9d5, MED-2: closure). Three answers:
///
/// - `Ok(None)`: nothing to write. Either there are no facts, or the row
///   already holds what the whole log arrives at — a live table whose
///   edits went through the door, or a second rebuild. A re-run is a
///   no-op (idempotence).
/// - `Ok(Some(head))`: the row is where the log started (a fresh
///   database: migrations, then the log replayed), and every fact's
///   `before` matched the row as folded so far; `head` is the metadata
///   to write.
/// - `Err(Drift)`: the row is at neither end — something changed it
///   outside the door, or the log is not this table's. Named, with the
///   fact's position and the keys, never overwritten.
pub fn replay_log(
    metadata: &Value,
    log: &[MetadataChange],
) -> Result<Option<Value>, SubjectKindError> {
    let current = metadata.as_object().cloned().unwrap_or_default();
    let head = log.iter().fold(current.clone(), apply_to_map);
    if head == current {
        return Ok(None);
    }
    let mut folded = current;
    for (i, change) in log.iter().enumerate() {
        if !holds_before(&folded, change) {
            let seen: Map<String, Value> = change
                .changed
                .iter()
                .filter_map(|k| folded.get(k).map(|v| (k.clone(), v.clone())))
                .collect();
            return Err(SubjectKindError::Drift(format!(
                "fact {} of {} expected {} before {:?} but the row holds {}",
                i + 1,
                log.len(),
                Value::Object(change.before.clone()),
                change.changed,
                Value::Object(seen)
            )));
        }
        folded = apply_to_map(folded, change);
    }
    Ok(Some(Value::Object(folded)))
}

/// Build the [`SUBJECT_KIND_UPDATED`] event for one patch, or `None`
/// when it changes nothing. The payload is the `kind`, the change's
/// `changed` / `before` / `after`, and `updated_by`, read from the stamp
/// so it is the value `_actor` carries (the `class.updated` shape). One
/// builder for both adapters, so the in-memory double records exactly
/// what the Pg adapter stages on the outbox.
pub fn updated_event(
    stamp: &EventStamp,
    kind: &str,
    change: &MetadataChange,
) -> Result<Option<Event>, SubjectKindError> {
    if change.is_empty() {
        return Ok(None);
    }
    let mut payload =
        serde_json::to_value(change).map_err(|e| SubjectKindError::Storage(e.to_string()))?;
    if let Value::Object(map) = &mut payload {
        map.insert("kind".to_string(), Value::String(kind.to_string()));
        map.insert(
            "updated_by".to_string(),
            Value::String(stamp.actor().to_string()),
        );
    }
    Ok(Some(stamp.event(SUBJECT_KIND_UPDATED, payload)))
}

#[derive(Debug, thiserror::Error)]
pub enum SubjectKindError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    /// The caller's patch is refused as written — a reserved or empty
    /// key, or a malformed `module`. The HTTP door answers 422.
    #[error("invalid patch: {0}")]
    InvalidPatch(String),
    /// The log and the table disagree: a replayed fact's `before` is
    /// not what the row held (the rebuild's drift check).
    #[error("drift: {0}")]
    Drift(String),
}

/// Persistence port for the Subject Kind registry.
///
/// Reads, plus one write — `patch_metadata` — which records its fact on
/// the outbox with the row (backlog abc2e9d5). Rows are still born in
/// the baseline migration and tenant seeds; declaring a NEW kind has no
/// evented door yet (backlog ca7bf46c names that half).
///
/// - `get` returns a single kind (including retired rows).
/// - `exists_active` is the hot-path validator — every write that
///   sets a Subject's kind calls this to reject typos / unregistered
///   kinds.
/// - `list_active` powers the admin / picker UI.
/// - `children_of` walks `parent_kind` hierarchies (e.g. all kinds
///   whose parent is `account`).
#[async_trait]
pub trait SubjectKindRepository: Send + Sync {
    async fn get(&self, kind: &str) -> Result<Option<SubjectKind>, SubjectKindError>;
    async fn exists_active(&self, kind: &str) -> Result<bool, SubjectKindError>;
    async fn list_active(&self) -> Result<Vec<SubjectKind>, SubjectKindError>;
    async fn children_of(&self, parent_kind: &str) -> Result<Vec<SubjectKind>, SubjectKindError>;

    /// Merge `patch` into one kind's `metadata` ([`metadata_change`]:
    /// a key replaces, a `null` deletes) and return the row as it now
    /// stands, or `None` when no row carries `kind`. Retired kinds are
    /// patchable — the row is still the registry's, and `get` still
    /// answers it.
    ///
    /// A patch that changes the metadata records one
    /// [`SUBJECT_KIND_UPDATED`] ([`updated_event`], signed by `stamp`)
    /// in the same transaction as the UPDATE; one that changes nothing
    /// writes nothing and records nothing, so a re-applied patch is a
    /// no-op (idempotence). Only `metadata` moves: the key, label,
    /// parent and ownership are not this door's.
    async fn patch_metadata(
        &self,
        kind: &str,
        patch: &Map<String, Value>,
        stamp: &EventStamp,
    ) -> Result<Option<SubjectKind>, SubjectKindError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patch(v: Value) -> Map<String, Value> {
        v.as_object().cloned().expect("a patch is an object")
    }

    fn stamp() -> EventStamp {
        EventStamp::new(
            "subject-kinds",
            boss_core::actor::ActorId::Automation("tenant-seed".into()),
        )
    }

    /// A key replaces, a null deletes, a restated key and a delete of a
    /// key the row never held are not changes, and a key the patch does
    /// not name is kept.
    #[test]
    fn a_merge_patch_changes_only_what_it_moves() {
        let held = json!({"module": "old", "keep": 1, "drop": true, "same": "x"});
        let change = metadata_change(
            &held,
            &patch(json!({"module": "equipment", "drop": null, "same": "x", "never": null, "new": [1]})),
        )
        .unwrap();
        assert_eq!(change.changed, ["drop", "module", "new"]);
        assert_eq!(
            Value::Object(change.before.clone()),
            json!({"drop": true, "module": "old"})
        );
        assert_eq!(
            Value::Object(change.after.clone()),
            json!({"module": "equipment", "new": [1]})
        );
        assert_eq!(
            apply_change(&held, &change),
            json!({"module": "equipment", "keep": 1, "same": "x", "new": [1]})
        );
    }

    /// Idempotence: the same patch applied to the row it produced
    /// changes nothing, so no second fact.
    #[test]
    fn a_reapplied_patch_is_empty() {
        let held = json!({});
        let p = patch(json!({"module": "finance", "gone": null}));
        let first = metadata_change(&held, &p).unwrap();
        let after = apply_change(&held, &first);
        let second = metadata_change(&after, &p).unwrap();
        assert!(second.is_empty(), "{second:?}");
        assert!(
            updated_event(&stamp(), "invoice", &second)
                .unwrap()
                .is_none()
        );
    }

    /// A key held as JSON null is a value, not an absence: deleting it
    /// is a change, and the fact says the key is gone rather than
    /// "null before, null after".
    #[test]
    fn deleting_a_key_held_as_null_is_a_change_the_fact_can_tell_apart() {
        let held = json!({"legacy": null});
        let change = metadata_change(&held, &patch(json!({"legacy": null}))).unwrap();
        assert_eq!(change.changed, ["legacy"]);
        assert_eq!(
            Value::Object(change.before.clone()),
            json!({"legacy": null})
        );
        assert!(change.after.is_empty());
        assert_eq!(apply_change(&held, &change), json!({}));
    }

    /// Determinism / rebuild: the event's payload alone, replayed onto
    /// the row it started from, reproduces the row the write left.
    #[test]
    fn the_fact_alone_rebuilds_the_row() {
        let held = json!({"a": 1, "b": 2});
        let change =
            metadata_change(&held, &patch(json!({"a": 9, "b": null, "c": "new"}))).unwrap();
        let written = apply_change(&held, &change);
        let event = updated_event(&stamp(), "asset", &change)
            .unwrap()
            .expect("a change leaves a fact");
        assert_eq!(event.kind, SUBJECT_KIND_UPDATED);
        assert_eq!(event.source, "subject-kinds");
        assert_eq!(event.payload["kind"], json!("asset"));
        assert_eq!(event.payload["updated_by"], json!("automation:tenant-seed"));
        assert_eq!(
            event.payload["_actor"], event.payload["updated_by"],
            "updated_by and the stamp's actor are one value"
        );
        let replayed: MetadataChange =
            serde_json::from_value(event.payload.clone()).expect("the payload is a change");
        assert_eq!(apply_change(&held, &replayed), written);
    }

    /// MED-1 of the review: a reserved key is refused whatever the patch
    /// does with it — set, delete, or restate — so the door cannot flip
    /// a kind's birth or reservability.
    #[test]
    fn a_patch_naming_a_reserved_key_is_refused() {
        let held = json!({"birth": "job", "calendar_reservable": true});
        for p in [
            json!({"birth": "job"}),
            json!({"birth": null}),
            json!({"module": "x", "birth": "job"}),
            json!({"calendar_reservable": false}),
        ] {
            let err = metadata_change(&held, &patch(p.clone())).unwrap_err();
            assert!(
                matches!(err, SubjectKindError::InvalidPatch(ref m) if m.contains("reserved")),
                "{p}: {err}"
            );
        }
    }

    /// LOW-2: `module` is a non-empty string or null, and no key is empty.
    #[test]
    fn module_is_a_non_empty_string_and_no_key_is_empty() {
        for p in [
            json!({"module": ""}),
            json!({"module": "  "}),
            json!({"module": 5}),
            json!({"module": ["finance"]}),
            json!({"": 1}),
        ] {
            let err = metadata_change(&json!({}), &patch(p.clone())).unwrap_err();
            assert!(
                matches!(err, SubjectKindError::InvalidPatch(_)),
                "{p}: {err}"
            );
        }
        assert!(metadata_change(&json!({"module": "x"}), &patch(json!({"module": null}))).is_ok());
    }

    /// MED-2 of the review, the pure half: replaying the log onto the
    /// seeded row reproduces the head; a row already at the head is left
    /// alone (a re-run is a no-op); a row that differs from both is drift,
    /// named, never overwritten silently.
    #[test]
    fn replay_reaches_the_head_or_names_the_drift() {
        let seeded = json!({"icon": "box"});
        let c1 = metadata_change(&seeded, &patch(json!({"module": "a"}))).unwrap();
        let mid = apply_change(&seeded, &c1);
        let c2 = metadata_change(&mid, &patch(json!({"module": "b", "icon": null}))).unwrap();
        let head = apply_change(&mid, &c2);
        let log = [c1, c2];

        assert_eq!(replay_log(&seeded, &log).unwrap(), Some(head.clone()));
        assert_eq!(replay_log(&head, &log).unwrap(), None, "already current");
        let err = replay_log(&json!({"icon": "star"}), &log).unwrap_err();
        assert!(matches!(err, SubjectKindError::Drift(_)), "{err}");
        assert_eq!(
            replay_log(&seeded, &[]).unwrap(),
            None,
            "no facts, no write"
        );
    }

    #[test]
    fn a_held_value_that_is_not_an_object_is_refused() {
        let err = metadata_change(&json!([1]), &patch(json!({"module": "x"}))).unwrap_err();
        assert!(err.to_string().contains("not a JSON object"), "{err}");
    }
}
