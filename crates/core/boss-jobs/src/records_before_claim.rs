//! The `records_before_claim` declaration on a step: first-record keys
//! that say how the step's work was STARTED, and so are written only
//! before anyone holds the step (backlog e6b05dd6, review fd7f1030 F1).
//!
//! WHY. An agent-run's Building step carries `launch.model`, the model
//! the launcher handed the harness, written through the first-record door
//! before the run exists; it is the only record a release counts for the
//! reviewer model (D2 of design fb9ba887). The honest launcher refuses a
//! run whose Building has begun, but that check lived only in the client.
//! The door refused nothing but a terminal step, and Active is its normal
//! case, so a run that started with no launcher record (the write failed
//! open, it was started by hand, or the hook did not recognise its
//! prompt) could claim its Building and then POST `launch.model` naming
//! its declared model: 201, and the release read `matches: true`. Writer
//! identity cannot tell them apart — the launcher and the run both sign
//! as the same agent. What CAN tell them apart is when: a launcher writes
//! before the claim, and a run only after it.
//!
//! THE DECLARATION IS PROTOCOL DATA, the channel `written_by` and
//! `human_only` ride: a Workflow step authors
//! `metadata_defaults.records_before_claim`, materialisation carries it
//! onto the packet's step, and [`crate::step_metadata_write::PROTOCOL_KEYS`]
//! freezes it there. Core names no key: agent-run.toml declares
//! `launch.model`.
//!
//! THREE CONSEQUENCES, each held by a door:
//! - the first-record door refuses a NEW record under a declared key once
//!   the step has EVER been claimed ([`refusal`]): the claim CAS marks
//!   the step ([`CLAIMED_KEY`], [`closed_by_claim`]) in its own write and
//!   a release leaves the mark, so release, record, re-claim forges
//!   nothing (re-review 211cbae4 B1); a replay of the stored record
//!   changes nothing and is answered as a replay;
//! - the merge door never writes a declared key, so a PATCH can neither
//!   stand in for the record nor squat on it ([`changed`]);
//! - a reader believes a record under a key only where the step declares
//!   it ([`declares`]), because only there could the door have refused a
//!   record the run wrote itself.

use boss_core::job::StepStatus;
use serde_json::Value;

/// The metadata key a Workflow step authors to list its before-claim keys.
pub const KEY: &str = "records_before_claim";

/// The mark the claim door leaves on a step carrying [`KEY`] the first
/// time it is claimed: the claim's instant. A release does not take it
/// off, so a step once claimed stays closed to its before-claim records.
pub const CLAIMED_KEY: &str = "records_before_claim_closed_at";

/// PURE: the keys `metadata` declares as written before the claim.
pub fn declared(metadata: &Value) -> Vec<&str> {
    metadata
        .get(KEY)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// PURE: `metadata` declares `key` as written before the claim.
pub fn declares(metadata: &Value, key: &str) -> bool {
    declared(metadata).contains(&key)
}

/// PURE: why a first record under `key` may not be written on a step in
/// `status` carrying `metadata`, or `None` when it may: the key is not
/// declared, or no one has ever claimed the step.
///
/// EVER, NOT NOW (re-review 211cbae4 B1). Asking only whether the step
/// is held now left a door its own holder could open: release the step
/// back to Ready (the supported release, 204), write the record, claim
/// again. So the claim leaves [`CLAIMED_KEY`] ([`closed_by_claim`]), a
/// release does not take it off, and its presence refuses here whatever
/// the status reads.
pub fn refusal(metadata: &Value, status: StepStatus, key: &str) -> Option<String> {
    let claimed = metadata.get(CLAIMED_KEY).is_some();
    if !declares(metadata, key)
        || (!claimed && matches!(status, StepStatus::Pending | StepStatus::Ready))
    {
        return None;
    }
    let shown = if claimed {
        "has been claimed".to_string()
    } else {
        serde_json::to_value(status)
            .ok()
            .and_then(|v| v.as_str().map(|s| format!("is {s}")))
            .unwrap_or_default()
    };
    Some(format!(
        "`{key}` is a record of how this step's work was started, which its protocol declares \
         ({KEY}) is written only before the step is first claimed, and the step {shown} — a \
         record written now would describe a start this caller did not make (backlog e6b05dd6)"
    ))
}

/// PURE: `metadata` as a claim at `at` leaves it. A step carrying the
/// declaration and not yet marked gets [`CLAIMED_KEY`] = `at`; every
/// other step, and a re-claim, is unchanged. The declaration is read by
/// PRESENCE, not by [`declared`], because the Postgres claim applies
/// this same rule inside its one UPDATE (`metadata ? …`), and one test
/// a statement can spell keeps the two adapters equal; a malformed
/// declaration only closes keys it declares none of.
pub fn closed_by_claim(metadata: &Value, at: &Value) -> Value {
    match metadata.as_object() {
        Some(obj) if obj.contains_key(KEY) && !obj.contains_key(CLAIMED_KEY) => {
            let mut obj = obj.clone();
            obj.insert(CLAIMED_KEY.to_string(), at.clone());
            Value::Object(obj)
        }
        _ => metadata.clone(),
    }
}

/// PURE: the declared keys whose value in `after` (the row as a merge
/// would leave it) differs from `stored`. Sorted; empty for a merge that
/// leaves every declared key as it stands.
pub fn changed<'a>(stored: &'a Value, after: &Value) -> Vec<&'a str> {
    let mut keys: Vec<&str> = declared(stored)
        .into_iter()
        .filter(|k| stored.get(*k) != after.get(*k))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_a_declared_key_on_a_held_step_is_refused() {
        let md = json!({KEY: ["launch.model"]});
        for status in [StepStatus::Pending, StepStatus::Ready] {
            assert_eq!(refusal(&md, status, "launch.model"), None);
        }
        for status in [
            StepStatus::Active,
            StepStatus::Completed,
            StepStatus::Skipped,
        ] {
            assert!(refusal(&md, status, "launch.model").is_some(), "{status:?}");
        }
        assert_eq!(refusal(&md, StepStatus::Active, "evidence"), None);
        assert_eq!(
            refusal(&json!({}), StepStatus::Active, "launch.model"),
            None
        );
        // A malformed declaration declares nothing rather than everything.
        assert!(declared(&json!({KEY: "launch.model"})).is_empty());
    }

    /// Once claimed, a release back to Ready does not reopen the key
    /// (re-review 211cbae4 B1): the claim's mark is what refuses.
    #[test]
    fn a_step_once_claimed_refuses_whatever_its_status_reads() {
        let at = json!("2026-10-10T00:40:00Z");
        let md = closed_by_claim(&json!({KEY: ["launch.model"]}), &at);
        assert_eq!(md[CLAIMED_KEY], at);
        for status in [StepStatus::Pending, StepStatus::Ready] {
            assert!(refusal(&md, status, "launch.model").is_some(), "{status:?}");
            assert_eq!(refusal(&md, status, "evidence"), None);
        }
        // A re-claim keeps the first claim's instant.
        let again = closed_by_claim(&md, &json!("2026-10-11T00:00:00Z"));
        assert_eq!(again, md);
        // A step that declares nothing is not marked by its claim.
        let plain = json!({"notes": "kept"});
        assert_eq!(closed_by_claim(&plain, &at), plain);
        assert_eq!(closed_by_claim(&Value::Null, &at), Value::Null);
    }

    #[test]
    fn a_merge_changes_a_declared_key_only_when_its_value_moves() {
        let stored = json!({KEY: ["launch.model"], "launch.model": {"model": "x"}});
        assert!(changed(&stored, &stored).is_empty());
        let mut after = stored.clone();
        after["launch.model"] = json!("y");
        assert_eq!(changed(&stored, &after), vec!["launch.model"]);
        after.as_object_mut().unwrap().remove("launch.model");
        assert_eq!(changed(&stored, &after), vec!["launch.model"]);
    }
}
