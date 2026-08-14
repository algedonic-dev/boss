//! Static validation for `StationSpec` rows — the station **viability
//! lint**, the counterpart to [`crate::workflow_lint`].
//!
//! A Workflow is a program and its lint proves the program can finish.
//! A station is a *queue declaration*, and the failure mode is not a
//! program that cannot finish — it is a queue that silently holds
//! nothing, or that behaves differently from what the row says. Both
//! are invisible: an empty queue and a correctly-empty queue render
//! identically, so a station that can never match is indistinguishable
//! from a quiet day. That is the whole reason this file exists.
//!
//! Every rule below refuses a row that is wrong *on its own terms* —
//! provable from the spec alone, with no reference to what packets
//! happen to exist. A station holding nothing today is not a defect; a
//! station that could not hold anything on any day is.
//!
//! - **Contradictory metadata.** A key in both `metadata_present` and
//!   `metadata_absent`. No packet can satisfy both.
//! - **Terminal status with no window.** `status: closed|cancelled`
//!   while `terminal_window_days` is unset. The evaluation universe is
//!   in-flight packets (`http/stations.rs` filters `JobStatus::Open`
//!   unless a window is declared), so the clause can never match.
//! - **Inert window.** The mirror image: a `terminal_window_days`
//!   retention rule on a predicate pinned to a non-terminal status.
//!   The window is dead configuration — it reads as "departed packets
//!   linger here" and nothing ever will.
//! - **The `@me` biconditional.** `kind: actor` means the queue
//!   depends on who is asking, and [`SELF`] is the only thing that
//!   makes it so. An `actor` row without `@me` shows every executor
//!   the same queue while claiming to be personal; a shared row *with*
//!   `@me` varies per viewer while claiming to be shared — which is
//!   what made the census read a per-actor station as depth 0 and
//!   overstate orphaned packets at 92% when the real figure was 75%.
//! - **Non-positive WIP limit.** `wip_limit <= 0` declares a station
//!   that is over its bandwidth while empty. Retiring the row is how
//!   you close a station.
//! - **Self-parented rollup.** `rollup_parent == name` is a
//!   one-element cycle; a renderer walking parents does not return.
//!
//! Runs at author time (`POST /api/stations/_validate`) and at publish
//! time (inside `StationRegistry::publish`, in BOTH adapters, against
//! the row the transaction actually promotes). One definition, two
//! call sites — because the 2026-08-13 outage was precisely a
//! `_validate` that could name the problem and a publish path that
//! never asked it.
//!
//! **Not yet covered: boot.** A row INSERTed straight into `stations`
//! by a SQL seed never passes through `publish`, so it bypasses this
//! file entirely — the same hole `workflow_quarantine` closes for
//! Workflows. Every row live today is viable (checked against the
//! deployed registry when this landed), so the gap is latent rather
//! than active; closing it is tracked separately.

use crate::station_queue::{SELF, StationPredicate};
use crate::stations::{StationKind, StationSpec};
use boss_core::job::JobStatus;
use serde_json::Value;

/// One viability failure. `field` is the offending spec field (empty
/// for whole-station failures), mirroring
/// [`crate::workflow_lint::WorkflowLintError`]'s `step`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationLintError {
    pub station: String,
    pub field: String,
    pub reason: String,
}

impl std::fmt::Display for StationLintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.field.is_empty() {
            write!(f, "[{}] {}", self.station, self.reason)
        } else {
            write!(f, "[{}] `{}`: {}", self.station, self.field, self.reason)
        }
    }
}

fn err(spec: &StationSpec, field: &str, reason: impl Into<String>) -> StationLintError {
    StationLintError {
        station: spec.name.clone(),
        field: field.to_string(),
        reason: reason.into(),
    }
}

/// Whether a job status is terminal — a packet in this status has
/// left the network, so an in-flight queue can never hold it.
fn is_terminal(status: JobStatus) -> bool {
    matches!(status, JobStatus::Closed | JobStatus::Cancelled)
}

/// Validate a single StationSpec. Returns every violation found; an
/// empty Vec means the station is viable.
pub fn validate_station(spec: &StationSpec) -> Vec<StationLintError> {
    let mut errs = Vec::new();
    check_predicate(spec, &spec.predicate, &mut errs);
    check_retention(spec, &mut errs);
    check_self_binding(spec, &mut errs);
    check_bandwidth(spec, &mut errs);
    check_rollup(spec, &mut errs);
    errs
}

/// Validate every spec in a list. One call, every error reported —
/// for the seed bundles and the boot check.
pub fn validate_all(specs: &[StationSpec]) -> Vec<StationLintError> {
    specs.iter().flat_map(validate_station).collect()
}

/// **The publish gate.** A spec may occupy the ACTIVE slot only if it
/// is viable; `Err` carries every problem in the order the lint found
/// them.
///
/// Called by every registry write that can set `status = active`, in
/// BOTH adapters. Draft writes deliberately do NOT call it: a draft is
/// work in progress and may be saved in any state — the same posture
/// as [`crate::workflow_lint::gate_active`].
pub fn gate_active(spec: &StationSpec) -> Result<(), Vec<StationLintError>> {
    let errs = validate_station(spec);
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// The wire shape for a list of lint problems: `[{field, reason,
/// message}]`. One definition so the author-time dry run
/// (`_validate`, 200 + `ok:false`) and the publish refusal (422) hand
/// the editor the same JSON to render.
pub fn problems_json(errs: &[StationLintError]) -> Vec<Value> {
    errs.iter()
        .map(|e| {
            serde_json::json!({
                "field": e.field,
                "reason": e.reason,
                "message": e.to_string(),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// A key demanded present and absent at once. Reported once per key,
/// in a deterministic order, so two runs of the lint agree.
fn check_predicate(spec: &StationSpec, p: &StationPredicate, errs: &mut Vec<StationLintError>) {
    let mut both: Vec<&String> = p
        .metadata_present
        .iter()
        .filter(|k| p.metadata_absent.contains(k))
        .collect();
    both.sort();
    both.dedup();
    for key in both {
        errs.push(err(
            spec,
            "predicate.metadata_present",
            format!(
                "metadata key `{key}` is required both present and absent; \
                 no packet can satisfy both, so this queue is always empty"
            ),
        ));
    }
}

/// The two halves of the retention rule: a terminal status needs a
/// window to be reachable at all, and a window needs a terminal status
/// to do anything.
fn check_retention(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    match (spec.predicate.status, spec.terminal_window_days) {
        (Some(status), None) if is_terminal(status) => errs.push(err(
            spec,
            "predicate.status",
            format!(
                "status `{status:?}` is terminal but no `terminal_window_days` is declared; \
                 a station evaluates in-flight packets only, so this queue is always empty. \
                 Declare a retention window, or match an in-flight status."
            )
            .to_lowercase(),
        )),
        (Some(status), Some(days)) if !is_terminal(status) => errs.push(err(
            spec,
            "terminal_window_days",
            format!(
                "a {days}-day retention window is declared, but the predicate pins \
                 status `{status:?}`, which is never terminal; the window can never \
                 apply. Drop the window, or match a terminal status."
            )
            .to_lowercase(),
        )),
        _ => {}
    }
}

/// `kind: actor` ⟺ the predicate names `@me`. Both directions are
/// failures, and each is silent in its own way.
fn check_self_binding(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    let binds = spec.predicate.binds_self();
    match (spec.kind, binds) {
        (StationKind::Actor, false) => errs.push(err(
            spec,
            "predicate",
            format!(
                "an `actor` station serves one executor, but no clause names the \
                 `{SELF}` placeholder, so every executor sees the same queue. \
                 Bind a clause to `{SELF}`, or declare a different station kind."
            ),
        )),
        (kind, true) if kind != StationKind::Actor => errs.push(err(
            spec,
            "kind",
            format!(
                "the predicate names `{SELF}`, so this queue's contents depend on who \
                 is asking — but the station is declared `{}`, which is shared. A \
                 shared queue that varies per viewer cannot be reported on or handed \
                 off. Declare `actor`, or drop the `{SELF}` clause.",
                kind.as_str()
            ),
        )),
        _ => {}
    }
}

/// A limit that is exceeded while the queue is empty.
fn check_bandwidth(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    if let Some(limit) = spec.wip_limit
        && limit <= 0
    {
        errs.push(err(
            spec,
            "wip_limit",
            format!(
                "a wip_limit of {limit} is exceeded by an empty queue, so the station \
                 reports over_limit forever. Leave it unset for no limit, or retire \
                 the station to close it."
            ),
        ));
    }
}

/// A station that rolls up into itself.
fn check_rollup(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    if spec.rollup_parent.as_deref() == Some(spec.name.as_str()) {
        errs.push(err(
            spec,
            "rollup_parent",
            "a station cannot roll up into itself; a renderer walking parents \
             does not terminate"
                .to_string(),
        ));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station_queue::StepMatch;
    use chrono::TimeZone;
    use std::collections::BTreeMap;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0).unwrap()
    }

    /// A viable batch station: the shape the platform actually seeds.
    fn viable() -> StationSpec {
        StationSpec::draft(
            "loading-dock",
            "Loading dock",
            StationKind::Batch,
            StationPredicate {
                kind: Some("ship-a-change".into()),
                status: Some(JobStatus::Open),
                ..Default::default()
            },
            now(),
        )
    }

    fn reasons(errs: &[StationLintError]) -> Vec<&str> {
        errs.iter().map(|e| e.field.as_str()).collect()
    }

    #[test]
    fn the_platform_seed_shape_is_viable() {
        assert_eq!(validate_station(&viable()), Vec::new());
        assert!(gate_active(&viable()).is_ok());
    }

    #[test]
    fn a_key_required_present_and_absent_can_never_match() {
        let mut spec = viable();
        spec.predicate.metadata_present = vec!["branch".into(), "train".into()];
        spec.predicate.metadata_absent = vec!["train".into()];
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["predicate.metadata_present"]);
        assert!(errs[0].reason.contains("`train`"), "{}", errs[0].reason);
        // `branch` is present-only and must not be reported.
        assert!(!errs[0].reason.contains("branch"), "{}", errs[0].reason);
    }

    #[test]
    fn a_terminal_status_without_a_window_is_an_always_empty_queue() {
        let mut spec = viable();
        spec.predicate.status = Some(JobStatus::Closed);
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["predicate.status"]);
        assert!(errs[0].reason.contains("terminal_window_days"));
    }

    #[test]
    fn a_terminal_status_with_a_window_is_exactly_what_the_window_is_for() {
        let mut spec = viable();
        spec.predicate.status = Some(JobStatus::Closed);
        spec.terminal_window_days = Some(7);
        assert_eq!(validate_station(&spec), Vec::new());
    }

    #[test]
    fn a_window_on_a_non_terminal_predicate_is_dead_configuration() {
        let mut spec = viable();
        spec.terminal_window_days = Some(7); // predicate pins status: open
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["terminal_window_days"]);
    }

    #[test]
    fn a_window_with_no_status_clause_is_fine() {
        // No status clause = the universe opens up and the window
        // narrows it back down. That is the documented pairing.
        let mut spec = viable();
        spec.predicate.status = None;
        spec.terminal_window_days = Some(7);
        assert_eq!(validate_station(&spec), Vec::new());
    }

    #[test]
    fn an_actor_station_that_never_binds_self_is_not_personal_at_all() {
        let mut spec = viable();
        spec.kind = StationKind::Actor;
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["predicate"]);
        assert!(errs[0].reason.contains("@me"));
    }

    #[test]
    fn a_shared_station_that_binds_self_varies_per_viewer() {
        let mut spec = viable();
        spec.predicate.metadata_equals =
            BTreeMap::from([("submitted_by".into(), SELF.to_string())]);
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["kind"]);
        assert!(errs[0].reason.contains("batch"), "{}", errs[0].reason);
    }

    #[test]
    fn self_binding_through_the_step_clause_counts_the_same() {
        // binds_self() reads the step assignee too; the lint must not
        // have its own narrower idea of what "personal" means.
        let mut spec = viable();
        spec.kind = StationKind::Actor;
        spec.predicate.step = Some(StepMatch {
            assignee_id: Some(SELF.to_string()),
            ..Default::default()
        });
        assert_eq!(validate_station(&spec), Vec::new());
    }

    #[test]
    fn the_real_watchlist_row_is_viable() {
        // my-watchlist as actually declared: an actor station bound
        // through metadata_equals.
        let spec = StationSpec::draft(
            "my-watchlist",
            "My watchlist",
            StationKind::Actor,
            StationPredicate {
                metadata_equals: BTreeMap::from([("submitted_by".into(), SELF.to_string())]),
                ..Default::default()
            },
            now(),
        );
        assert_eq!(validate_station(&spec), Vec::new());
    }

    #[test]
    fn a_non_positive_wip_limit_is_over_limit_while_empty() {
        for limit in [0, -1] {
            let mut spec = viable();
            spec.wip_limit = Some(limit);
            assert_eq!(reasons(&validate_station(&spec)), vec!["wip_limit"]);
        }
        let mut ok = viable();
        ok.wip_limit = Some(1);
        assert_eq!(validate_station(&ok), Vec::new());
    }

    #[test]
    fn a_station_cannot_roll_up_into_itself() {
        let mut spec = viable();
        spec.rollup_parent = Some(spec.name.clone());
        assert_eq!(reasons(&validate_station(&spec)), vec!["rollup_parent"]);

        let mut ok = viable();
        ok.rollup_parent = Some("engineering".into());
        assert_eq!(validate_station(&ok), Vec::new());
    }

    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let mut spec = viable();
        spec.kind = StationKind::Actor; // no @me
        spec.wip_limit = Some(0);
        spec.rollup_parent = Some(spec.name.clone());
        let errs = validate_station(&spec);
        assert_eq!(
            reasons(&errs),
            vec!["predicate", "wip_limit", "rollup_parent"]
        );
        assert!(gate_active(&spec).is_err());
    }

    #[test]
    fn the_wire_shape_carries_field_reason_and_rendered_message() {
        let mut spec = viable();
        spec.wip_limit = Some(0);
        let errs = validate_station(&spec);
        let json = problems_json(&errs);
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["field"], "wip_limit");
        assert_eq!(
            json[0]["message"].as_str().unwrap(),
            format!("[loading-dock] `wip_limit`: {}", errs[0].reason)
        );
    }

    #[test]
    fn validate_all_reports_across_every_spec() {
        let mut bad = viable();
        bad.name = "broken".into();
        bad.wip_limit = Some(0);
        let errs = validate_all(&[viable(), bad]);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].station, "broken");
    }
}
