//! A PACKET'S PRIORITY MOVES THROUGH ITS OWN DOOR (backlog eb26059d).
//!
//! MEASURED 2026-10-18: the conductor raised 99b7ae76 from standard to
//! urgent, and no verb and no narrow route set the field. The only path
//! was the job PUT, which REPLACES the row: GET the whole job, edit one
//! field, PUT the whole body back, then diff the read-back by hand. A
//! metadata write landing between that GET and that PUT was erased, and
//! the record held no reason for the move — only an actor on a
//! `jobs.job.updated` event whose payload differed in one field.
//!
//! So `PUT /api/jobs/{id}/priority` (`boss job priority`) takes ONE field
//! and a reason. The adapter sets the column and appends a record to the
//! packet's reserved `priority_changes` list in one write, under the
//! row's lock, against the row as it stands at write time — so `from` is
//! the stored value the move replaced, never a copy a caller read
//! earlier — and records the `jobs.job.updated` state event built from
//! the post-write row. A finished packet is refused (its priority orders
//! nothing any more, and a closed record does not move); asking for the
//! priority a packet already has writes nothing and says so.
//!
//! The list is append-only and has this one writer: the metadata merge
//! door refuses the key, and the job PUT carries the stored list
//! forward over whatever its body holds, as it does `repins`.

use boss_core::job::Priority;
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The packet's reserved, append-only record of every priority move.
pub const PRIORITY_CHANGES_KEY: &str = "priority_changes";

/// What the metadata merge door answers a patch naming the list.
pub const PATCH_REFUSAL: &str = "`priority_changes` is a reserved, append-only list: it is \
     written only by the priority door, PUT /api/jobs/{id}/priority (`boss job priority`), in \
     the same write that moves the packet's priority. A metadata patch could rewrite or erase \
     the record of a move";

/// The body `PUT /api/jobs/{id}/priority` takes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriorityAsk {
    pub priority: Priority,
    pub why: String,
}

/// What a priority write did: the row as it now stands, the priority it
/// held before, and whether anything was written (`false` when the
/// packet already had the priority asked for).
#[derive(Debug, Clone)]
pub struct PriorityMove {
    pub job: boss_core::job::Job,
    pub from: Priority,
    pub changed: bool,
}

/// A priority as the wire and the column spell it (`urgent`).
pub fn word(p: Priority) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// One move, as the list records it: from, to, who, why, when. `by` is
/// the actor the write's event records, so a reader of the list and a
/// reader of the log agree on who moved it.
pub fn record(
    from: Priority,
    to: Priority,
    by: &str,
    why: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Value {
    json!({
        "from": word(from),
        "to": word(to),
        "by": by,
        "why": why,
        "at": at.to_rfc3339(),
    })
}

/// The metadata patch that appends `entry` to the stored list. A stored
/// value that is not a list is not a record this door wrote, and it is
/// refused rather than overwritten.
pub fn appended(stored_metadata: &Value, entry: Value) -> Result<Map<String, Value>, String> {
    let mut list = match stored_metadata.get(PRIORITY_CHANGES_KEY) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list)) => list.clone(),
        Some(other) => {
            return Err(format!(
                "metadata.{PRIORITY_CHANGES_KEY} holds {other}, not a list — it was not written \
                 by the priority door, and a move is not recorded over it"
            ));
        }
    };
    list.push(entry);
    let mut patch = Map::new();
    patch.insert(PRIORITY_CHANGES_KEY.into(), Value::Array(list));
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-18T15:40:00Z")
            .unwrap()
            .into()
    }

    #[test]
    fn a_move_records_from_to_who_why_and_when() {
        let r = record(
            Priority::Standard,
            Priority::Urgent,
            "agent-claude",
            "the SoR is dark",
            at(),
        );
        assert_eq!(
            r,
            json!({"from": "standard", "to": "urgent", "by": "agent-claude",
                   "why": "the SoR is dark", "at": "2026-10-18T15:40:00+00:00"})
        );
    }

    #[test]
    fn the_list_grows_by_one_and_keeps_what_it_held() {
        let first = appended(&json!({"other": 1}), json!({"n": 1})).unwrap();
        assert_eq!(first[PRIORITY_CHANGES_KEY], json!([{"n": 1}]));
        let stored = json!({PRIORITY_CHANGES_KEY: [{"n": 1}]});
        let second = appended(&stored, json!({"n": 2})).unwrap();
        assert_eq!(second[PRIORITY_CHANGES_KEY], json!([{"n": 1}, {"n": 2}]));
        assert_eq!(second.len(), 1, "the patch names only the list");
    }

    #[test]
    fn a_list_the_door_did_not_write_is_refused_not_overwritten() {
        let err = appended(&json!({PRIORITY_CHANGES_KEY: "hand-typed"}), json!({})).unwrap_err();
        assert!(err.contains("not a list"), "{err}");
    }

    #[test]
    fn the_ask_reads_the_wire_spelling_and_refuses_an_unknown_word() {
        let ask: PriorityAsk =
            serde_json::from_value(json!({"priority": "emergency", "why": "w"})).unwrap();
        assert_eq!(ask.priority, Priority::Emergency);
        assert!(
            serde_json::from_value::<PriorityAsk>(json!({"priority": "high", "why": "w"})).is_err()
        );
        assert!(serde_json::from_value::<PriorityAsk>(json!({"priority": "urgent"})).is_err());
        assert_eq!(word(Priority::Urgent), "urgent");
    }
}
