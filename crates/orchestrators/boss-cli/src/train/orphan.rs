//! A train orphaned before its PR — opened, never collected, holding
//! the track (backlog 238d70c3).
//!
//! Measured 2026-10-09: PR train 2026-10-09 09:19 (d1a8b140) was opened
//! by the conductor at 09:21:24Z, inside the conductor pod's restart
//! (the #1021 converge). Its `collect` step stayed ready for the
//! conductor with no car stamped, no `boarded_jobs` and no PR, and every
//! pass after it logged `track occupied` — the board held, the dock's
//! re-gates held — until the drain session cancelled it by hand 28
//! minutes later. Nothing would have cleared it on its own: `reconcile`
//! returned at the pr-step check for a train with no PR, and the stall
//! sentinel reads the newest step `completed_at`, which the `scheduled`
//! trigger does not carry, so it never even stamped the stall.
//!
//! WHY CANCEL AND NOT RESUME. The boarding pass is not resumable from
//! the record: the packet opens AFTER the consist is assembled in the
//! conductor's clone, checked and pushed, and which cars it carries
//! lives only in that pass's memory until the `boarded_jobs` merge that
//! follows the PR. A train orphaned before its PR has nothing on the
//! record to resume from (d1a8b140 had neither a list nor a PR), and
//! the one thing that IS idempotent before the PR is the whole board,
//! run fresh against main as it stands now: no car is stamped aboard a
//! train before its PR, so every car is still on the dock for it. So
//! the conductor cancels the orphan itself, with the reason on the
//! `cancelled` terminal, and the next board departs fresh.

use super::*;

/// How long an open train may stand before its PR step completes
/// before the conductor calls it orphaned and cancels it.
///
/// Measured 2026-10-09 over the 59 newest pr-trains carrying a PR: from
/// the packet's `opened_at` to the `pr` step's `completed_at` took 0 s
/// at the median and 27 s at the worst — boarding opens the packet,
/// creates the PR and completes three steps in one pass. Ten minutes is
/// twenty times that worst case and five reconcile passes (the
/// `train-reconcile` cadence rule fires every two minutes), so an
/// orphan clears within about twelve minutes instead of never.
///
/// A compiled bound and not a delivery-policy row, like
/// `ORPHAN_GATE_RUN_MINUTES`: it is not an operating-model choice but
/// a bound on the conductor's own code path, and it only has to be
/// longer than that path. Within one conductor the lock already
/// serializes boarding and reconcile, so a reconcile can never see a
/// board in progress; the bound is what keeps an overlapping pod roll —
/// the old pod's lock is on its own disk — from cancelling a train a
/// live pass is still opening.
pub(crate) const PRE_PR_ORPHAN_MINUTES: i64 = 10;

// Far shorter than the stall rule it stands in front of — the whole
// point (238d70c3 asked for exactly that) — held at compile time.
const _: () =
    assert!(PRE_PR_ORPHAN_MINUTES < crate::delivery_policy::COMPILED_STALL_HOURS * 60 / 6);

/// The instant the packet opened: the job's own `opened_at` column,
/// falling back to the `metadata.opened_at` stamp older readers use.
/// None when neither parses — no basis, never a guess.
fn opened_at(train: &Value) -> Option<DateTime<Utc>> {
    ["/opened_at", "/metadata/opened_at"]
        .iter()
        .find_map(|p| {
            train
                .pointer(p)
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        })
        .map(|t| t.with_timezone(&Utc))
}

/// Whole minutes an open train has stood before its PR, when that is at
/// least `bound_minutes`: the train is orphaned. None when its `pr`
/// step is done (the train left boarding; the red and stall rules own
/// it from there), when it is younger than the bound, or when no
/// opening instant can be read.
///
/// ONE predicate for the conductor that cancels and the orient line
/// that names it troubled, so the two cannot disagree about which
/// train is stuck (CLAUDE.md §Diagnosis: a troubled packet must look
/// troubled).
pub(crate) fn orphaned_before_pr(
    train: &Value,
    now: DateTime<Utc>,
    bound_minutes: i64,
) -> Option<i64> {
    if step_done(find_step(train, "pr", "Open the batched PR")) {
        return None;
    }
    let age = now.signed_duration_since(opened_at(train)?).num_minutes();
    (age >= bound_minutes).then_some(age)
}

/// The reason the conductor records on the orphan's `cancelled`
/// terminal — what was found, how long, and why nothing is struck.
pub(crate) fn orphaned_before_pr_reason(age_minutes: i64, bound_minutes: i64) -> String {
    format!(
        "orphaned before its PR: opened {age_minutes}m ago and its pr step never completed \
         (bound {bound_minutes}m; boarding opens a train and its PR in seconds) — the pass \
         that opened it died before collecting it, so nothing would, and it held the track. \
         Cancelled by the conductor so the next board departs fresh; no car is stamped aboard \
         a train before its PR, so none is released and none is struck (238d70c3)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::train::test_support::orphan_at_collect;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// THE REPRODUCTION, read as the predicate: the stall sentinel had
    /// no basis on this train at all (no step carries `completed_at`),
    /// so the six-hour rule the item quoted would never have fired —
    /// and the orphan bound reads it orphaned 28 minutes in.
    #[test]
    fn the_d1a8b140_shape_is_orphaned_and_the_stall_rule_never_saw_it() {
        let t = orphan_at_collect();
        let now = at("2026-10-09T09:49:00Z");
        assert_eq!(
            stall_age_hours(&t, now, 6),
            None,
            "the stall sentinel cannot see a train whose only completed step carries no stamp"
        );
        assert_eq!(
            orphaned_before_pr(&t, now, PRE_PR_ORPHAN_MINUTES),
            Some(27),
            "opened 09:21:24, read at 09:49:00: 27 whole minutes before its PR"
        );
    }

    #[test]
    fn a_train_inside_the_bound_is_a_board_still_opening_it() {
        let t = orphan_at_collect();
        assert_eq!(
            orphaned_before_pr(&t, at("2026-10-09T09:30:00Z"), PRE_PR_ORPHAN_MINUTES),
            None,
            "8 minutes old is under the 10-minute bound"
        );
        assert_eq!(
            orphaned_before_pr(&t, at("2026-10-09T09:31:24Z"), PRE_PR_ORPHAN_MINUTES),
            None,
            "9m59.5s is whole minutes 9 — still under the bound"
        );
        assert_eq!(
            orphaned_before_pr(&t, at("2026-10-09T09:31:25Z"), PRE_PR_ORPHAN_MINUTES),
            Some(10),
            "the bound itself is orphaned"
        );
    }

    /// Whichever step it stopped at — collect, assemble or the PR
    /// itself — a train whose `pr` step is not done is before its PR.
    #[test]
    fn a_train_stuck_at_assemble_is_still_before_its_pr() {
        let mut t = orphan_at_collect();
        t["steps"][1]["status"] = json!("completed");
        t["steps"][2]["status"] = json!("ready");
        assert_eq!(
            orphaned_before_pr(&t, at("2026-10-09T10:21:25Z"), PRE_PR_ORPHAN_MINUTES),
            Some(60)
        );
    }

    /// A train past its PR is the red and stall rules' to judge, however
    /// old: a two-hour CI is not an orphan.
    #[test]
    fn a_train_whose_pr_step_is_done_is_never_an_orphan() {
        let mut t = orphan_at_collect();
        t["steps"][3]["status"] = json!("completed");
        assert_eq!(
            orphaned_before_pr(&t, at("2026-10-10T09:21:24Z"), PRE_PR_ORPHAN_MINUTES),
            None
        );
    }

    /// The metadata stamp is the fallback for a row without the column,
    /// and a train with neither has no basis — never a guess.
    #[test]
    fn the_opening_instant_falls_back_to_the_metadata_stamp_and_absence_is_no_basis() {
        let mut t = orphan_at_collect();
        t.as_object_mut().unwrap().remove("opened_at");
        let now = at("2026-10-09T09:49:00Z");
        assert_eq!(orphaned_before_pr(&t, now, PRE_PR_ORPHAN_MINUTES), Some(27));
        t["metadata"].as_object_mut().unwrap().remove("opened_at");
        assert_eq!(orphaned_before_pr(&t, now, PRE_PR_ORPHAN_MINUTES), None);
    }

    #[test]
    fn the_reason_names_the_age_the_bound_and_that_nothing_is_struck() {
        let r = orphaned_before_pr_reason(27, 10);
        assert!(
            r.starts_with("orphaned before its PR: opened 27m ago"),
            "{r}"
        );
        assert!(r.contains("bound 10m"), "{r}");
        assert!(r.contains("none is struck"), "{r}");
    }
}
