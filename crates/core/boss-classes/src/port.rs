//! Hexagonal port: `ClassRepository` defines what the domain needs
//! from the Class registry's persistence layer.

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::primitives::{Class, ClassRef};
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

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

/// The fields an edit may change — the editable body, in
/// [`class_differs`] order. The key is an identity other rows point at
/// and `retired_at` is the retire door's, so neither is here, and a
/// logged change naming either is refused rather than replayed.
pub const EDITABLE_FIELDS: [&str; 5] = [
    "display_name",
    "parent_code",
    "member_attribute",
    "metadata",
    "sort_order",
];

/// What one edit changes — the payload of [`CLASS_UPDATED`] minus its
/// key and signer, and the one thing both the edit door and a replay
/// apply ([`apply_change`]), so the row a write leaves is by
/// construction the row its fact rebuilds (backlog 3c6d0186; the
/// subject-kinds `MetadataChange` shape). `before` and `after` hold
/// exactly the `changed` fields, each as the row serializes it (an
/// absent optional is JSON `null`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClassChange {
    pub changed: Vec<String>,
    pub before: Map<String, Value>,
    pub after: Map<String, Value>,
}

impl ClassChange {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }
}

fn to_fields(c: &Class) -> Result<Map<String, Value>, ClassError> {
    match serde_json::to_value(c).map_err(|e| ClassError::Storage(e.to_string()))? {
        Value::Object(m) => Ok(m),
        other => Err(ClassError::Storage(format!(
            "a Class serialized to {other}, not an object"
        ))),
    }
}

/// The change an edit asking for `asked` makes to `held`: the
/// [`class_differs`] fields, each with its held and asked value. Empty
/// when the edit restates the row.
pub fn class_change(held: &Class, asked: &Class) -> Result<ClassChange, ClassError> {
    let changed = class_differs(held, asked);
    let (h, a) = (to_fields(held)?, to_fields(asked)?);
    let pick = |m: &Map<String, Value>| -> Map<String, Value> {
        changed
            .iter()
            .map(|f| (f.clone(), m.get(f).cloned().unwrap_or_default()))
            .collect()
    };
    Ok(ClassChange {
        before: pick(&h),
        after: pick(&a),
        changed,
    })
}

/// Apply a [`ClassChange`] to `held`: each changed field takes its
/// `after` value. The edit door writes what this returns, and a replay
/// of [`CLASS_UPDATED`] calls it too — one function, so the projection
/// cannot disagree with its log. Refuses a change naming a field
/// outside [`EDITABLE_FIELDS`], or one whose value does not fit the row.
pub fn apply_change(held: &Class, change: &ClassChange) -> Result<Class, ClassError> {
    let mut fields = to_fields(held)?;
    for f in &change.changed {
        if !EDITABLE_FIELDS.contains(&f.as_str()) {
            return Err(ClassError::Drift(format!(
                "a class change names `{f}`, which is not the editable body ({})",
                EDITABLE_FIELDS.join(", ")
            )));
        }
        fields.insert(f.clone(), change.after.get(f).cloned().unwrap_or_default());
    }
    serde_json::from_value(Value::Object(fields))
        .map_err(|e| ClassError::Drift(format!("a class change does not fit the row: {e}")))
}

/// Build the `class.updated` event for one edit, or `None` when the
/// edit changes nothing (backlog 10dabe13). The payload is the key,
/// the [`ClassChange`] (`changed`, and `before` / `after` holding ONLY
/// those fields — enough for a rebuilder to replay the edit onto the
/// row `class.declared` or a migration made) and `updated_by`, read
/// from the stamp so it is the value `_actor` carries. One builder for
/// both adapters, as with [`declared_event`].
pub fn updated_event(
    stamp: &EventStamp,
    held: &Class,
    change: &ClassChange,
) -> Result<Option<Event>, ClassError> {
    if change.is_empty() {
        return Ok(None);
    }
    let payload = serde_json::json!({
        "subject_kind": held.subject_kind,
        "code": held.code,
        "changed": change.changed,
        "before": change.before,
        "after": change.after,
        "updated_by": stamp.actor().to_string(),
    });
    Ok(Some(stamp.event(CLASS_UPDATED, payload)))
}

/// One Class registry fact as a replay reads it back out of audit_log
/// (backlog 3c6d0186: nothing read these, so a log-rooted rebuild onto a
/// fresh database came back with the migration-seeded Classes and every
/// live declare, edit and retirement gone).
#[derive(Debug, Clone, PartialEq)]
pub enum ClassFact {
    /// [`CLASS_DECLARED`]: the row the batch inserted.
    Declared(Class),
    /// [`CLASS_UPDATED`]: the edit door's change.
    Updated(ClassChange),
    /// [`CLASS_RETIRED`]: the stamp the retire door set.
    Retired(DateTime<Utc>),
}

impl ClassFact {
    /// Parse one logged fact into its key and the fact. Refuses a kind
    /// that is not a Class fact and a payload without its key.
    pub fn from_logged(kind: &str, payload: &Value) -> Result<(ClassRef, ClassFact), ClassError> {
        let bad = |e: String| ClassError::Drift(format!("a logged {kind} does not parse: {e}"));
        let key: ClassRef = serde_json::from_value(serde_json::json!({
            "subject_kind": payload.get("subject_kind"),
            "code": payload.get("code"),
        }))
        .map_err(|e| bad(e.to_string()))?;
        let fact = match kind {
            CLASS_DECLARED => ClassFact::Declared(
                serde_json::from_value(payload.clone()).map_err(|e| bad(e.to_string()))?,
            ),
            CLASS_UPDATED => ClassFact::Updated(
                serde_json::from_value(payload.clone()).map_err(|e| bad(e.to_string()))?,
            ),
            CLASS_RETIRED => ClassFact::Retired(
                serde_json::from_value(payload.get("retired_at").cloned().unwrap_or_default())
                    .map_err(|e| bad(e.to_string()))?,
            ),
            other => return Err(bad(format!("`{other}` is not a Class registry fact"))),
        };
        Ok((key, fact))
    }
}

/// Apply one fact to the row as held (`None`: no row), without asking
/// whether the row was where the fact started — [`replay_class`] asks
/// that. A declare is the row it records: the batch INSERT's, which never
/// binds `retired_at`, or a backfilled birth of a row a migration retired
/// with no fact (backlog 6c2aa86c), which carries it; an edit is
/// [`apply_change`]; a retire sets the stamp. An edit or retire of no row
/// is drift.
pub fn apply_fact(held: Option<&Class>, fact: &ClassFact) -> Result<Class, ClassError> {
    match (fact, held) {
        (ClassFact::Declared(c), _) => Ok(c.clone()),
        (ClassFact::Updated(change), Some(h)) => apply_change(h, change),
        (ClassFact::Retired(at), Some(h)) => Ok(Class {
            retired_at: Some(*at),
            ..h.clone()
        }),
        (_, None) => Err(ClassError::Drift(
            "the log edits or retires a class the registry does not carry".into(),
        )),
    }
}

/// Where the row stood before `fact`, as the write door saw it: a
/// declare found no row (or, from a migration that seeded the same row
/// later, one identical to what it inserted — retired too, when it
/// records a retirement, though maybe at another time: a migration
/// retires with its own NOW() on each database, backlog 6c2aa86c, and the
/// log's time is the one the replay keeps); an edit found each changed
/// field at `before`; a retire found the row active.
fn held_before(held: Option<&Class>, fact: &ClassFact) -> Result<(), String> {
    match (fact, held) {
        (ClassFact::Declared(_), None) => Ok(()),
        (ClassFact::Declared(_), Some(h)) => match apply_fact(None, fact) {
            Ok(inserted) if inserted == *h => Ok(()),
            Ok(inserted)
                if inserted.retired_at.is_some()
                    && h.retired_at.is_some()
                    && Class {
                        retired_at: h.retired_at,
                        ..inserted.clone()
                    } == *h =>
            {
                Ok(())
            }
            _ => Err(format!(
                "a declare expected no row, but the registry holds {}",
                serde_json::to_value(h).unwrap_or_default()
            )),
        },
        (ClassFact::Updated(change), Some(h)) => {
            let fields = to_fields(h).map_err(|e| e.to_string())?;
            let off: Vec<String> = change
                .changed
                .iter()
                .filter(|f| {
                    fields.get(*f).unwrap_or(&Value::Null)
                        != change.before.get(*f).unwrap_or(&Value::Null)
                })
                .map(|f| {
                    format!(
                        "{f}: expected {} but the row holds {}",
                        change.before.get(f).unwrap_or(&Value::Null),
                        fields.get(f).unwrap_or(&Value::Null)
                    )
                })
                .collect();
            if off.is_empty() {
                Ok(())
            } else {
                Err(off.join("; "))
            }
        }
        (ClassFact::Retired(_), Some(h)) => match h.retired_at {
            None => Ok(()),
            Some(at) => Err(format!("a retire expected an active row, retired at {at}")),
        },
        (_, None) => Err("an edit or retire of a class the registry does not carry".into()),
    }
}

/// Replay one Class's facts, oldest first, onto the row the registry
/// holds — the rebuild's pure half (backlog 3c6d0186, closure; the
/// subject-kinds `replay_log` shape). Three answers:
///
/// - `Ok(None)`: nothing to write — no facts, or the row already holds
///   what the whole log arrives at (a live table whose changes all went
///   through the doors, or a second rebuild). A re-run is a no-op.
/// - `Ok(Some(head))`: the row is where the log started (a fresh
///   database: migrations, then the log), and every fact found the row
///   where the write door found it; `head` is the row to write.
/// - `Err(Drift)`: the row is at neither end — changed outside the
///   doors, or the log is not this table's. Named with the fact's
///   position and what differed, never overwritten.
pub fn replay_class(held: Option<&Class>, log: &[ClassFact]) -> Result<Option<Class>, ClassError> {
    if log.is_empty() {
        return Ok(None);
    }
    let head = log.iter().try_fold(held.cloned(), |row, f| {
        apply_fact(row.as_ref(), f).map(Some)
    });
    if let Ok(head) = &head
        && head.as_ref() == held
    {
        return Ok(None);
    }
    let mut folded = held.cloned();
    for (i, fact) in log.iter().enumerate() {
        held_before(folded.as_ref(), fact)
            .map_err(|why| ClassError::Drift(format!("fact {} of {}: {why}", i + 1, log.len())))?;
        folded = Some(apply_fact(folded.as_ref(), fact)?);
    }
    Ok(folded)
}

/// The payload key a BACKFILLED [`CLASS_DECLARED`] carries: its
/// provenance, `"backfilled from the live row on <date>"` (backlog
/// 9d345f9b). A declare the batch door staged has no such key.
///
/// WHY. The batch door staged no fact until 2026-09-17 (d9409039), so
/// a Class declared through it before then, and not seeded by a
/// migration, has no birth in the log: on a fresh database its first
/// logged edit or retirement replays onto no row, which is drift, and
/// the whole rebuild stops. Measured 2026-09-28: ten live Classes, four
/// of them already retired. The backfill records the birth the door
/// never did — but it is recorded NOW, after the facts it precedes, so
/// a replay reads it first among its Class's facts (rebuild.rs). The
/// key is what tells the two apart.
///
/// A `class.updated` carries it too when it records an edit made
/// outside the doors, beside the birth a caller named (backlog
/// 93f361af, [`backfill_edit`]). That one is read in LOG order, not
/// first: it is staged right after its declare, in the same transaction,
/// on a Class with no other fact, so where the log put it is where it
/// happened.
pub const BACKFILLED: &str = "backfilled";

/// The payload key naming where a backfill's body is recorded, when the
/// log itself cannot say (backlog 93f361af): a tenant commit, a file.
pub const BACKFILL_SOURCE: &str = "backfill_source";

/// The payload key a named birth's `class.declared` carries: when the row
/// was inserted, read off its `created_at` under the backfill's lock.
///
/// WHY (review of car 8778f12f, finding 2). A backfilled fact is recorded
/// today, so its own timestamp says when it was WRITTEN; the row's two
/// stamps are the only machine record of when the Class was born and when
/// it was edited outside the doors — and the next door PUT moves
/// `updated_at`, while the facts are immutable, so they are copied into
/// the facts at the one moment both are still true. `null` where the
/// adapter holds no stamps (the in-memory double).
pub const BORN_AT: &str = "born_at";

/// The payload key a named birth's `class.updated` carries: when the edit
/// outside the doors was made, read off the row's `updated_at` ([`BORN_AT`]).
pub const EDITED_AT: &str = "edited_at";

/// The payload key a named birth's `class.updated` carries as `null`: who
/// made the edit outside the doors is not recorded anywhere, and
/// `updated_by` — the actor the backfill request signed with, as on every
/// `class.updated` — recorded the fact; it did not make the edit.
pub const EDITED_BY: &str = "edited_by";

/// What a caller names for a Class changed outside the doors (backlog
/// 93f361af): the body it was `born` with, the fields it says the one
/// edit `changed` — checked against what `born` actually differs from
/// the row in, so a typo in `born` is refused rather than recorded as an
/// edit that never happened (review of car 8778f12f, finding 3) — and
/// the `source` where that body is recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedBirth {
    pub born: Class,
    pub changed: Vec<String>,
    pub source: String,
}

/// True when a logged Class fact is a backfill ([`BACKFILLED`]).
pub fn is_backfill(payload: &Value) -> bool {
    payload.get(BACKFILLED).is_some_and(|v| !v.is_null())
}

/// What a backfill of one Class's missing declare did.
#[derive(Debug, Clone, PartialEq)]
pub enum Backfill {
    /// A `class.declared` was staged for the row as it was born.
    Recorded(Class),
    /// A named birth ([`backfill_edit`]): a `class.declared` of `born`
    /// and a `class.updated` of `change`, the edit made outside the
    /// doors that took it to the row held, both staged.
    RecordedWithEdit { born: Class, change: ClassChange },
    /// An observed birth ([`observed_birth`]): a `class.declared` of the
    /// row as it stands with its logged facts un-applied, marked observed.
    Observed(Class),
    /// The log already declares the Class (through the door, or an
    /// earlier backfill) and replays from nothing to the row held:
    /// nothing staged.
    AlreadyDeclared,
    /// The registry holds no such row.
    NotFound,
}

/// A Class's logged facts, given in log order as `(kind, payload)`, in
/// the order a replay reads them: a backfilled declare ([`is_backfill`])
/// first — it records a birth that preceded the facts it was appended
/// after — and every other fact where the log put it (a stable sort).
/// Refuses a fact that does not parse.
pub fn facts_in_replay_order<'a>(
    logged: impl IntoIterator<Item = (&'a str, &'a Value)>,
) -> Result<Vec<ClassFact>, ClassError> {
    let mut facts = logged
        .into_iter()
        .map(|(kind, payload)| {
            let backfilled = kind == CLASS_DECLARED && is_backfill(payload);
            ClassFact::from_logged(kind, payload).map(|(_, f)| (backfilled, f))
        })
        .collect::<Result<Vec<_>, _>>()?;
    facts.sort_by_key(|(backfilled, _)| !*backfilled);
    Ok(facts.into_iter().map(|(_, f)| f).collect())
}

/// The row as it stood before the first fact the log holds for it —
/// what a backfilled declare records (backlog 9d345f9b). `log` is the
/// Class's facts in replay order ([`facts_in_replay_order`]).
///
/// `None` when the log already declares the Class — and only once that
/// log is shown to replay from nothing to `live`: a declare, a delete
/// outside the doors and a re-declare of another body is drift, not
/// "nothing to do" (review of car ce5ce2de, LOW-3).
///
/// Otherwise each logged fact is un-applied from `live`, newest first
/// (an edit's changed fields go back to `before`, a retirement's stamp
/// comes off), and the answer is proved before it is returned: replayed
/// from nothing, the backfill and the log must arrive at `live`, and
/// replayed onto `live` they must write nothing. That proof covers the
/// fields and stamps the log's facts TOUCH. A field no logged fact
/// touches is taken from `live` as it stands: a change made to it
/// outside the doors — a bare UPDATE before 10dabe13 gave edits a fact
/// — cannot be seen from the log and is absorbed into the birth. So the
/// caller must decide from outside the log whether the row was changed
/// where the log could not see it; the Postgres adapter asks the row's
/// stamps ([`changed_outside_the_doors`]) and refuses when they say so
/// (review of car ce5ce2de, HIGH-1, for an empty log; backlog 6c2aa86c
/// for a log with a fact, which the first check skipped).
pub fn backfill_birth(live: &Class, log: &[ClassFact]) -> Result<Option<Class>, ClassError> {
    if log.iter().any(|f| matches!(f, ClassFact::Declared(_))) {
        return match replay_class(None, log) {
            Ok(Some(head)) if head == *live => Ok(None),
            other => Err(ClassError::Drift(format!(
                "({}, {}): the log declares this class but does not replay to the row \
                 it holds — from nothing it replays to {other:?}",
                live.subject_kind, live.code
            ))),
        };
    }
    let born = log
        .iter()
        .rev()
        .try_fold(live.clone(), |row, fact| match fact {
            ClassFact::Updated(change) => {
                let mut fields = to_fields(&row)?;
                for f in &change.changed {
                    fields.insert(f.clone(), change.before.get(f).cloned().unwrap_or_default());
                }
                serde_json::from_value(Value::Object(fields))
                    .map_err(|e| ClassError::Drift(format!("a logged edit does not un-apply: {e}")))
            }
            ClassFact::Retired(_) => Ok(Class {
                retired_at: None,
                ..row
            }),
            ClassFact::Declared(_) => Ok(row),
        })?;
    // A retirement no fact records — a migration retired the row (the
    // four refurb asset phases, 2026-09-24; message/archived, 2026-09-26)
    // — stays on the birth: it is where the row stood before any logged
    // fact, and a birth without it would replay to an active row (backlog
    // 6c2aa86c review). A logged retirement came off in the fold above.
    let replay: Vec<ClassFact> = std::iter::once(ClassFact::Declared(born.clone()))
        .chain(log.iter().cloned())
        .collect();
    let from_nothing = replay_class(None, &replay);
    let onto_live = replay_class(Some(live), &replay);
    match (&from_nothing, &onto_live) {
        (Ok(Some(head)), Ok(None)) if head == live => Ok(Some(born)),
        _ => Err(ClassError::Drift(format!(
            "({}, {}): the log does not explain the row it holds — from nothing the \
             backfill replays to {from_nothing:?}, onto the row to {onto_live:?}",
            live.subject_kind, live.code
        ))),
    }
}

/// Whether a row's two stamps show a write the log did not hear of, for
/// a Class the log does not declare — `Some(why)` when they do (backlog
/// 6c2aa86c). `log` is the Class's facts in log order.
///
/// An insert — the batch door, a migration seed — writes `created_at`
/// and `updated_at` with one NOW(); the retire door writes `retired_at`
/// and `updated_at` with one NOW(), and its fact carries that
/// `retired_at`; a rebuild leaves `updated_at` alone. So the stamps
/// clear the row when `updated_at` is still its insert's, or is the
/// retirement its newest fact records. Anything else — `updated_at`
/// moved with no fact, after the newest fact, or behind an edit fact,
/// which records no time it wrote the row (the edit door's `updated_at`
/// is the database's clock, the fact's timestamp the service's) — is a
/// write the log cannot account for.
///
/// WHY. Until this, the only check was on an EMPTY log (HIGH-1), so the
/// first door fact on an undeclared Class switched it off for good: a
/// row changed outside the doors and then retired through them was
/// backfilled with the outside body as its birth.
///
/// What the stamps CANNOT show: a change made before a later write that
/// moved `updated_at` again — before the first fact, between two facts,
/// or by a bare UPDATE that left `updated_at` alone. That change is
/// absorbed into the birth, beside the untouched-field caveat on
/// [`backfill_birth`]; only a record outside the database (a tenant
/// commit, [`NamedBirth`]) can recover it.
pub fn changed_outside_the_doors(
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    log: &[ClassFact],
) -> Option<String> {
    if updated_at == created_at {
        return None;
    }
    match log.last() {
        None => Some(format!(
            "the row was written at {updated_at}, after its insert at {created_at}, and the \
             log holds no fact for that change"
        )),
        Some(ClassFact::Retired(at)) if *at == updated_at => None,
        Some(ClassFact::Retired(at)) => Some(format!(
            "the row was written at {updated_at}, after its insert at {created_at} and after \
             its newest fact, the retirement at {at}"
        )),
        Some(_) => Some(format!(
            "the row was written at {updated_at}, after its insert at {created_at}, and its \
             newest fact records no time it wrote the row, so the stamp cannot be accounted for"
        )),
    }
}

/// The birth a door must record before its own fact, for a Class the log
/// does not declare (backlog 6c2aa86c): `None` when the log declares it;
/// otherwise [`backfill_birth`] — the row before every logged fact —
/// unless `outside` ([`changed_outside_the_doors`]) says the row was
/// written where the log could not see, which is refused, naming the
/// named-birth door.
///
/// WHY. A door fact on an undeclared Class is what switched the backfill's
/// stamp check off (it ran only on an empty log), so every door write
/// now leaves a declared Class behind. The operator's call (2026-09-28):
/// declare it in the door's own transaction when the stamps stand behind
/// the birth, rather than refuse the edit — 80 live Classes carry no
/// declare, and refusing would stop ordinary edits on every one of them
/// until someone backfilled it. Refused only where the birth would be
/// false; the bodiless backfill asks the same question the same way.
pub fn undeclared_birth(
    live: &Class,
    log: &[ClassFact],
    outside: Option<String>,
) -> Result<Option<Class>, ClassError> {
    if log.iter().any(|f| matches!(f, ClassFact::Declared(_))) {
        return Ok(None);
    }
    if let Some(why) = outside {
        return Err(ClassError::Drift(format!(
            "({}, {}): the log does not declare this class, and {why} — its live body is \
             not its proven birth, so no fact is recorded for it. {}.",
            live.subject_kind,
            live.code,
            ways_to_declare(live, log)
        )));
    }
    backfill_birth(live, log)
}

/// The doors that WILL take a Class whose stamps refused its birth
/// (review of this car, 2026-09-28: the 409 named a door that refused
/// the same Class in turn). The observed birth always does; the named
/// birth only when [`backfill_edit`] can — a log with no fact and a row
/// no retirement it cannot record has touched.
fn ways_to_declare(live: &Class, log: &[ClassFact]) -> String {
    let door = format!(
        "POST /api/classes/{}/{}/backfill-declared",
        live.subject_kind, live.code
    );
    let observed = format!(
        "Record it as observed: {door} with {{\"observed\": true, \"source\": \"<why>\"}} \
         declares the row as it stands with its logged facts un-applied, marked observed \
         and carrying this drift"
    );
    if log.is_empty() && live.retired_at.is_none() {
        format!(
            "{observed}; or, if the body it was born with is recorded somewhere, the same \
             door with {{born, changed, source}} records that birth and the outside edit"
        )
    } else {
        observed
    }
}

/// The payload key an OBSERVED birth's `class.declared` carries as
/// `"observed"` (review of this car, backlog 6c2aa86c): the birth is the
/// row as it stands with its logged facts un-applied, recorded although
/// its stamps could not prove nothing outside the doors touched it —
/// the operator's act, never a door's by itself.
pub const BIRTH: &str = "birth";

/// The value of [`BIRTH`] on an observed birth.
pub const OBSERVED: &str = "observed";

/// The payload key an observed birth carries: the row's `updated_at` when
/// it was observed ([`BORN_AT`] carries its `created_at`).
pub const ROW_UPDATED_AT: &str = "row_updated_at";

/// The payload key an observed birth carries: why its stamps could not
/// prove the birth ([`changed_outside_the_doors`]), or `null` when they
/// could or the adapter holds none.
pub const DRIFT: &str = "drift";

/// The birth an OBSERVED backfill records (review of this car, backlog
/// 6c2aa86c) — `None` when the log already declares the Class and replays
/// to `live`. The body is [`backfill_birth`]'s: the live row with every
/// logged fact un-applied, keeping a retirement no fact records, and
/// proved to replay from nothing to `live`. What it skips is the stamp
/// question, because the answer is already known: something outside the
/// doors wrote the row, and nothing in the database can say what it was.
///
/// WHY. Five live Classes had no door that would take them once the
/// doors declared first: asset/triaging, refurbing, qa and ready
/// (retired by a migration on 2026-09-24) and message/archived (on
/// 2026-09-26) — no fact, stamps moved — so the edit door refused on the
/// stamps, the bodiless backfill refused on the stamps, and the named
/// birth refused a retired row with no retirement fact. So did a Class
/// retired through the door and changed outside it after, and one with a
/// pre-10dabe13 edit fact. Their birth cannot be proved; it can be
/// OBSERVED, and the fact says so ([`BIRTH`], [`DRIFT`]) rather than
/// passing for a proved one.
pub fn observed_birth(live: &Class, log: &[ClassFact]) -> Result<Option<Class>, ClassError> {
    backfill_birth(live, log)
}

/// Build an observed birth's `class.declared`: [`declared_event`]'s
/// payload plus [`BACKFILLED`] (`"observed from the live row on <date>"`),
/// [`BIRTH`] `"observed"`, [`BACKFILL_SOURCE`] (the operator's reason),
/// [`BORN_AT`] and [`ROW_UPDATED_AT`] (the row's stamps, `null` where the
/// adapter holds none) and [`DRIFT`]. Refuses a blank source: an observed
/// birth is only as good as the reason given for it.
pub fn observed_declared_event(
    stamp: &EventStamp,
    born: &Class,
    source: &str,
    born_at: Option<DateTime<Utc>>,
    row_updated_at: Option<DateTime<Utc>>,
    drift: Option<String>,
) -> Result<Event, ClassError> {
    let source = source.trim();
    if source.is_empty() {
        return Err(ClassError::Drift(format!(
            "({}, {}): an observed birth names its source — why the row is recorded as it \
             stands",
            born.subject_kind, born.code
        )));
    }
    let mut event = declared_event(stamp, born)?;
    let on = event.timestamp.date_naive();
    if let Value::Object(map) = &mut event.payload {
        map.insert(
            BACKFILLED.to_string(),
            Value::String(format!("observed from the live row on {on}")),
        );
        map.insert(BIRTH.to_string(), Value::String(OBSERVED.to_string()));
        map.insert(
            BACKFILL_SOURCE.to_string(),
            Value::String(source.to_string()),
        );
        map.insert(BORN_AT.to_string(), serde_json::json!(born_at));
        map.insert(
            ROW_UPDATED_AT.to_string(),
            serde_json::json!(row_updated_at),
        );
        map.insert(DRIFT.to_string(), serde_json::json!(drift));
    }
    Ok(event)
}

/// What the edit and retire doors would do to one Class, read without
/// writing (review of this car: drift must be measurable before anyone
/// edits) — `GET /api/classes/{kind}/{code}/birth` and the list form.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BirthPlan {
    pub subject_kind: String,
    pub code: String,
    pub retired: bool,
    /// The row's stamps; `null` where the adapter holds none.
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    /// How many Class facts the log holds for it.
    pub facts: usize,
    pub declared: bool,
    /// `nothing` (the log declares it), `declare` (the door records its
    /// birth first), or `refuse` (its stamps show a write the log did not
    /// hear of, or its log does not explain it).
    pub door: &'static str,
    /// Why, for `nothing` and `refuse` — the refusal's own words.
    pub why: Option<String>,
}

/// The [`BirthPlan`] for one row, its log (replay order) and its stamps
/// — the same decision the doors make ([`undeclared_birth`]), run dry.
pub fn birth_plan(
    live: &Class,
    log: &[ClassFact],
    stamps: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> BirthPlan {
    let declared = log.iter().any(|f| matches!(f, ClassFact::Declared(_)));
    let outside =
        stamps.and_then(|(created, updated)| changed_outside_the_doors(created, updated, log));
    let (door, why) = if declared {
        ("nothing", Some("the log declares this class".to_string()))
    } else {
        match undeclared_birth(live, log, outside) {
            Ok(_) => ("declare", None),
            Err(e) => ("refuse", Some(e.to_string())),
        }
    };
    BirthPlan {
        subject_kind: live.subject_kind.clone(),
        code: live.code.clone(),
        retired: live.retired_at.is_some(),
        created_at: stamps.map(|(c, _)| c),
        updated_at: stamps.map(|(_, u)| u),
        facts: log.len(),
        declared,
        door,
        why,
    }
}

/// Build the backfilled `class.declared` for a row born before the door
/// staged one: [`declared_event`]'s payload — the row and `declared_by`,
/// read from the stamp — plus [`BACKFILLED`], naming the day it was
/// recorded (the event's own timestamp, so the two cannot disagree), so
/// the log says what it is rather than passing for the door's fact.
pub fn backfilled_declared_event(stamp: &EventStamp, born: &Class) -> Result<Event, ClassError> {
    let mut event = declared_event(stamp, born)?;
    let on = event.timestamp.date_naive();
    if let Value::Object(map) = &mut event.payload {
        map.insert(
            BACKFILLED.to_string(),
            Value::String(format!("backfilled from the live row on {on}")),
        );
    }
    Ok(event)
}

/// The edit a NAMED birth says was made outside the doors, for a Class
/// whose log holds no fact at all (backlog 93f361af) — or `None` when
/// the log already declares the Class and replays to `live` (a repeat).
///
/// WHY. A row the batch door inserted before it staged facts, and that
/// a bare UPDATE then changed before 10dabe13 gave edits a fact, has
/// nothing in the log to un-apply: [`backfill_birth`] would record its
/// EDITED body as its birth, a false fact every rebuild replays, so the
/// Postgres adapter refuses it (review of car ce5ce2de, HIGH-1). Measured
/// 2026-09-28: the LLC's three invoice Classes, inserted 2026-09-16 with
/// no metadata and given `gl_account` by `boss tenant publish --take
/// classes` on 2026-09-26. Their pre-edit body is not lost — it is in
/// the tenant repo's history — so the caller names it, and the backfill
/// records what happened: born as `born`, then edited to `live`.
///
/// The door stands behind it only when that one edit explains the row:
/// the log is empty (a log with an edit or a retirement is the one the
/// bodiless backfill un-applies, and an edit made through the door
/// first is refused here rather than laundered into a birth); `born`
/// carries the row's key and differs from it only in the editable body
/// the edit records — never in a retirement no fact records; it does
/// differ, in exactly the fields the caller names in `changed` (review
/// of car 8778f12f, finding 3: a typo in `born` is a difference nobody
/// meant, and once recorded a repeat answers already declared, so it
/// could never be corrected); and replayed from nothing the two facts
/// AND the Class's whole log arrive at `live`, while replayed onto
/// `live` they write nothing (the proof [`backfill_birth`] runs). The
/// proof takes the log although the guard above admits only an empty
/// one (finding 1: it replayed the two new facts alone, so nothing but
/// the guard stood between a named birth and a log it did not explain).
/// What it cannot check is that `born` is TRUE — that is what the
/// caller's named source is for.
pub fn backfill_edit(
    live: &Class,
    log: &[ClassFact],
    named: &NamedBirth,
) -> Result<Option<ClassChange>, ClassError> {
    let key = format!("({}, {})", live.subject_kind, live.code);
    let refuse = |why: String| Err(ClassError::Drift(format!("{key}: {why}")));
    if log.iter().any(|f| matches!(f, ClassFact::Declared(_))) {
        return backfill_birth(live, log).and_then(|b| match b {
            None => Ok(None),
            Some(_) => refuse("the log declares this class and still asks for a birth".into()),
        });
    }
    if !log.is_empty() {
        return refuse(format!(
            "the log holds {} fact(s) for this class, so its birth is the row before \
             them — the backfill without a body un-applies them, or, when its stamps \
             refuse that, the body {{\"observed\": true, \"source\": \"<why>\"}} records \
             it as observed; a named birth is for a class with no fact at all",
            log.len()
        ));
    }
    let born = &named.born;
    if (born.subject_kind.as_str(), born.code.as_str())
        != (live.subject_kind.as_str(), live.code.as_str())
    {
        return refuse(format!(
            "the named birth is ({}, {}), not this class",
            born.subject_kind, born.code
        ));
    }
    if let Some(at) = live.retired_at {
        return refuse(format!(
            "the row was retired at {at} and the log holds no retirement; an edit \
             does not carry retired_at — the body {{\"observed\": true, \"source\": \
             \"<why>\"}} records the row as observed, retirement and all"
        ));
    }
    let born = Class {
        retired_at: None,
        ..born.clone()
    };
    let change = class_change(&born, live)?;
    if change.is_empty() {
        return refuse(
            "the named birth is the row as it stands, so there is no edit to record — \
             the backfill without a body records that birth"
                .into(),
        );
    }
    let sorted = |fields: &[String]| {
        let mut v = fields.to_vec();
        v.sort();
        v
    };
    if sorted(&named.changed) != sorted(&change.changed) {
        return refuse(format!(
            "the caller says the edit changed {:?}, but the named birth differs from the \
             row in {:?} — a difference nobody named would be recorded as an edit",
            named.changed, change.changed
        ));
    }
    let replay: Vec<ClassFact> = [
        ClassFact::Declared(born.clone()),
        ClassFact::Updated(change.clone()),
    ]
    .into_iter()
    .chain(log.iter().cloned())
    .collect();
    let from_nothing = replay_class(None, &replay);
    let onto_live = replay_class(Some(live), &replay);
    match (&from_nothing, &onto_live) {
        (Ok(Some(head)), Ok(None)) if head == live => Ok(Some(change)),
        _ => refuse(format!(
            "the named birth and its edit do not replay to the row — from nothing to \
             {from_nothing:?}, onto the row to {onto_live:?}"
        )),
    }
}

/// Build the two facts a named birth records ([`backfill_edit`]): the
/// `class.declared` of `born` and the `class.updated` of `change`, each
/// the door's own payload ([`declared_event`], [`updated_event`]) plus
/// [`BACKFILLED`] (`"backfilled from <source> on <date>"`) and
/// [`BACKFILL_SOURCE`] — so neither passes for a door's fact, and each
/// says where the body it asserts is recorded. The declare carries
/// [`BORN_AT`] (`born_at`, the row's `created_at`) and the edit
/// [`EDITED_AT`] (`edited_at`, its `updated_at`) and [`EDITED_BY`]
/// (`null`: unknown), because each fact's own timestamp is the day it was
/// recorded, not the day it happened (review of car 8778f12f, finding 2).
/// Staged in this order, in one transaction, on a Class with no other
/// fact, so the log's order is the order they happened in. Refuses a
/// blank source: a birth the log cannot show is only as good as the
/// record it names.
pub fn backfilled_edit_events(
    stamp: &EventStamp,
    born: &Class,
    change: &ClassChange,
    source: &str,
    born_at: Option<DateTime<Utc>>,
    edited_at: Option<DateTime<Utc>>,
) -> Result<[Event; 2], ClassError> {
    let source = source.trim();
    if source.is_empty() {
        return Err(ClassError::Drift(format!(
            "({}, {}): a named birth names its source — where the body it asserts is recorded",
            born.subject_kind, born.code
        )));
    }
    let mut declared = declared_event(stamp, born)?;
    let mut updated = updated_event(stamp, born, change)?.ok_or_else(|| {
        ClassError::Drift(format!(
            "({}, {}): a named birth records an edit, and this one changes nothing",
            born.subject_kind, born.code
        ))
    })?;
    if let Value::Object(map) = &mut declared.payload {
        map.insert(BORN_AT.to_string(), serde_json::json!(born_at));
    }
    if let Value::Object(map) = &mut updated.payload {
        map.insert(EDITED_AT.to_string(), serde_json::json!(edited_at));
        map.insert(EDITED_BY.to_string(), Value::Null);
    }
    Ok([declared, updated].map(|mut event| {
        let on = event.timestamp.date_naive();
        if let Value::Object(map) = &mut event.payload {
            map.insert(
                BACKFILLED.to_string(),
                Value::String(format!("backfilled from {source} on {on}")),
            );
            map.insert(
                BACKFILL_SOURCE.to_string(),
                Value::String(source.to_string()),
            );
        }
        event
    }))
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
    /// A row the log does not explain, or a logged fact that cannot be
    /// replayed (backlog 3c6d0186): the rebuild names it and writes
    /// nothing rather than overwrite what it cannot account for.
    #[error("drift: {0}")]
    Drift(String),
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
    ///
    /// On a Class the log does not declare, an edit that changes the body
    /// first records its birth ([`undeclared_birth`], built by
    /// [`backfilled_declared_event`]) in the same transaction, so no door
    /// fact lands on an undeclared Class (backlog 6c2aa86c); when the
    /// row's stamps show a change outside the doors it is refused as
    /// [`ClassError::Drift`] and writes nothing.
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
    /// nothing and records nothing (backlog 10dabe13). On a Class the log
    /// does not declare, the birth first, or the refusal, as [`Self::update`].
    async fn retire(&self, class_ref: &ClassRef, stamp: &EventStamp) -> Result<bool, ClassError>;

    /// Record the `class.declared` a row was born without (backlog
    /// 9d345f9b): the row as [`backfill_birth`] reconstructs it from the
    /// row held and the Class's logged facts, built by
    /// [`backfilled_declared_event`] and staged on the outbox. Writes no
    /// row — the registry already holds it; what was missing is the
    /// log's account of where it came from. A Class the log already
    /// declares, and whose log replays to the row, records nothing
    /// ([`Backfill::AlreadyDeclared`]), so a repeat is a no-op; a row its
    /// log does not explain is [`ClassError::Drift`] and records nothing
    /// — as is, in the Postgres adapter, a row whose stamps show a write
    /// the log did not hear of ([`changed_outside_the_doors`]: review of
    /// car ce5ce2de for a row with no fact, backlog 6c2aa86c for one with).
    ///
    /// For a Class a fresh database would NOT hold — declared through
    /// the batch door before it staged facts (2026-09-17). A
    /// migration-seeded row needs none: the migration is its birth. One
    /// backfilled anyway is harmless while it matches its seed (a
    /// declare onto an identical row replays as current), and names the
    /// drift if it does not.
    async fn backfill_declared(
        &self,
        class_ref: &ClassRef,
        stamp: &EventStamp,
    ) -> Result<Backfill, ClassError>;

    /// Record a NAMED birth (backlog 93f361af): for a Class with no fact
    /// whose row was changed outside the doors, the `class.declared` of
    /// `named.born` and the `class.updated` that took it to the row held,
    /// both built by [`backfilled_edit_events`] naming `named.source` and
    /// the row's own stamps, staged together only when [`backfill_edit`]
    /// stands behind them ([`Backfill::RecordedWithEdit`]). Writes no
    /// row. A Class the log already declares, and whose log replays to
    /// the row, records nothing ([`Backfill::AlreadyDeclared`]); anything
    /// else the proof refuses is [`ClassError::Drift`] and records
    /// nothing — as is, in the Postgres adapter, a row whose `updated_at`
    /// never moved off `created_at`, which no edit has touched.
    async fn backfill_edited(
        &self,
        class_ref: &ClassRef,
        named: &NamedBirth,
        stamp: &EventStamp,
    ) -> Result<Backfill, ClassError>;

    /// Record an OBSERVED birth (review of this car, backlog 6c2aa86c):
    /// the `class.declared` of [`observed_birth`], built by
    /// [`observed_declared_event`] naming `source`, the row's stamps and
    /// the drift they show ([`Backfill::Observed`]). The operator's act
    /// for a Class whose stamps refuse every other birth; no door records
    /// it by itself. Writes no row. A Class the log already declares, and
    /// whose log replays to the row, records nothing
    /// ([`Backfill::AlreadyDeclared`]); a log that does not explain the
    /// row is [`ClassError::Drift`].
    async fn backfill_observed(
        &self,
        class_ref: &ClassRef,
        source: &str,
        stamp: &EventStamp,
    ) -> Result<Backfill, ClassError>;

    /// What the edit and retire doors would do to each Class of
    /// `subject_kind` (every kind when `None`), retired ones included,
    /// read without writing ([`birth_plan`]), ordered by kind then code.
    async fn birth_plans(&self, subject_kind: Option<&str>) -> Result<Vec<BirthPlan>, ClassError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn stamp() -> EventStamp {
        EventStamp::new(
            "classes",
            boss_core::actor::ActorId::Automation("tenant-seed".into()),
        )
    }

    fn row(code: &str) -> Class {
        Class {
            subject_kind: "employee".into(),
            code: code.into(),
            display_name: code.into(),
            parent_code: None,
            member_attribute: Some("department".into()),
            metadata: json!({"is_management": false}),
            sort_order: 0,
            retired_at: None,
        }
    }

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 28, 3, minute, 0).unwrap()
    }

    /// The facts exactly as the write doors log them, read back the way
    /// a replay reads audit_log: kind + payload, through [`ClassFact::from_logged`].
    fn logged(event: &Event) -> (ClassRef, ClassFact) {
        ClassFact::from_logged(&event.kind, &event.payload).expect("a class fact parses")
    }

    #[test]
    fn a_change_applied_to_the_held_row_is_the_row_the_edit_asked_for() {
        let held = row("qa");
        let mut asked = held.clone();
        asked.display_name = "Quality".into();
        asked.parent_code = Some("ops".into());
        asked.metadata = json!({"is_management": true});
        let change = class_change(&held, &asked).unwrap();
        assert_eq!(
            change.changed,
            vec!["display_name", "parent_code", "metadata"]
        );
        assert_eq!(apply_change(&held, &change).unwrap(), asked);
        // The fact carries the same change, so its replay is the write.
        let event = updated_event(&stamp(), &held, &change).unwrap().unwrap();
        let (key, fact) = logged(&event);
        assert_eq!(key, ClassRef::new("employee", "qa"));
        assert_eq!(apply_fact(Some(&held), &fact).unwrap(), asked);
        // A restatement changes nothing and records nothing.
        let none = class_change(&held, &held).unwrap();
        assert!(none.is_empty());
        assert!(updated_event(&stamp(), &held, &none).unwrap().is_none());
    }

    #[test]
    fn a_change_that_names_the_key_is_refused_rather_than_applied() {
        let held = row("qa");
        let change = ClassChange {
            changed: vec!["code".into()],
            before: Map::from_iter([("code".to_string(), json!("qa"))]),
            after: Map::from_iter([("code".to_string(), json!("renamed"))]),
        };
        let err = apply_change(&held, &change).unwrap_err();
        assert!(err.to_string().contains("code"), "{err}");
    }

    /// A fresh database: the declare, an edit and the retire, replayed
    /// from nothing, arrive at the row the doors left — and a second
    /// replay onto that row writes nothing.
    #[test]
    fn a_declared_edited_and_retired_class_replays_from_nothing() {
        let declared = row("dept-ops");
        let mut edited = declared.clone();
        edited.display_name = "Operations".into();
        let change = class_change(&declared, &edited).unwrap();
        let mut retired = edited.clone();
        retired.retired_at = Some(at(1));
        let log: Vec<ClassFact> = [
            declared_event(&stamp(), &declared).unwrap(),
            updated_event(&stamp(), &declared, &change)
                .unwrap()
                .unwrap(),
            retired_event(&stamp(), &retired),
        ]
        .iter()
        .map(|e| logged(e).1)
        .collect();

        assert_eq!(replay_class(None, &log).unwrap(), Some(retired.clone()));
        assert_eq!(replay_class(Some(&retired), &log).unwrap(), None);
        assert_eq!(replay_class(Some(&declared), &[]).unwrap(), None);
    }

    /// A migration-seeded row with no `class.declared`: its edit and its
    /// retirement replay onto the seeded body.
    #[test]
    fn a_seeded_row_takes_its_logged_edit_and_retirement() {
        let seeded = row("recruiter");
        let mut edited = seeded.clone();
        edited.sort_order = 9;
        let change = class_change(&seeded, &edited).unwrap();
        let mut retired = edited.clone();
        retired.retired_at = Some(at(2));
        let log = vec![
            logged(&updated_event(&stamp(), &seeded, &change).unwrap().unwrap()).1,
            logged(&retired_event(&stamp(), &retired)).1,
        ];
        assert_eq!(replay_class(Some(&seeded), &log).unwrap(), Some(retired));
    }

    /// A row at neither end of its log was changed outside the doors:
    /// drift, named, never overwritten. So is a log that edits a row the
    /// registry does not carry, and a retire of a row already retired at
    /// another time.
    #[test]
    fn a_row_the_log_does_not_explain_is_drift() {
        let seeded = row("recruiter");
        let mut edited = seeded.clone();
        edited.display_name = "Talent".into();
        let change = class_change(&seeded, &edited).unwrap();
        let log = vec![logged(&updated_event(&stamp(), &seeded, &change).unwrap().unwrap()).1];

        let mut foreign = seeded.clone();
        foreign.display_name = "Somebody else's".into();
        let err = replay_class(Some(&foreign), &log).unwrap_err();
        assert!(matches!(err, ClassError::Drift(_)), "{err}");
        assert!(err.to_string().contains("display_name"), "{err}");

        let err = replay_class(None, &log).unwrap_err();
        assert!(matches!(err, ClassError::Drift(_)), "{err}");

        let mut retired = seeded.clone();
        retired.retired_at = Some(at(3));
        let mut elsewhen = seeded.clone();
        elsewhen.retired_at = Some(at(4));
        let log = vec![logged(&retired_event(&stamp(), &retired)).1];
        assert!(matches!(
            replay_class(Some(&elsewhen), &log),
            Err(ClassError::Drift(_))
        ));
    }

    /// A declare onto a row a migration already seeded with the same
    /// body is the state the declare left; a different body is drift.
    #[test]
    fn a_declare_onto_an_identical_row_is_current_and_onto_a_different_one_is_drift() {
        let declared = row("dept-ops");
        let log = vec![logged(&declared_event(&stamp(), &declared).unwrap()).1];
        assert_eq!(replay_class(Some(&declared), &log).unwrap(), None);
        let mut other = declared.clone();
        other.display_name = "Seeded otherwise".into();
        assert!(matches!(
            replay_class(Some(&other), &log),
            Err(ClassError::Drift(_))
        ));
    }

    #[test]
    fn a_fact_of_another_kind_is_not_a_class_fact() {
        assert!(ClassFact::from_logged("subject_kind.updated", &json!({})).is_err());
    }

    /// Backlog 9d345f9b: a Class the batch door inserted before it
    /// staged `class.declared` (2026-09-17) has no birth in the log, so
    /// its later retirement replays onto no row — drift, on a fresh
    /// database. The backfill declares the row as it stood before the
    /// first fact the log holds: active, the retire un-applied. Read
    /// back the way a replay reads it (the backfilled declare first), it
    /// rebuilds the live row from nothing and is current on the live one.
    #[test]
    fn a_class_born_before_the_declare_door_is_backfilled_as_the_row_it_was_born() {
        let mut live = row("hosting");
        live.retired_at = Some(at(5));
        let retire = logged(&retired_event(&stamp(), &live)).1;
        // The shape that stops the rebuild today.
        assert!(matches!(
            replay_class(None, std::slice::from_ref(&retire)),
            Err(ClassError::Drift(_))
        ));

        let born = backfill_birth(&live, std::slice::from_ref(&retire))
            .unwrap()
            .expect("no declare in the log, so there is a birth to record");
        assert_eq!(born, row("hosting"), "the row before its retirement");

        let event = backfilled_declared_event(&stamp(), &born).unwrap();
        assert_eq!(event.kind, CLASS_DECLARED);
        assert!(is_backfill(&event.payload));
        assert_eq!(
            event.payload[BACKFILLED],
            json!(format!(
                "backfilled from the live row on {}",
                event.timestamp.date_naive()
            ))
        );
        assert_eq!(
            event.payload["declared_by"],
            json!("automation:tenant-seed")
        );
        let log = vec![logged(&event).1, retire];
        assert_eq!(replay_class(None, &log).unwrap(), Some(live.clone()));
        assert_eq!(replay_class(Some(&live), &log).unwrap(), None);
        // A declare the door made is not a backfill.
        let door = declared_event(&stamp(), &born).unwrap();
        assert!(!is_backfill(&door.payload));
    }

    /// An edit logged after the birth is un-applied too, so the backfill
    /// is the row the edit found; a row with no fact at all is born as
    /// it stands.
    #[test]
    fn a_backfilled_birth_is_the_row_before_every_logged_edit() {
        let born = row("drafting-agent");
        let mut live = born.clone();
        live.display_name = "Drafting".into();
        live.sort_order = 4;
        let change = class_change(&born, &live).unwrap();
        let log = vec![logged(&updated_event(&stamp(), &born, &change).unwrap().unwrap()).1];
        assert_eq!(backfill_birth(&live, &log).unwrap(), Some(born));
        assert_eq!(backfill_birth(&live, &[]).unwrap(), Some(live.clone()));
    }

    /// A Class the log already declares needs no backfill (the door's
    /// fact, or a backfill already recorded — a repeat records nothing);
    /// and a row its own log cannot explain is drift, never recorded as
    /// a birth that would replay to a different row.
    #[test]
    fn a_declared_class_needs_no_backfill_and_an_unexplained_one_is_refused() {
        let live = row("support");
        let declared = logged(&declared_event(&stamp(), &live).unwrap()).1;
        assert_eq!(backfill_birth(&live, &[declared]).unwrap(), None);
        let backfilled = logged(&backfilled_declared_event(&stamp(), &live).unwrap()).1;
        assert_eq!(backfill_birth(&live, &[backfilled]).unwrap(), None);

        let mut live = row("product");
        live.retired_at = Some(at(6));
        let mut elsewhen = live.clone();
        elsewhen.retired_at = Some(at(7));
        let log = vec![logged(&retired_event(&stamp(), &elsewhen)).1];
        let err = backfill_birth(&live, &log).unwrap_err();
        assert!(matches!(err, ClassError::Drift(_)), "{err}");
    }

    /// Review of car ce5ce2de, LOW-3: "already declared" is an answer
    /// about the log, so it is checked against the log. A Class declared,
    /// deleted outside the doors (infra/postgres/example-reference-rows.sh
    /// deletes Class rows) and declared again with another body replays
    /// from nothing to drift — the second declare finds a row — so the
    /// backfill answers drift, never "nothing to do" while every fresh
    /// rebuild stops on it.
    #[test]
    fn an_already_declared_class_whose_log_does_not_replay_is_drift() {
        let first = row("sales");
        let mut again = first.clone();
        again.display_name = "Sales & Sponsorships".into();
        let log: Vec<ClassFact> = [&first, &again]
            .iter()
            .map(|c| logged(&declared_event(&stamp(), c).unwrap()).1)
            .collect();
        let err = backfill_birth(&again, &log).unwrap_err();
        assert!(matches!(err, ClassError::Drift(_)), "{err}");
        assert!(err.to_string().contains("sales"), "{err}");
    }

    fn invoice(metadata: Value) -> Class {
        Class {
            subject_kind: "invoice".into(),
            code: "hosting".into(),
            display_name: "Hosted instance".into(),
            parent_code: None,
            member_attribute: Some("revenue_category".into()),
            metadata,
            sort_order: 30,
            retired_at: None,
        }
    }

    /// Backlog 93f361af: a row with no fact whose body was changed by a
    /// bare UPDATE (before 10dabe13 gave edits a fact). Its birth is
    /// named from outside the log, and the backfill is the birth AND the
    /// edit — both marked, both naming the source — proved to replay
    /// from nothing to the live row and to write nothing onto it. Once
    /// recorded, the log declares the Class and a repeat records nothing.
    #[test]
    fn a_class_edited_outside_the_doors_is_backfilled_as_its_birth_and_that_edit() {
        let live = invoice(json!({"gl_account": "4400"}));
        let born = invoice(json!({}));
        let change = backfill_edit(&live, &[], &named(&born, &["metadata"]))
            .unwrap()
            .expect("an empty log, so there is a birth and an edit to record");
        assert_eq!(change.changed, vec!["metadata"]);
        assert_eq!(change.before["metadata"], json!({}));
        assert_eq!(change.after["metadata"], json!({"gl_account": "4400"}));

        let source = "algedonic-llc 28a7ac1^ seeds/classes.json";
        let (inserted, edited) = (at(1), at(7));
        let [declared, updated] = backfilled_edit_events(
            &stamp(),
            &born,
            &change,
            source,
            Some(inserted),
            Some(edited),
        )
        .unwrap();
        assert_eq!(
            (declared.kind.as_str(), updated.kind.as_str()),
            (CLASS_DECLARED, CLASS_UPDATED)
        );
        // Review of car 8778f12f, finding 2: the row's own stamps are the
        // only machine record of WHEN it was born and edited, and the next
        // door PUT moves one of them — so the facts carry them, and say
        // the edit's author is unknown rather than crediting the signer.
        assert_eq!(declared.payload[BORN_AT], json!(inserted));
        assert_eq!(updated.payload[EDITED_AT], json!(edited));
        assert_eq!(updated.payload.get(EDITED_BY), Some(&Value::Null));
        for e in [&declared, &updated] {
            assert!(is_backfill(&e.payload), "{e:?}");
            assert_eq!(e.payload[BACKFILL_SOURCE], json!(source));
            assert_eq!(
                e.payload[BACKFILLED],
                json!(format!(
                    "backfilled from {source} on {}",
                    e.timestamp.date_naive()
                ))
            );
        }
        let log = facts_in_replay_order(
            [&declared, &updated]
                .into_iter()
                .map(|e| (e.kind.as_str(), &e.payload)),
        )
        .unwrap();
        assert_eq!(
            log,
            vec![
                ClassFact::Declared(born.clone()),
                ClassFact::Updated(change)
            ]
        );
        assert_eq!(replay_class(None, &log).unwrap(), Some(live.clone()));
        assert_eq!(replay_class(Some(&live), &log).unwrap(), None);
        // Declared now: a repeat is nothing to do, whatever it names.
        assert_eq!(
            backfill_edit(&live, &log, &named(&born, &["metadata"])).unwrap(),
            None
        );
        assert_eq!(backfill_birth(&live, &log).unwrap(), None);
    }

    /// A named birth as a caller sends it, with `changed` the fields it
    /// says the edit made outside the doors changed.
    fn named(born: &Class, changed: &[&str]) -> NamedBirth {
        NamedBirth {
            born: born.clone(),
            changed: changed.iter().map(|f| f.to_string()).collect(),
            source: "algedonic-llc 28a7ac1^ seeds/classes.json".into(),
        }
    }

    /// Review of car 8778f12f, finding 1. A named birth is for a Class
    /// with NO fact, and that guard is what refuses the loophole — PUT
    /// through the door first, then name a birth — so the test asserts
    /// the guard's own refusal, with a birth that differs from the
    /// post-PUT row (a birth EQUAL to it is refused for another reason,
    /// "no edit to record", and proved nothing about the guard). The
    /// replay proof runs over the whole log too, so with the guard gone
    /// it still refuses, but in other words: this test goes red then.
    #[test]
    fn a_named_birth_is_refused_for_a_class_whose_log_holds_a_fact() {
        let live = invoice(json!({"gl_account": "4400"}));
        let mut emptied = live.clone();
        emptied.metadata = json!({});
        let put = class_change(&live, &emptied).unwrap();
        let log = vec![logged(&updated_event(&stamp(), &live, &put).unwrap().unwrap()).1];
        // The reviewer's probe: the born body differs from the row held.
        let born = invoice(json!({"x": 1}));
        let err = backfill_edit(&emptied, &log, &named(&born, &["metadata"])).unwrap_err();
        assert!(matches!(err, ClassError::Drift(_)), "{err}");
        assert!(err.to_string().contains("holds 1 fact(s)"), "{err}");

        let mut retired = emptied.clone();
        retired.retired_at = Some(at(9));
        let log = vec![logged(&retired_event(&stamp(), &retired)).1];
        let err = backfill_edit(&retired, &log, &named(&born, &["metadata"])).unwrap_err();
        assert!(err.to_string().contains("holds 1 fact(s)"), "{err}");
    }

    /// Review of car 8778f12f, finding 3: the caller says which fields
    /// the edit changed, and a named birth whose difference from the row
    /// is anything else is refused — a typo in `born` (a display name
    /// cased wrong) would otherwise record a permanent edit that never
    /// happened, and a repeat cannot correct it (it answers already
    /// declared). The order the caller lists them in does not matter.
    #[test]
    fn a_named_birth_is_refused_unless_it_differs_in_exactly_the_fields_named() {
        let live = invoice(json!({"gl_account": "4400"}));
        let mut typo = invoice(json!({}));
        typo.display_name = "Hosted Instance".into();
        let err = backfill_edit(&live, &[], &named(&typo, &["metadata"])).unwrap_err();
        assert!(matches!(err, ClassError::Drift(_)), "{err}");
        assert!(err.to_string().contains("display_name"), "{err}");

        let born = invoice(json!({}));
        for claimed in [&[][..], &["display_name"], &["metadata", "sort_order"]] {
            let err = backfill_edit(&live, &[], &named(&born, claimed)).unwrap_err();
            assert!(
                err.to_string().contains("the caller says"),
                "{claimed:?}: {err}"
            );
        }

        let mut two = invoice(json!({}));
        two.sort_order = 3;
        let change = backfill_edit(&live, &[], &named(&two, &["sort_order", "metadata"]))
            .unwrap()
            .expect("both fields named, in either order");
        assert_eq!(change.changed, vec!["metadata", "sort_order"]);
    }

    /// What a named birth may not do. It records one edit, so the birth
    /// may differ from the live row only in the editable body the edit
    /// carries — not in the key, and not in a retirement no fact
    /// records. It must differ (a birth equal to the row is the bodiless
    /// backfill). It is for a Class with NO fact: a log that already
    /// holds an edit or a retirement is the one the bodiless backfill
    /// un-applies — so "edit it through the door first, then name a
    /// birth" (the loophole triage refused: two edits that never
    /// happened) is drift. And its source is named, or nothing is built.
    #[test]
    fn a_named_birth_is_refused_unless_one_edit_explains_the_live_row() {
        let live = invoice(json!({"gl_account": "4400"}));
        let born = invoice(json!({}));
        let drift =
            |r: Result<Option<ClassChange>, ClassError>| matches!(r, Err(ClassError::Drift(_)));
        assert!(
            drift(backfill_edit(&live, &[], &named(&live, &[]))),
            "no edit to record"
        );

        let mut elsewhere = born.clone();
        elsewhere.code = "support".into();
        assert!(
            drift(backfill_edit(&live, &[], &named(&elsewhere, &["metadata"]))),
            "another key"
        );

        let mut retired = live.clone();
        retired.retired_at = Some(at(9));
        assert!(
            drift(backfill_edit(&retired, &[], &named(&born, &["metadata"]))),
            "a retirement no fact records"
        );

        let change = backfill_edit(&live, &[], &named(&born, &["metadata"]))
            .unwrap()
            .unwrap();
        for blank in ["", "  "] {
            assert!(matches!(
                backfilled_edit_events(&stamp(), &born, &change, blank, None, None),
                Err(ClassError::Drift(_))
            ));
        }
    }

    /// Backlog 6c2aa86c (b): what the row's two stamps can and cannot
    /// prove about a change made outside the doors, for a Class the log
    /// does not declare. An insert writes both stamps with one NOW(); the
    /// retire door writes `retired_at` and `updated_at` with one NOW() —
    /// so a row whose `updated_at` is its insert, or its newest fact's
    /// retirement, was written by nothing the log did not hear of AFTER
    /// that. Anything else was.
    #[test]
    fn the_stamps_prove_no_change_outside_the_doors_only_at_the_insert_or_the_newest_retirement() {
        let retired = |at| ClassFact::Retired(at);
        // Nothing wrote the row after its insert — with no fact, or with
        // facts a rebuild replayed (it leaves `updated_at` alone).
        assert_eq!(changed_outside_the_doors(at(1), at(1), &[]), None);
        assert_eq!(
            changed_outside_the_doors(at(1), at(1), &[retired(at(4))]),
            None
        );
        // The retire door wrote it last, and nothing since.
        assert_eq!(
            changed_outside_the_doors(at(1), at(4), &[retired(at(4))]),
            None
        );
        // Written after its insert with no fact, and after its newest
        // retirement: each a change the log never heard of.
        let why = changed_outside_the_doors(at(1), at(2), &[]).expect("moved, no fact");
        assert!(why.contains("no fact"), "{why}");
        let why = changed_outside_the_doors(at(1), at(5), &[retired(at(4))])
            .expect("moved after the newest fact");
        assert!(why.contains("newest fact"), "{why}");
        // An edit fact records no time it wrote the row (the door's
        // `updated_at` is the database's clock, the fact's timestamp the
        // service's), so it cannot vouch for the stamp.
        let held = row("edited");
        let mut asked = held.clone();
        asked.sort_order = 3;
        let edit = ClassFact::Updated(class_change(&held, &asked).unwrap());
        assert!(changed_outside_the_doors(at(1), at(5), &[edit]).is_some());
    }

    /// Backlog 6c2aa86c (a), with the operator's change: a door's fact on
    /// a Class the log does not declare is what disabled the stamp guard
    /// for good, so the door records the birth first — the row before
    /// every logged fact — when the stamps say nothing outside the doors
    /// touched it, and refuses, naming the named-birth door, when they
    /// say something did. A declared Class needs nothing.
    #[test]
    fn a_door_fact_on_an_undeclared_class_is_preceded_by_its_birth_or_refused() {
        let live = row("untouched");
        assert_eq!(
            undeclared_birth(&live, &[], None).unwrap(),
            Some(live.clone())
        );

        let mut retired = row("retired-first");
        retired.retired_at = Some(at(4));
        let log = vec![logged(&retired_event(&stamp(), &retired)).1];
        assert_eq!(
            undeclared_birth(&retired, &log, None).unwrap(),
            Some(row("retired-first")),
            "the birth is the row before its retirement"
        );

        let declared = vec![logged(&declared_event(&stamp(), &live).unwrap()).1];
        assert_eq!(
            undeclared_birth(&live, &declared, Some("moved".into())).unwrap(),
            None,
            "the log declares it: the door records nothing more"
        );

        let err = undeclared_birth(&live, &[], Some("it moved".into())).unwrap_err();
        assert!(matches!(err, ClassError::Drift(_)), "{err}");
        let err = err.to_string();
        assert!(err.contains("it moved"), "{err}");
        // Review of this car: the 409 names a door that WILL take the
        // Class — the observed birth always, the named birth only where
        // it applies (no fact, and no retirement it cannot record).
        let observed = "\"observed\": true";
        assert!(
            err.contains("/api/classes/employee/untouched/backfill-declared")
                && err.contains(observed)
                && err.contains("{born, changed, source}"),
            "an empty log on an active row names both doors: {err}"
        );
        for (row, log) in [
            (&retired, log.clone()),
            (
                &{
                    let mut r = row("migration-retired");
                    r.retired_at = Some(at(2));
                    r
                },
                vec![],
            ),
        ] {
            let err = undeclared_birth(row, &log, Some("it moved".into()))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(observed) && !err.contains("{born, changed, source}"),
                "the named birth would refuse {}, so it is not named: {err}",
                row.code
            );
        }
    }

    /// Review of this car (backlog 6c2aa86c): the three shapes no door
    /// took once the doors declared first. Each is recorded as observed —
    /// the live row with its logged facts un-applied, keeping a
    /// retirement no fact records — and replays from nothing to the live
    /// row, and onto it as current.
    #[test]
    fn an_observed_birth_takes_every_shape_the_stamps_refuse_and_replays_to_the_row() {
        // (a) retired by a migration: no fact, retirement kept.
        let mut migrated = row("triaging");
        migrated.retired_at = Some(at(2));
        // (b) retired through the door, then changed outside it: the
        // retirement comes off, the outside body stays (it is observed).
        let mut changed = row("retired-then-changed");
        changed.display_name = "Changed outside".into();
        changed.retired_at = Some(at(3));
        let retire = logged(&retired_event(&stamp(), &changed)).1;
        // (c) an edit fact from before the doors declared, no declare.
        let before = row("edited-early");
        let mut edited = before.clone();
        edited.sort_order = 8;
        let edit = logged(
            &updated_event(&stamp(), &before, &class_change(&before, &edited).unwrap())
                .unwrap()
                .unwrap(),
        )
        .1;
        for (live, log, want) in [
            (&migrated, vec![], migrated.clone()),
            (
                &changed,
                vec![retire],
                Class {
                    retired_at: None,
                    ..changed.clone()
                },
            ),
            (&edited, vec![edit], before.clone()),
        ] {
            let born = observed_birth(live, &log)
                .unwrap()
                .expect("undeclared, so a birth to record");
            assert_eq!(born, want, "{}", live.code);
            let event =
                observed_declared_event(&stamp(), &born, "measured", None, None, None).unwrap();
            // A backfill: read first, before the log it was appended after.
            assert!(is_backfill(&event.payload));
            let facts: Vec<ClassFact> = std::iter::once(logged(&event).1)
                .chain(log.iter().cloned())
                .collect();
            assert_eq!(
                replay_class(None, &facts).unwrap(),
                Some(live.clone()),
                "{}",
                live.code
            );
            assert_eq!(
                replay_class(Some(live), &facts).unwrap(),
                None,
                "{}",
                live.code
            );
            // Once recorded, a repeat is nothing to do.
            assert_eq!(observed_birth(live, &facts).unwrap(), None, "{}", live.code);
        }
    }

    /// A migration retires with its own NOW() on each database, so on a
    /// fresh one the observed birth of a migration-retired row finds the
    /// row retired at another time. That is the birth it records, retired
    /// as the log says: the replay writes the log's time, not drift. A
    /// row the fresh database holds ACTIVE, or differing otherwise, is
    /// still drift.
    #[test]
    fn a_retired_birth_onto_a_row_a_migration_retired_at_another_time_replays_to_the_logs_time() {
        let mut live = row("ready");
        live.retired_at = Some(at(2));
        let log = vec![ClassFact::Declared(live.clone())];
        let mut fresh = live.clone();
        fresh.retired_at = Some(at(9));
        assert_eq!(
            replay_class(Some(&fresh), &log).unwrap(),
            Some(live.clone())
        );
        assert_eq!(replay_class(Some(&live), &log).unwrap(), None);
        let active = row("ready");
        assert!(matches!(
            replay_class(Some(&active), &log),
            Err(ClassError::Drift(_))
        ));
        let mut other = fresh.clone();
        other.display_name = "Ready!".into();
        assert!(matches!(
            replay_class(Some(&other), &log),
            Err(ClassError::Drift(_))
        ));
    }

    /// The observed birth's fact says what it is: a backfill, observed,
    /// the operator's reason, the row's stamps and the drift — and it
    /// refuses a blank reason.
    #[test]
    fn an_observed_birth_names_its_reason_its_stamps_and_its_drift() {
        let born = row("archived");
        let event = observed_declared_event(
            &stamp(),
            &born,
            " retired by migration 20260926061106 ",
            Some(at(1)),
            Some(at(4)),
            Some("the row was written at 4".into()),
        )
        .unwrap();
        assert_eq!(event.kind, CLASS_DECLARED);
        assert!(is_backfill(&event.payload));
        assert_eq!(event.payload[BIRTH], json!(OBSERVED));
        assert_eq!(
            event.payload[BACKFILL_SOURCE],
            json!("retired by migration 20260926061106")
        );
        assert_eq!(event.payload[BORN_AT], json!(at(1)));
        assert_eq!(event.payload[ROW_UPDATED_AT], json!(at(4)));
        assert_eq!(event.payload[DRIFT], json!("the row was written at 4"));
        assert_eq!(logged(&event).1, ClassFact::Declared(born.clone()));
        for blank in ["", "  "] {
            assert!(matches!(
                observed_declared_event(&stamp(), &born, blank, None, None, None),
                Err(ClassError::Drift(_))
            ));
        }
    }

    /// The dry run: what the doors would do, from the same decision.
    #[test]
    fn a_birth_plan_says_what_the_door_would_do_and_why() {
        let live = row("planned");
        let declared = vec![logged(&declared_event(&stamp(), &live).unwrap()).1];
        let plan = birth_plan(&live, &declared, Some((at(1), at(5))));
        assert_eq!((plan.door, plan.declared, plan.facts), ("nothing", true, 1));

        let plan = birth_plan(&live, &[], Some((at(1), at(1))));
        assert_eq!((plan.door, plan.why.as_deref()), ("declare", None));
        assert_eq!(plan.created_at, Some(at(1)));

        let plan = birth_plan(&live, &[], Some((at(1), at(5))));
        assert_eq!(plan.door, "refuse");
        let why = plan.why.unwrap();
        assert!(why.contains("\"observed\": true"), "{why}");

        let plan = birth_plan(&live, &[], None);
        assert_eq!((plan.door, plan.updated_at), ("declare", None));
    }

    /// The facts as a log reader holds them, in replay order: a
    /// backfilled declare first (it records a birth that preceded the
    /// facts it was appended after), every other fact where the log
    /// put it.
    #[test]
    fn a_backfilled_declare_is_replayed_first_and_the_rest_in_log_order() {
        let born = row("hosting");
        let mut retired = born.clone();
        retired.retired_at = Some(at(8));
        let events = [
            retired_event(&stamp(), &retired),
            backfilled_declared_event(&stamp(), &born).unwrap(),
        ];
        let facts =
            facts_in_replay_order(events.iter().map(|e| (e.kind.as_str(), &e.payload))).unwrap();
        assert_eq!(
            facts,
            vec![ClassFact::Declared(born), ClassFact::Retired(at(8))]
        );
    }
}
