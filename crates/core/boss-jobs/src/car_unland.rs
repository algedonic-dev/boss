//! The writes that close a car whose LANDING MAIN LOST, and open the
//! successor that rides again — the pure half of `boss car unland`
//! (backlog f9256445, design d812f1b7 D2 and its Q1).
//!
//! WHY. Train 2026-09-25 20:04 (PR #687) merged car dec3136a as squash
//! commit c85941b4 at 20:10:41Z; by 20:11:14Z forge main was back at
//! 777a5888, and the next train was assembled on that real main. The
//! record said the car landed ("landed on main as c85941b423bc" on its
//! review step, `merged: "true"` on the job) and its `proven` step stood
//! ready — for a change main does not carry. None of the five terminals
//! said what happened: `merged` needs a proof no probe can give, a probe
//! run now would close it `disproved` (a claim measured false, when it
//! was never measured), `abandoned` says the work was given up when it
//! must ride again, `settled` says there was nothing to merge, and
//! `landed-twin` says it landed inside another car.
//!
//! ONE TERMINAL, `unlanded` (David answered Q1 on 2026-09-25 as
//! recommended). `aborted`, because the delivery did not complete; an
//! aborted terminal completes from any open state, so its three required
//! fields — `merge_ref` (the merge main lost), `evidence` (the ancestry
//! reading that proved it) and `superseded_by` (the successor car) — are
//! its gate, written onto the step BEFORE the marker readies it, as
//! `landed-twin` and `disproved` are. Nobody reaches it without them.
//!
//! NEVER A REOPEN. The log keeps both facts: the landing (the review
//! step is left as it was, and a CORRECTION is appended beside its note
//! through the corrections door) and the loss (this terminal). The work
//! rides again as a SUCCESSOR car for the same branch — the same
//! `custom` Subject, so the branch's history shows both — linked both
//! ways: `supersedes` on the new packet (a declared job edge, so the id
//! is ref-checked) and `superseded_by` on this one. The successor carries
//! the predecessor's scope with the predecessor named, its build is
//! recorded as carried, and its first open work is `gate`: only a gate
//! on CURRENT main vouches for what will land, and a rebase that can
//! conflict is a builder's judgement, taken through `boss gate
//! --park-file`, which the auto-park handler ADOPTS onto this successor.
//!
//! The item the car answered is NOT closed: the arrival rule fires on
//! `merged` only, and the successor carries the same `backlog_item`, so
//! the item closes when the work really lands.
//!
//! What counts as "main lost it" is the CLI's to read (git lives there);
//! this module refuses to write a blank reading and pins the writes
//! against the real router in `tests/ship_a_change_unlanded.rs`.

use serde_json::{Map, Value, json};

use crate::car::{
    ALSO_ANSWERS, BACKLOG_ITEM, BUILD, BUILD_SLUG, PARTIAL_ITEM, REVIEW, REVIEW_SLUG, SCOPE,
    SCOPE_SLUG, StepWrite, find_step, is_open,
};
pub use crate::car_retire::SUPERSEDED_BY;
use crate::car_retire::{EVIDENCE, RetireWrites};

/// The `unlanded` outcome's slug and title.
pub const UNLANDED_SLUG: &str = "unlanded";
pub const UNLANDED: &str = "Unlanded — main lost its merge; a successor rides again";
/// The job-metadata marker its `ready_when` waits on.
pub const UNLANDED_MARKER: &str = "unlanded";
/// The merge main lost — a required field on the step.
pub const MERGE_REF: &str = "merge_ref";
/// The same reading on the job, so the packet says why it closed.
pub const UNLAND_EVIDENCE: &str = "unland_evidence";
/// The successor's edge back to this car — the declared `*` relation
/// edge "the packet this one replaces" (design c0d2787a).
pub const SUPERSEDES: &str = "supersedes";

/// The review note the conductor writes at landing starts with this —
/// `close_boarded_cars` writes `landed on main as <merge_ref>`.
const LANDED_NOTE: &str = "landed on main as";

/// An unlanding, already judged by the caller: git read that the merge
/// is not an ancestor of the forge's main, and the successor exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unlanding {
    /// The merge main lost, as the car records it.
    pub merge_ref: String,
    /// The reading: forge main at the read, when, and the ancestry
    /// command's answer.
    pub evidence: String,
    /// The FULL id of the successor car.
    pub superseded_by: String,
    /// RFC3339, the instant of the reading.
    pub completed_at: String,
}

fn md<'a>(c: &'a Value, k: &str) -> Option<&'a str> {
    c.pointer(&format!("/metadata/{k}"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn short(car: &Value) -> &str {
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    &id[..8.min(id.len())]
}

fn status(step: &Value) -> &str {
    step.get("status").and_then(Value::as_str).unwrap_or("?")
}

/// Do two commit identifiers name the same commit — prefix containment,
/// gated at 7 characters a side (the conductor's `commits_match`).
/// Public since backlog 090720c3: the dispatcher's
/// `jobs.converge_trains_on_rolled` asks the same question of a train's
/// twelve-character `merge_ref` and the runner's full sha, and reads it
/// here rather than writing another copy.
pub fn same_commit(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    a.len() >= 7 && b.len() >= 7 && (a.starts_with(b) || b.starts_with(a))
}

/// PURE: can `car` be unlanded for `merge_ref`? Checked BEFORE git is
/// asked anything. Open (a closed car is history), landed by the
/// conductor's marker, and the merge named is the one the car records —
/// an unlanding names the merge it says was lost, and a different sha is
/// a different question.
pub fn unlandable(car: &Value, merge_ref: &str) -> Result<(), String> {
    let id = short(car);
    if !is_open(car) {
        return Err(format!(
            "car {id} is already {} (outcome {}) — there is nothing left to unland",
            car.get("status").and_then(Value::as_str).unwrap_or("?"),
            md(car, "outcome").unwrap_or("none recorded")
        ));
    }
    if md(car, "merged") != Some("true") {
        return Err(format!(
            "car {id} has not landed (no `merged` marker) — only a landing can be lost"
        ));
    }
    let Some(recorded) = md(car, MERGE_REF) else {
        return Err(format!(
            "car {id} records no `merge_ref`, so nothing says which merge it landed as"
        ));
    };
    if !same_commit(recorded, merge_ref) {
        return Err(format!(
            "car {id} landed as {recorded}, not {} — name the merge the car records",
            merge_ref.trim()
        ));
    }
    Ok(())
}

/// PURE: the writes that close `car` through `unlanded`, or the refusal.
/// [`unlandable`] first; then no blank field, and the car's pinned
/// protocol must carry the terminal, still untaken.
pub fn unland_writes(car: &Value, u: &Unlanding) -> Result<RetireWrites, String> {
    unlandable(car, &u.merge_ref)?;
    let id = short(car);
    for (key, v) in [
        (MERGE_REF, &u.merge_ref),
        (EVIDENCE, &u.evidence),
        (SUPERSEDED_BY, &u.superseded_by),
    ] {
        if v.trim().is_empty() {
            return Err(format!(
                "an unlanding records the merge main lost, the reading that proved it and \
                 the successor, and `{key}` is blank — no evidence is not a pass"
            ));
        }
    }
    if car.get("id").and_then(Value::as_str) == Some(u.superseded_by.trim()) {
        return Err(format!("car {id} cannot be its own successor"));
    }
    let Some(outcome) = find_step(car, UNLANDED_SLUG, UNLANDED) else {
        return Err(format!(
            "car {id} is pinned to ship-a-change v{}, which has no `{UNLANDED_SLUG}` \
             terminal. An in-flight packet keeps the version it was admitted under; `boss \
             job convert {id}` moves it to the active version, and if that version has no \
             `{UNLANDED_SLUG}` either, `boss workflow publish ship-a-change` is owed first.",
            car.get("workflow_version")
                .map(Value::to_string)
                .unwrap_or_else(|| "?".into())
        ));
    };
    if !matches!(status(outcome), "pending" | "ready") {
        return Err(format!(
            "car {id}'s `{UNLANDED_SLUG}` terminal is {} — it cannot be taken again",
            status(outcome)
        ));
    }
    let outcome_id = outcome
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("car {id}'s `{UNLANDED_SLUG}` step has no id"))?;
    let merge_ref = u.merge_ref.trim();
    let evidence = u.evidence.trim();
    let by = u.superseded_by.trim();
    Ok(RetireWrites {
        outcome: StepWrite {
            step_id: outcome_id.to_string(),
            title: UNLANDED,
            metadata: json!({
                (MERGE_REF): merge_ref,
                (EVIDENCE): evidence,
                (SUPERSEDED_BY): by,
                "completed_at": u.completed_at,
            }),
            status_body: json!({"status": "completed"}),
        },
        // A landed car's review step is done; there is no hold to lift.
        held_review: None,
        marker: json!({
            (UNLANDED_MARKER): "true",
            (SUPERSEDED_BY): by,
            (UNLAND_EVIDENCE): evidence,
        }),
    })
}

/// PURE: the correction the car's review note is owed — `(reads,
/// should_read)` for the corrections door — or `None` when there is
/// nothing to correct: no landing note, or a correction of it already
/// handed to the step (the verb runs again after a partial pass).
///
/// `reads` is the stored note VERBATIM, because the door refuses an
/// excerpt that is not in the stored text.
pub fn review_correction(car: &Value, lost: &str) -> Option<(String, String)> {
    let review = find_step(car, REVIEW_SLUG, REVIEW)?;
    let note = review
        .pointer("/metadata/note")
        .and_then(Value::as_str)
        .filter(|n| n.contains(LANDED_NOTE))?;
    let corrected = review
        .get("corrections")
        .and_then(Value::as_array)
        .is_some_and(|cs| {
            cs.iter()
                .any(|c| c.get("field").and_then(Value::as_str) == Some("note"))
        });
    if corrected {
        return None;
    }
    Some((note.to_string(), lost.trim().to_string()))
}

/// The successor's scope and build as the predecessor declared them,
/// read off its steps (the job's `summary` is the fallback for a car
/// filed before its scope carried one).
fn declared<'a>(car: &'a Value, slug: &str, title: &str, key: &str) -> Option<&'a str> {
    find_step(car, slug, title)
        .and_then(|s| s.pointer(&format!("/metadata/{key}")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// PURE: the successor car's packet body, or the refusal. The same
/// branch as its `custom` Subject, the same owner, the same summary and
/// title, every item edge the predecessor carried (so the item closes
/// when the work really lands), and `supersedes` naming the predecessor.
pub fn successor_body(pred: &Value) -> Result<Value, String> {
    let id = pred
        .get("id")
        .and_then(Value::as_str)
        .ok_or("the car has no id")?;
    let branch =
        md(pred, "branch").ok_or_else(|| format!("car {} names no branch", short(pred)))?;
    let summary = declared(pred, SCOPE_SLUG, SCOPE, "summary")
        .or_else(|| md(pred, "summary"))
        .ok_or_else(|| format!("car {} declares no summary to carry", short(pred)))?;
    let mut metadata = Map::new();
    metadata.insert("branch".into(), json!(branch));
    metadata.insert("summary".into(), json!(summary));
    metadata.insert(SUPERSEDES.into(), json!(id));
    for key in [BACKLOG_ITEM, PARTIAL_ITEM, "delivery_channel"] {
        if let Some(v) = md(pred, key) {
            metadata.insert(key.into(), json!(v));
        }
    }
    if let Some(also) = pred
        .pointer(&format!("/metadata/{ALSO_ANSWERS}"))
        .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    {
        metadata.insert(ALSO_ANSWERS.into(), also.clone());
    }
    Ok(json!({
        "kind": "ship-a-change",
        "title": pred.get("title").and_then(Value::as_str).unwrap_or(summary),
        "subject": {"subject_kind": "custom", "id": branch},
        "owner_id": pred.get("owner_id").and_then(Value::as_str).unwrap_or(""),
        "priority": pred.get("priority").and_then(Value::as_str).unwrap_or("standard"),
        "status": "open",
        "tags": [],
        "metadata": Value::Object(metadata),
    }))
}

/// PURE: the writes that bring a freshly opened successor to its first
/// open work, `gate` — `scope` with the predecessor's boundary and
/// `build` recorded as carried, each naming the predecessor. A step
/// already taken is skipped, so a re-run writes nothing twice.
pub fn successor_writes(
    successor: &Value,
    pred: &Value,
    lost: &str,
    at: &str,
) -> Result<Vec<StepWrite>, String> {
    let pid = short(pred);
    let summary = declared(pred, SCOPE_SLUG, SCOPE, "summary")
        .or_else(|| md(pred, "summary"))
        .unwrap_or("");
    let excludes = declared(pred, SCOPE_SLUG, SCOPE, "excludes").unwrap_or("(none declared)");
    let test = declared(pred, BUILD_SLUG, BUILD, "test").unwrap_or("(none recorded)");
    let lost = lost.trim();
    let wants = [
        (
            SCOPE_SLUG,
            SCOPE,
            json!({
                "summary": summary,
                "excludes": format!(
                    "{excludes} — carried from car {pid}, whose landing main lost ({lost})"
                ),
                "completed_at": at,
            }),
        ),
        (
            BUILD_SLUG,
            BUILD,
            json!({
                "test": format!(
                    "carried from car {pid}, whose landing main lost ({lost}): {test} — the \
                     change rides again as built; its gate re-runs on current main"
                ),
                "completed_at": at,
            }),
        ),
    ];
    let mut out = Vec::new();
    for (slug, title, metadata) in wants {
        let step = find_step(successor, slug, title)
            .ok_or_else(|| format!("the successor has no `{slug}` step"))?;
        if matches!(status(step), "completed" | "skipped") {
            continue;
        }
        let step_id = step
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("the successor's `{slug}` step has no id"))?;
        out.push(StepWrite {
            step_id: step_id.to_string(),
            title,
            metadata,
            status_body: json!({"status": "completed"}),
        });
    }
    Ok(out)
}

// -- the second reading: a merge main REVERTED (backlog 7f4694eb) --------
//
// WHY. Car 526974b0 merged in train #1784 (d6c3a7037, 2026-11-16 03:48Z),
// failed every converge, and main REVERTED it in train #1787 (car
// 6c272983, f325cf542); its intent landed in car bfa6f7be. A revert
// leaves the merge an ancestor of main, so the lost-merge reading above
// refused it, and every other terminal records a wrong fact — so the car
// stood open at `proven` for four days, holding its item open through
// `item_awaits_its_car`. The same `unlanded` terminal is the truth ("main
// does not carry this car's change"), read a second way: the merge IS an
// ancestor, and a revert that main also carries undoes all of it. The
// successor is NAMED, not opened — it already exists, and usually has
// already landed.

/// The commit main reverted the merge in — on the job, beside the
/// terminal, so the packet names it as data rather than prose.
pub const REVERTED_BY: &str = "reverted_by";
/// The revert's car, when the revert was named by its car.
pub const REVERTED_BY_CAR: &str = "reverted_by_car";

fn kind(c: &Value) -> &str {
    c.get("kind").and_then(Value::as_str).unwrap_or("?")
}

/// PURE: the merge a REVERT CAR landed as — the commit `--reverted-by
/// <car>` names. Only a ship-a-change car closed with outcome `merged`:
/// a revert still in flight has reverted nothing, and git's reading of
/// that commit is what says whether it reverted THIS merge.
pub fn revert_car_merge(revert: &Value) -> Result<&str, String> {
    let id = short(revert);
    if kind(revert) != "ship-a-change" {
        return Err(format!(
            "{id} is a {} packet, not a car — name the revert's car or its merge sha",
            kind(revert)
        ));
    }
    if revert.get("status").and_then(Value::as_str) != Some("closed")
        || md(revert, "outcome") != Some("merged")
    {
        return Err(format!(
            "revert car {id} is not closed merged (status {}, outcome {}) — a revert that has \
             not landed has reverted nothing",
            revert.get("status").and_then(Value::as_str).unwrap_or("?"),
            md(revert, "outcome").unwrap_or("none recorded")
        ));
    }
    md(revert, MERGE_REF).ok_or_else(|| {
        format!("revert car {id} records no `merge_ref`, so nothing says which commit it landed as")
    })
}

/// PURE: the evidence that `successor` carries the intent of `car`, whose
/// merge main reverted — or the refusal. Another ship-a-change car, open
/// or closed merged, answering the car's own `backlog_item` (as its own
/// `backlog_item`, or in `also_answers`), and not already superseding a
/// different car: `supersedes` is one edge, and overwriting it would
/// erase a recorded fact.
pub fn reverted_successor(car: &Value, successor: &Value) -> Result<String, String> {
    let (id, sid) = (short(car), short(successor));
    if car.get("id") == successor.get("id") {
        return Err(format!("car {id} cannot be its own successor"));
    }
    if kind(successor) != "ship-a-change" {
        return Err(format!(
            "{sid} is a {} packet, not a car — a successor is a ship-a-change car",
            kind(successor)
        ));
    }
    let landed = successor.get("status").and_then(Value::as_str) == Some("closed")
        && md(successor, "outcome") == Some("merged");
    if !is_open(successor) && !landed {
        return Err(format!(
            "car {sid} is closed without landing (outcome {}) — a spent car carries nothing",
            md(successor, "outcome").unwrap_or("none recorded")
        ));
    }
    let Some(item) = md(car, BACKLOG_ITEM) else {
        return Err(format!(
            "car {id} names no `backlog_item`, so nothing on the record says what its \
             successor must answer"
        ));
    };
    let also = successor
        .pointer(&format!("/metadata/{ALSO_ANSWERS}"))
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(item)));
    let how = if md(successor, BACKLOG_ITEM) == Some(item) {
        BACKLOG_ITEM
    } else if also {
        ALSO_ANSWERS
    } else {
        return Err(format!(
            "car {sid} does not answer item {} (neither its `{BACKLOG_ITEM}` nor in \
             `{ALSO_ANSWERS}`) — a successor answers the item car {id} answered",
            &item[..8.min(item.len())]
        ));
    };
    if let Some(other) = md(successor, SUPERSEDES)
        && Some(other) != car.get("id").and_then(Value::as_str)
    {
        return Err(format!(
            "car {sid} already supersedes car {} — one car replaces one",
            &other[..8.min(other.len())]
        ));
    }
    Ok(format!(
        "its intent {} ({}), and it answers the same item {} as its {how}",
        successor_standing(successor),
        md(successor, "branch").unwrap_or("?"),
        &item[..8.min(item.len())]
    ))
}

/// PURE: where the named successor's intent stands, as the record says
/// it. A successor LANDED only on a record of its merge: closed with
/// outcome `merged`, or still open carrying the conductor's `merged`
/// marker (a car awaiting proof). Anything else carries the intent and
/// has not landed it — an open car the verb admits is not a delivery,
/// and writing "landed" for one is an immutable claim nothing observed
/// (review b279aa0c, 2026-11-20).
pub fn successor_standing(successor: &Value) -> String {
    let sid = short(successor);
    if is_open(successor) && md(successor, "merged") == Some("true") {
        return format!(
            "landed as car {sid} (merged as {}, not yet closed)",
            md(successor, "merge_ref").unwrap_or("an unrecorded ref")
        );
    }
    if successor.get("status").and_then(Value::as_str) == Some("closed")
        && md(successor, "outcome") == Some("merged")
    {
        return format!("landed as car {sid} (closed merged)");
    }
    format!("rides as car {sid}, which has not landed")
}

/// PURE: the back-edge the named successor is owed — `{supersedes: car}`
/// — or `None` when it already carries it (a re-run writes nothing).
pub fn supersedes_patch(car_id: &str, successor: &Value) -> Option<Value> {
    (md(successor, SUPERSEDES) != Some(car_id)).then(|| json!({ (SUPERSEDES): car_id }))
}

/// PURE: the job metadata naming the revert, or `None` when a pass
/// before this one wrote it.
pub fn reverted_by_patch(car: &Value, revert: &str, revert_car: Option<&str>) -> Option<Value> {
    let done = md(car, REVERTED_BY) == Some(revert)
        && revert_car.is_none_or(|c| md(car, REVERTED_BY_CAR) == Some(c));
    if done {
        return None;
    }
    let mut m = Map::new();
    m.insert(REVERTED_BY.into(), json!(revert));
    if let Some(c) = revert_car {
        m.insert(REVERTED_BY_CAR.into(), json!(c));
    }
    Some(Value::Object(m))
}

/// The successor already opened for `pred_id`, if a pass before this one
/// got that far — so a re-run finishes the unlanding rather than opening
/// a second successor. Any status: a successor that has since landed is
/// still the successor.
pub fn successor_of<'a>(cars: &'a [Value], pred_id: &str) -> Option<&'a Value> {
    cars.iter().find(|c| {
        c.pointer(&format!("/metadata/{SUPERSEDES}"))
            .and_then(Value::as_str)
            == Some(pred_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Car dec3136a's shape at 21:15Z on 2026-09-25: landed by the
    /// conductor (review completed with its landing note, the `merged`
    /// marker and `merge_ref` on the job), `proven` ready, the
    /// `unlanded` terminal pending.
    fn landed_car() -> Value {
        json!({
            "id": "dec3136a-0000-4000-8000-000000000001",
            "kind": "ship-a-change",
            "status": "open",
            "title": "A stamp cannot land on a moved shape",
            "owner_id": "emp-david",
            "priority": "standard",
            "workflow_version": 4,
            "metadata": {
                "branch": "fix/a-stamp-cannot-land-on-a-moved-shape",
                "summary": "A stamp cannot land on a moved shape.",
                "merged": "true",
                "merge_ref": "c85941b423bc",
                "backlog_item": "11111111-0000-4000-8000-000000000002",
                "train": "c94d5d39-34ad-4247-83de-eeee3f81534c",
            },
            "steps": [
                {"id": "s-scope", "spec_slug": "scope", "title": SCOPE, "status": "completed",
                 "metadata": {"summary": "A stamp cannot land on a moved shape.",
                              "excludes": "the claim door's UI"}},
                {"id": "s-build", "spec_slug": "build", "title": BUILD, "status": "completed",
                 "metadata": {"test": "boss-jobs 12 new tests"}},
                {"id": "s-review", "spec_slug": "review", "title": REVIEW,
                 "status": "completed",
                 "metadata": {"note": "landed on main as c85941b423bc", "pr_url": "x/pulls/687"}},
                {"id": "s-proven", "spec_slug": "proven", "title": "Proven in prod",
                 "status": "ready"},
                {"id": "s-unlanded", "spec_slug": UNLANDED_SLUG, "title": UNLANDED,
                 "status": "pending"},
            ],
        })
    }

    fn unlanding() -> Unlanding {
        Unlanding {
            merge_ref: "c85941b423bc".into(),
            evidence: "forge main 777a5888 at 2026-09-25T21:19:00Z; merge-base \
                       --is-ancestor c85941b423bc 777a5888 exited 1"
                .into(),
            superseded_by: "22222222-0000-4000-8000-000000000003".into(),
            completed_at: "2026-09-25T21:19:00Z".into(),
        }
    }

    /// THE WRITES: the three required fields onto `unlanded`, then the
    /// marker its `ready_when` waits on with the successor on the job
    /// too — and no hold, because a landed car has none.
    #[test]
    fn a_car_whose_landing_main_lost_closes_through_unlanded_naming_its_successor() {
        let w = unland_writes(&landed_car(), &unlanding()).unwrap();
        assert_eq!(w.outcome.step_id, "s-unlanded");
        assert_eq!(w.outcome.metadata["merge_ref"], "c85941b423bc");
        assert!(
            w.outcome.metadata["evidence"]
                .as_str()
                .unwrap()
                .contains("exited 1")
        );
        assert_eq!(
            w.outcome.metadata["superseded_by"],
            "22222222-0000-4000-8000-000000000003"
        );
        assert_eq!(w.outcome.status_body, json!({"status": "completed"}));
        assert_eq!(w.held_review, None);
        assert_eq!(w.marker["unlanded"], "true");
        assert_eq!(
            w.marker["superseded_by"],
            "22222222-0000-4000-8000-000000000003"
        );
    }

    /// Not landed, already closed, a different merge — each refused
    /// before git is asked, each naming why.
    #[test]
    fn a_car_that_is_not_unlandable_is_refused_naming_why() {
        let mut never = landed_car();
        never["metadata"]["merged"] = json!("false");
        assert!(
            unlandable(&never, "c85941b423bc")
                .unwrap_err()
                .contains("has not landed")
        );
        let mut closed = landed_car();
        closed["status"] = json!("closed");
        closed["metadata"]["outcome"] = json!("merged");
        assert!(
            unlandable(&closed, "c85941b423bc")
                .unwrap_err()
                .contains("already closed (outcome merged)")
        );
        let e = unlandable(&landed_car(), "777a5888504f").unwrap_err();
        assert!(
            e.contains("landed as c85941b423bc, not 777a5888504f"),
            "{e}"
        );
        // The full sha names the same merge as the 12-char record.
        assert!(unlandable(&landed_car(), "c85941b423bcdf85f1576ab3b4bdfee45004eea7").is_ok());
    }

    /// No evidence is not a pass; a car is not its own successor; a
    /// version without the terminal names the conversion.
    #[test]
    fn a_blank_reading_a_self_successor_and_a_missing_terminal_are_refused() {
        for blank in ["evidence", "superseded_by"] {
            let mut u = unlanding();
            match blank {
                "evidence" => u.evidence = " ".into(),
                _ => u.superseded_by = String::new(),
            }
            let e = unland_writes(&landed_car(), &u).unwrap_err();
            assert!(e.contains(&format!("`{blank}` is blank")), "{e}");
        }
        let mut u = unlanding();
        u.superseded_by = "dec3136a-0000-4000-8000-000000000001".into();
        assert!(
            unland_writes(&landed_car(), &u)
                .unwrap_err()
                .contains("its own successor")
        );
        let mut v3 = landed_car();
        v3["steps"].as_array_mut().unwrap().pop();
        let e = unland_writes(&v3, &unlanding()).unwrap_err();
        assert!(e.contains("boss job convert dec3136a"), "{e}");
    }

    /// The review note is corrected ONCE, quoting the stored note
    /// verbatim — and not at all when a correction of it is already
    /// handed to the step.
    #[test]
    fn the_landing_note_is_corrected_once_quoting_it_verbatim() {
        let lost = "merged as c85941b423bc; main lost it; not on main";
        let (reads, should) = review_correction(&landed_car(), lost).unwrap();
        assert_eq!(reads, "landed on main as c85941b423bc");
        assert_eq!(should, lost);
        let mut done = landed_car();
        done["steps"][2]["corrections"] = json!([{"index": 0, "field": "note"}]);
        assert_eq!(review_correction(&done, lost), None);
    }

    /// THE SUCCESSOR: the same branch and Subject, the same summary,
    /// owner and item edge, and `supersedes` naming the predecessor;
    /// its scope and build carried with the predecessor named, so its
    /// first open work is `gate`.
    #[test]
    fn the_successor_rides_the_same_branch_and_names_its_predecessor() {
        let body = successor_body(&landed_car()).unwrap();
        assert_eq!(body["kind"], "ship-a-change");
        assert_eq!(
            body["subject"],
            json!({"subject_kind": "custom", "id": "fix/a-stamp-cannot-land-on-a-moved-shape"})
        );
        assert_eq!(body["owner_id"], "emp-david");
        assert_eq!(
            body["metadata"]["supersedes"],
            "dec3136a-0000-4000-8000-000000000001"
        );
        assert_eq!(
            body["metadata"]["backlog_item"],
            "11111111-0000-4000-8000-000000000002"
        );
        assert_eq!(
            body["metadata"].get("merged"),
            None,
            "a successor has not landed"
        );
        assert_eq!(body["metadata"].get("train"), None, "nor boarded");

        let successor = json!({"id": "s2", "steps": [
            {"id": "n-scope", "spec_slug": "scope", "title": SCOPE, "status": "ready"},
            {"id": "n-build", "spec_slug": "build", "title": BUILD, "status": "pending"},
        ]});
        let w = successor_writes(
            &successor,
            &landed_car(),
            "c85941b423bc",
            "2026-09-25T21:20:00Z",
        )
        .unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].step_id, "n-scope");
        let excludes = w[0].metadata["excludes"].as_str().unwrap();
        assert!(excludes.starts_with("the claim door's UI"), "{excludes}");
        assert!(excludes.contains("car dec3136a"), "{excludes}");
        assert!(
            w[1].metadata["test"]
                .as_str()
                .unwrap()
                .contains("current main")
        );

        let mut taken = successor.clone();
        taken["steps"][0]["status"] = json!("completed");
        let w = successor_writes(&taken, &landed_car(), "c85941b423bc", "t").unwrap();
        assert_eq!(w.len(), 1, "a taken scope is not written twice");

        let cars = vec![
            json!({"id": "s2", "metadata": {"supersedes": "dec3136a-0000-4000-8000-000000000001"}}),
        ];
        assert_eq!(
            successor_of(&cars, "dec3136a-0000-4000-8000-000000000001").map(|c| &c["id"]),
            Some(&json!("s2"))
        );
        assert_eq!(successor_of(&cars, "other"), None);
    }

    /// Car bfa6f7be's shape on 2026-11-20: closed merged, answering the
    /// same item as the reverted car, with no `supersedes` yet.
    fn landed_successor() -> Value {
        json!({
            "id": "bfa6f7be-0000-4000-8000-000000000004",
            "kind": "ship-a-change",
            "status": "closed",
            "metadata": {
                "branch": "fix/pin-postgres-by-purpose",
                "outcome": "merged",
                "backlog_item": "11111111-0000-4000-8000-000000000002",
            },
        })
    }

    /// THE NAMED SUCCESSOR (backlog 7f4694eb): open or landed, answering
    /// the reverted car's item by either key — and owed the back-edge
    /// only once.
    #[test]
    fn a_named_successor_answers_the_same_item_and_is_owed_supersedes_once() {
        let car = landed_car();
        let e = reverted_successor(&car, &landed_successor()).unwrap();
        assert!(e.contains("car bfa6f7be"), "{e}");
        assert!(e.contains("landed"), "{e}");
        assert!(e.contains("as its backlog_item"), "{e}");

        let mut also = landed_successor();
        also["metadata"]["backlog_item"] = json!("33333333-0000-4000-8000-000000000005");
        also["metadata"]["also_answers"] = json!(["11111111-0000-4000-8000-000000000002"]);
        assert!(
            reverted_successor(&car, &also)
                .unwrap()
                .contains("as its also_answers")
        );

        let mut live = landed_successor();
        live["status"] = json!("open");
        live["metadata"]["outcome"] = Value::Null;
        assert!(
            reverted_successor(&car, &live)
                .unwrap()
                .contains("rides as car bfa6f7be, which has not landed")
        );

        assert_eq!(
            supersedes_patch(car["id"].as_str().unwrap(), &landed_successor()),
            Some(json!({"supersedes": "dec3136a-0000-4000-8000-000000000001"}))
        );
        let mut linked = landed_successor();
        linked["metadata"]["supersedes"] = car["id"].clone();
        assert_eq!(supersedes_patch(car["id"].as_str().unwrap(), &linked), None);
        assert!(reverted_successor(&car, &linked).is_ok(), "a re-run passes");
    }

    /// WHERE THE INTENT STANDS (review b279aa0c): a successor is said to
    /// have LANDED only on a record of its merge — closed `merged`, or the
    /// conductor's `merged` marker on a car still open awaiting proof. A
    /// car that is merely open carries the intent; it has not landed it.
    #[test]
    fn a_successor_landed_only_when_its_merge_is_on_the_record() {
        assert_eq!(
            successor_standing(&landed_successor()),
            "landed as car bfa6f7be (closed merged)"
        );

        let mut merged_open = landed_successor();
        merged_open["status"] = json!("open");
        merged_open["metadata"]["outcome"] = Value::Null;
        merged_open["metadata"]["merged"] = json!("true");
        merged_open["metadata"]["merge_ref"] = json!("eab71518e5aa");
        assert_eq!(
            successor_standing(&merged_open),
            "landed as car bfa6f7be (merged as eab71518e5aa, not yet closed)"
        );

        let mut fresh = landed_successor();
        fresh["status"] = json!("open");
        fresh["metadata"]["outcome"] = Value::Null;
        assert_eq!(
            successor_standing(&fresh),
            "rides as car bfa6f7be, which has not landed"
        );
        // A marker that is not "true" is no record of a merge.
        fresh["metadata"]["merged"] = json!("false");
        assert!(successor_standing(&fresh).starts_with("rides as"));
    }

    /// Each successor that is not one is refused, naming why: another
    /// item, a spent car, a car superseding a different one, itself,
    /// and not a car at all.
    #[test]
    fn a_successor_that_carries_nothing_of_the_car_is_refused_naming_why() {
        let car = landed_car();
        let mut other = landed_successor();
        other["metadata"]["backlog_item"] = json!("33333333-0000-4000-8000-000000000005");
        let e = reverted_successor(&car, &other).unwrap_err();
        assert!(e.contains("does not answer item 11111111"), "{e}");

        let mut spent = landed_successor();
        spent["metadata"]["outcome"] = json!("abandoned");
        let e = reverted_successor(&car, &spent).unwrap_err();
        assert!(e.contains("closed without landing"), "{e}");

        let mut taken = landed_successor();
        taken["metadata"]["supersedes"] = json!("44444444-0000-4000-8000-000000000006");
        let e = reverted_successor(&car, &taken).unwrap_err();
        assert!(e.contains("already supersedes car 44444444"), "{e}");

        assert!(
            reverted_successor(&car, &car)
                .unwrap_err()
                .contains("its own successor")
        );

        let mut item = landed_successor();
        item["kind"] = json!("backlog-item");
        assert!(
            reverted_successor(&car, &item)
                .unwrap_err()
                .contains("not a car")
        );

        let mut no_item = landed_car();
        no_item["metadata"]["backlog_item"] = Value::Null;
        assert!(
            reverted_successor(&no_item, &landed_successor())
                .unwrap_err()
                .contains("names no `backlog_item`")
        );
    }

    /// `--reverted-by <car>` reads the car's merge — only once it landed.
    #[test]
    fn a_revert_car_names_its_merge_only_once_it_landed() {
        let mut revert = landed_successor();
        revert["metadata"]["merge_ref"] = json!("f325cf542ee0");
        assert_eq!(revert_car_merge(&revert), Ok("f325cf542ee0"));
        let mut riding = revert.clone();
        riding["status"] = json!("open");
        let e = revert_car_merge(&riding).unwrap_err();
        assert!(e.contains("has reverted nothing"), "{e}");
        let mut unrecorded = revert.clone();
        unrecorded["metadata"]["merge_ref"] = Value::Null;
        assert!(
            revert_car_merge(&unrecorded)
                .unwrap_err()
                .contains("no `merge_ref`")
        );
    }

    /// The revert is named on the job once — and both keys when its car
    /// was named.
    #[test]
    fn the_revert_is_named_on_the_job_once() {
        let sha = "f325cf542ee0f325cf542ee0f325cf542ee0f325";
        let p = reverted_by_patch(&landed_car(), sha, Some("6c272983-x")).unwrap();
        assert_eq!(
            p,
            json!({"reverted_by": sha, "reverted_by_car": "6c272983-x"})
        );
        let mut done = landed_car();
        done["metadata"]["reverted_by"] = json!(sha);
        done["metadata"]["reverted_by_car"] = json!("6c272983-x");
        assert_eq!(reverted_by_patch(&done, sha, Some("6c272983-x")), None);
        assert_eq!(reverted_by_patch(&done, sha, None), None);
        assert_eq!(
            reverted_by_patch(&landed_car(), sha, None),
            Some(json!({"reverted_by": sha}))
        );
    }
}
