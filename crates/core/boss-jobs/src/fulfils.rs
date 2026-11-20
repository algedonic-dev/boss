//! The R8 edge: a child packet names the ONE step, on another packet,
//! that its outcome answers (factory-taxonomy-v2.md R8, "a step may be
//! fulfilled by packets one altitude down"; design 689d1f7f car 2,
//! backlog a3a367e4).
//!
//! `{"fulfils": {"job": "<parent packet>", "step": "<its step id>"}}` on
//! the CHILD's metadata. It is a LOCATOR, not an authority: it says where
//! the child's outcome goes, so the reader of that outcome follows one
//! edge instead of scanning every open parent for the one that names the
//! child. Whether the outcome may complete the step is the parent step's
//! own question, judged at the write — for a car's `gate` step, that the
//! step still names this gate-run as its holder (`car::edge_gate`), which
//! is the stamp the launch made under its fence.
//!
//! OUTCOME-AWARE, AND THAT IS WHY IT IS NOT `parent_step_id`. The
//! delegate-subjob edge (`jobs.spawn` stamps `parent_step_id` and
//! `parent_job_id`) is resolved by `resolve-subjob-on-child-job-closed`,
//! which completes the parent step when the child CLOSES, whatever it
//! closed as (`jobs_subjob_resolve`). A gate-run closes on a red too, and
//! a completed car gate step is the dock's authority to board — so a gate
//! edge spelled `parent_step_id` would have boarded red code. This key is
//! read only by a reader of the child's VERDICT, which completes the step
//! on the outcome that fulfils it and hands it back on any other. Another
//! child of the same shape (a protocol publish fulfilling its publication
//! step, backlog dcbeef5a) takes the same key and the same rule.

use serde_json::{Value, json};

/// The child-metadata key carrying the edge. One spelling, here: the
/// launch that writes it and every verdict reader that follows it.
pub const KEY: &str = "fulfils";

/// The parent packet and the step on it that a child's outcome answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fulfils {
    /// The parent packet's id.
    pub job: String,
    /// The step's id on that packet, as the packet reported it.
    pub step: String,
}

impl Fulfils {
    /// The merging job-metadata PATCH body that records this edge on the
    /// child.
    pub fn patch(&self) -> Value {
        json!({ KEY: { "job": self.job, "step": self.step } })
    }
}

/// PURE: the edge `metadata` carries. `Ok(None)` when it carries none —
/// absent or `null`, which is every child admitted before the edge
/// existed, so its reader keeps the path it had. `Err` when the key is
/// present but is not two non-blank ids: a half-written edge is not an
/// absence, and a reader must not quietly fall back to the scan the
/// edge exists to replace.
pub fn of(metadata: &Value) -> Result<Option<Fulfils>, String> {
    let Some(edge) = metadata.get(KEY).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let id = |k: &str| {
        edge.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match (id("job"), id("step")) {
        (Some(job), Some(step)) => Ok(Some(Fulfils { job, step })),
        _ => Err(format!(
            "`{KEY}` is present but does not name both a job and a step ({edge}) — an edge \
             that cannot be followed is refused, not read as no edge"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edge_round_trips_through_its_patch() {
        let edge = Fulfils {
            job: "car-1".into(),
            step: "s-gate".into(),
        };
        assert_eq!(of(&edge.patch()), Ok(Some(edge)));
    }

    /// Every child admitted before the edge existed reads as none, so
    /// its reader keeps the path it had (the cutover, design 689d1f7f).
    #[test]
    fn absent_or_null_is_no_edge() {
        assert_eq!(of(&json!({})), Ok(None));
        assert_eq!(of(&json!({ KEY: null })), Ok(None));
        assert_eq!(of(&Value::Null), Ok(None));
    }

    #[test]
    fn a_half_written_edge_is_refused_not_absent() {
        for bad in [
            json!({ KEY: {"job": "car-1"} }),
            json!({ KEY: {"job": "car-1", "step": "  "} }),
            json!({ KEY: "car-1" }),
        ] {
            assert!(of(&bad).is_err(), "{bad}");
        }
    }

    /// NOT THE SUBJOB EDGE. `resolve-subjob-on-child-job-closed` fires on
    /// `parent_step_id != null` and completes the parent step on ANY
    /// close; a gate edge under that name would complete a car's gate on
    /// a red (backlog a3a367e4).
    #[test]
    fn the_edge_never_spells_the_outcome_blind_subjob_keys() {
        let patch = Fulfils {
            job: "car-1".into(),
            step: "s-gate".into(),
        }
        .patch();
        let keys: Vec<&String> = patch.as_object().unwrap().keys().collect();
        assert_eq!(keys, [KEY]);
        assert_ne!(KEY, "parent_step_id");
        assert_ne!(KEY, "parent_job_id");
    }
}
