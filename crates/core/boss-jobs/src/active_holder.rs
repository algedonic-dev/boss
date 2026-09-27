//! An ACTIVE step keeps the holder that claimed it until it is released.
//!
//! A claim is the Ready→Active compare-and-set, and it records who won
//! it. Two step PUTs used to take an active step off that actor without
//! a claim of their own:
//!
//! - a PUT naming a DIFFERENT holder (backlog 650ebd0c, the review of
//!   car e341f7cd): a redelivered dispatcher nomination wrote its pick
//!   over the claimant about a second after the claim;
//! - a PUT CLEARING the holder while the step stays Active (backlog
//!   0f42efa0, the review of car fb3e9424): `{"assignee_id":null}` or
//!   `""` answered 204 and left an active step nobody held, so the next
//!   PUT naming anyone met no holder to protect and installed them —
//!   the first defect again, in two writes.
//!
//! So both are refused, and the one way an active step changes hands is
//! the RELEASE — `{"status":"ready","assignee_id":null}` in ONE body,
//! the body the abandoned-step reclaim and `boss step release` send —
//! followed by a claim through the CAS: two writes, each on the record.
//! A release names nobody; naming someone in it is the one-write
//! reassignment again.
//!
//! AND THE NEXT HOLDER ARRIVES THROUGH THE CLAIM (backlog 6ef4a36b, the
//! review of car 781b9209). Once a step was released it was Ready, and
//! a PUT could take a Ready step to Active naming anyone — the
//! PUT-as-claim path the platform step surfaces' Start and the sim's
//! workforce used to start work. Design 611fbffd (answered 2026-09-26)
//! decided who may START one: the claim door reserves as the PUT did
//! (one function, `start_hold`, both doors), and a claim FOR someone
//! else is admitted only for the executor the step declares or a holder
//! of `step-assign` ([`nominee`], [`declared_executor`]). Every start
//! moved to the claim door, and then the PUT's move to Active from
//! Ready or Pending became a refusal ([`opens_by_put`]), so release,
//! then claim, is the only way round — enforced, not hinted.
//!
//! `""` and a blank are the same clear as `null`: every reader of the
//! holder here (and the dispatcher's assignee check) reads them as
//! nobody, so a rule that only knew `null` would leave `""` as the way
//! round it. And a blank is STORED as nobody ([`stored`]), because the
//! claim CAS is the one reader that did not read it that way.
//!
//! The refusal's body is built HERE, once, and the dispatcher's test
//! reads this builder's output rather than a hand copy of its shape
//! (CLAUDE.md §9a) — the dispatcher acks a nomination this refuses as
//! "somebody holds it".

use boss_core::job::StepStatus;
use serde_json::Value;

/// The way to move an active step to another holder, named in the
/// refusal that stops a PUT doing it in one write (backlog 650ebd0c).
pub const HINT: &str = "an active step changes hands in two writes: free it with \
     {\"status\":\"ready\",\"assignee_id\":null}, then the next holder claims it through \
     POST .../claim. A nominator that finds the step already held has nothing to do.";

/// The error line of the refusal.
pub const ERROR: &str = "step is active and held — a PUT does not replace or clear its holder unless it releases the step";

/// A holder value that names someone: `None`, `""` and a blank do not.
fn named(holder: Option<&str>) -> Option<&str> {
    holder.filter(|h| !h.trim().is_empty())
}

/// The holder a step PUT stores: a blank is stored as `None` (backlog
/// 6ef4a36b, the review of car 781b9209). Every reader here reads a
/// blank as nobody, but both claim CASes admit only a NULL holder or
/// the claimant, so a stored `Some("")` was a holder to the one door
/// that hands a step out — a `""` release answered 204 and left a
/// ready step no one could ever claim (409 `holder: ""`).
pub fn stored(holder: Option<String>) -> Option<String> {
    holder.filter(|h| !h.trim().is_empty())
}

/// Is this write refused? `old_*` is the stored row; `next_*` is the
/// row the PUT would write (the body overlaid on it). Only an ACTIVE
/// step whose stored holder names someone is judged: there is nothing
/// to keep on any other.
pub fn refuses(
    old_status: StepStatus,
    old_holder: Option<&str>,
    next_status: StepStatus,
    next_holder: Option<&str>,
) -> bool {
    if old_status != StepStatus::Active {
        return false;
    }
    let Some(holder) = named(old_holder) else {
        return false;
    };
    match named(next_holder) {
        // A different name, whatever the status beside it: a release
        // names nobody, so this is a reassignment either way.
        Some(next) => next != holder,
        // A clear is a release only when the same body moves the step
        // out of Active.
        None => next_status == StepStatus::Active,
    }
}

/// Does this PUT OPEN the step — take it to Active from a status no one
/// has claimed it in (backlog 6ef4a36b, finding (2) of the review of
/// car 781b9209; design 611fbffd clause (b))? A claim is the one start:
/// it is the compare-and-set that decides who wins, and it judges who
/// may start a step for someone else, the station and the budget. A PUT
/// judged none of those, so `{"status":"active","assignee_id":X}` after
/// a release installed a holder who never claimed. Every in-tree start
/// moved to the claim door first (the web's Start, the sim's workforce,
/// the ops runner), so this is refused, never judged. An Active step's
/// own writes do not open it, and a terminal is frozen elsewhere.
pub fn opens_by_put(old_status: StepStatus, next_status: StepStatus) -> bool {
    matches!(old_status, StepStatus::Pending | StepStatus::Ready)
        && next_status == StepStatus::Active
}

/// The refusal of a PUT that opens a step, naming the door that does:
/// in the terminal freeze's shape (`step_status` + `refused_fields`),
/// plus the claim door's path so a caller can go there without reading
/// prose. Built once, so the test reads this and not a hand copy.
pub fn opening_refusal_body(job_id: &str, step_id: &str, from: &str) -> Value {
    serde_json::json!({
        "error": OPENING_ERROR,
        "step_id": step_id,
        "step_status": from,
        "refused_fields": ["status"],
        "claim_door": format!("/api/jobs/{job_id}/steps/{step_id}/claim"),
        "hint": OPENING_HINT,
    })
}

/// The error line of the opening refusal.
pub const OPENING_ERROR: &str =
    "a step becomes active only through a claim — a PUT does not start it";

/// What to do instead, named in the opening refusal.
pub const OPENING_HINT: &str = "POST /api/jobs/{id}/steps/{step_id}/claim starts a READY step \
     (?claimed_for=<id> to start it for someone else, which only the executor the protocol \
     declares or a holder of step-assign may do). A pending step is opened by its protocol, \
     not by hand; its ready_when must hold before anyone can claim it. Save the step's \
     fields without a status, or through PATCH .../metadata.";

/// The holder a claim names when it is NOT the caller (design
/// 611fbffd): `claimed_for` read as a holder is read everywhere here —
/// a blank names nobody — and the caller's own id is an ordinary claim
/// for oneself, so neither is a claim on someone else's behalf.
///
/// It compares SPELLINGS, so it is only the first filter: the claim
/// door resolves what it returns (an agent's login to its registered
/// id) and treats a nominee that resolves to the caller as a claim for
/// oneself before any rule judges it (backlog 5d1c0b7a).
pub fn nominee<'a>(claimed_for: Option<&'a str>, caller: &str) -> Option<&'a str> {
    named(claimed_for.map(str::trim)).filter(|n| *n != caller)
}

/// The executor the PROTOCOL declares for a step — the `individual`
/// audience (`{individual = "automation:boss-step"}`) of the step named
/// `spec_slug` in `pinned`, the Workflow row at the version the packet
/// was admitted under (or moved to, on the record, by `boss job
/// convert`). That actor may start the step for someone else without
/// the `step-assign` authority (design 611fbffd, Q1 (a)); a role,
/// station or department audience names no one, and a step with no
/// slug (an ad-hoc add, a pre-slug row) declares no one.
///
/// NEVER FROM THE STEP'S METADATA (the adversarial review of car
/// 611fbffd, 2026-09-26). This read `metadata.audience`, which any
/// step writer could PATCH: a caller with no `step-assign` wrote
/// `{"audience":{"individual":<self>}}` (204) and then claimed the
/// step for anyone (200), logged as an authorised claim-for. The row
/// is registry data no step write reaches, so it is the one place the
/// declaration is still the protocol's.
pub fn declared_executor(
    pinned: &crate::registry::WorkflowSpec,
    spec_slug: Option<&str>,
) -> Option<String> {
    let slug = spec_slug?;
    let step = pinned.steps.iter().find(|s| s.title == slug)?;
    match step.audience.as_ref()? {
        crate::audience::Audience::Individual(id) => named(Some(id)).map(str::to_string),
        _ => None,
    }
}

/// The 403 a claim for someone else gets from anyone the rule does not
/// admit, naming the rule and the two ways through it.
pub fn claim_for_refusal_body(step_id: &str, caller: &str, nominee: &str) -> Value {
    serde_json::json!({
        "error": "a claim for someone else (claimed_for) is made only by the step's \
                  declared executor or a holder of the step-assign authority",
        "step_id": step_id,
        "caller": caller,
        "claimed_for": nominee,
        "hint": "claim the step for yourself (omit claimed_for), or ask a holder of \
                 Update on step-assign to start it for them",
    })
}

/// The error line of the claim door's 409, by the status the CAS found
/// (backlog 5d1c0b7a). One line — "step already claimed or not
/// claimable" — answered every conflict, so a READY step someone else
/// holds (a nomination, or the executor materialisation hands it to)
/// read as claimed, and the reader went looking for a claim that was
/// never made. Only an active step has been claimed.
pub fn claim_conflict_error(status: &str) -> &'static str {
    match status {
        "active" => "step already claimed or not claimable",
        "ready" => "step not claimable by this claim: it is ready and held by someone else",
        _ => "step not claimable in its status: only a ready step is claimed",
    }
}

/// The 409's body, in the terminal freeze's shape (`step_status` +
/// `refused_fields`), naming who holds the step.
pub fn refusal_body(step_id: &str, holder: &str) -> Value {
    serde_json::json!({
        "error": ERROR,
        "step_id": step_id,
        "step_status": "active",
        "holder": holder,
        "refused_fields": ["assignee_id"],
        "hint": HINT,
    })
}

#[cfg(test)]
mod tests {
    use super::{claim_conflict_error, declared_executor, nominee, refuses, stored};

    #[test]
    fn only_an_active_step_is_called_already_claimed() {
        assert!(claim_conflict_error("active").contains("already claimed"));
        for status in ["ready", "pending", "completed", "skipped"] {
            let line = claim_conflict_error(status);
            assert!(!line.contains("already claimed"), "{status}: {line}");
            assert!(line.contains("not claimable"), "{status}: {line}");
        }
    }

    #[test]
    fn a_nominee_is_someone_other_than_the_caller() {
        assert_eq!(nominee(None, "emp-a"), None);
        assert_eq!(nominee(Some(""), "emp-a"), None);
        assert_eq!(nominee(Some("  "), "emp-a"), None);
        assert_eq!(nominee(Some("emp-a"), "emp-a"), None);
        assert_eq!(nominee(Some(" emp-a "), "emp-a"), None);
        assert_eq!(nominee(Some("emp-b"), "emp-a"), Some("emp-b"));
    }

    #[test]
    fn only_an_individual_audience_in_the_pinned_row_declares_an_executor() {
        use crate::audience::Audience;
        use crate::registry::{StepSpec, WorkflowSpec};
        let step = |slug: &str, audience: Option<Audience>| StepSpec {
            title: slug.into(),
            kind: "task".into(),
            ready_when: "true".into(),
            audience,
            ..Default::default()
        };
        let row = WorkflowSpec::platform_seed(
            "k",
            "K",
            "platform",
            vec![],
            vec![
                step(
                    "run",
                    Some(Audience::Individual("automation:boss-step".into())),
                ),
                step("review", Some(Audience::Role("platform-admin".into()))),
                step("blank", Some(Audience::Individual(" ".into()))),
                step("plain", None),
            ],
        );
        assert_eq!(
            declared_executor(&row, Some("run")),
            Some("automation:boss-step".into())
        );
        assert_eq!(declared_executor(&row, Some("review")), None);
        assert_eq!(declared_executor(&row, Some("blank")), None);
        assert_eq!(declared_executor(&row, Some("plain")), None);
        assert_eq!(declared_executor(&row, Some("gone")), None);
        assert_eq!(declared_executor(&row, None), None);
    }

    #[test]
    fn a_blank_holder_is_stored_as_nobody() {
        assert_eq!(stored(None), None);
        assert_eq!(stored(Some(String::new())), None);
        assert_eq!(stored(Some(" \t".into())), None);
        assert_eq!(stored(Some("emp-a".into())), Some("emp-a".into()));
    }
    use boss_core::job::StepStatus::{Active, Completed, Pending, Ready};

    const H: Option<&str> = Some("emp-holder");

    #[test]
    fn a_different_holder_is_refused_in_any_status() {
        assert!(refuses(Active, H, Active, Some("emp-other")));
        assert!(refuses(Active, H, Ready, Some("emp-other")));
    }

    #[test]
    fn a_clear_is_refused_unless_the_step_leaves_active() {
        for cleared in [None, Some(""), Some("  ")] {
            assert!(refuses(Active, H, Active, cleared), "{cleared:?}");
            assert!(!refuses(Active, H, Ready, cleared), "{cleared:?}");
            assert!(!refuses(Active, H, Pending, cleared), "{cleared:?}");
            assert!(!refuses(Active, H, Completed, cleared), "{cleared:?}");
        }
    }

    #[test]
    fn the_holder_itself_and_steps_nobody_holds_pass() {
        assert!(!refuses(Active, H, Active, H));
        assert!(!refuses(Active, H, Completed, H));
        assert!(!refuses(Active, None, Active, None));
        assert!(!refuses(Active, Some(""), Active, Some("emp-other")));
        assert!(!refuses(Ready, H, Ready, Some("emp-other")));
        assert!(!refuses(Ready, H, Ready, None));
    }
}
