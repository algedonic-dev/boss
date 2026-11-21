//! `jobs.auto-park` — file a car when a gate-run goes GREEN carrying a
//! park intent, so a gate-green branch never strands unparked.
//!
//! THE ACTOR HALF OF AUTO-PARK. `boss gate --park-*` stamps the park
//! prose onto the gate-run (`park_*` metadata; the input half). When the
//! gate goes green the `record-verdict` step completes as
//! `gate-verdict`, and a rule on `step.done.gate-verdict` fires this
//! handler, which reads that intent + the verbatim receipt and files the
//! ship-a-change car — exactly what a human does with `boss park`, at
//! computer speed and while the base is still current (a stranded green
//! decays: gated yesterday, unmergeable today — 2026-09-01).
//!
//! FILES THROUGH `boss_jobs::car`, the shared builder `boss park` uses,
//! so the receipt-copy contract cannot drift (CLAUDE.md §9a). The receipt
//! is copied VERBATIM — never rebuilt — which is the bug `boss park` was
//! created to kill.
//!
//! A NO-OP, NOT AN ERROR, when this is not an auto-park: the verdict is
//! not green, or no `--park-*` intent was stamped (a plain manual gate).
//! The rule's `when` filters most of those, but the handler re-checks so
//! it is correct on its own terms.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::car::{self, Receipt};

use super::common::{
    Judged, StepEvent, api_client, dispatcher_actor_header, get_json, owner_for_filing,
    rows_or_refuse, write_json, write_json_expecting,
};

pub struct JobsAutoPark {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    /// Who the car this handler files is owned by — the platform owner
    /// through the port (backlog 3c23662d), resolved once per filing by
    /// `common::owner_for_filing`; never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsAutoPark {
    pub fn new(
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// Every open car, paged on `total` — see [`open_cars_url`] for why
    /// it does not narrow by subject. `full` keeps each step's metadata,
    /// which the gate hand-back reads its gate-run off.
    async fn open_cars(&self, full: bool, rule: &str) -> Result<Vec<Value>, HandlerError> {
        list_open_cars(&self.client, self.base(), full, rule).await
    }

    /// A GATE THAT DID NOT GO GREEN HANDS ITS CAR'S GATE STEP BACK
    /// (backlog 68a8726e, design 689d1f7f car 1). `boss gate` moved the
    /// branch's car into its `gate` step when it launched; a failed,
    /// lost, refused or withdrawn verdict ends that hold, so the step
    /// goes back to READY with the verdict recorded beside it
    /// (`car::gate_return`) — the car stands waiting for its next gate,
    /// not in one that has ended. Only the car whose gate step THIS
    /// gate-run holds is moved; a redelivery finds the step no longer
    /// held and writes nothing.
    ///
    /// Ownership is judged AT EACH WRITE, not only on the read (Codex
    /// review of car 0d04ee3e, R3): a gate launched on the branch after
    /// the read restamps the step to its own gate-run, and both writes
    /// then land on nothing (`car::gate_return`'s expectations).
    async fn hand_gate_back(
        &self,
        gate_run: &str,
        verdict: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let cars = self.open_cars(true, rule).await?;
        let Some(held) = car::car_in_gate_run(&cars, gate_run) else {
            return Ok(());
        };
        let car_id = held.get("id").and_then(Value::as_str).unwrap_or("?");
        for w in car::gate_return(held, gate_run, verdict) {
            if self.send_judged(&w, rule).await? == Judged::Moved {
                tracing::info!(rule = %rule, car = %car_id, gate_run = %gate_run,
                    verdict = %verdict, "the car's gate step changed hands after this \
                    verdict's read — a newer gate-run holds it, so nothing is handed back");
                return Ok(());
            }
        }
        tracing::info!(rule = %rule, car = %car_id, gate_run = %gate_run, verdict = %verdict,
            "the gate ended {verdict}: the car's gate step is handed back and waits for its \
             next gate");
        Ok(())
    }

    /// A NON-GREEN VERDICT FOLLOWS ITS EDGE (design 689d1f7f car 2,
    /// backlog a3a367e4). A gate-run `boss gate` admitted as the child of
    /// a car's gate step names that step (`boss_jobs::fulfils`), so its
    /// verdict reads the one car instead of every open car. A gate-run
    /// with no edge — every one launched before this car deployed — is
    /// handed back by the scan, as before. So is one whose edge cannot be
    /// read: a hand-back is judged on the holder alone and can only put
    /// the step back to READY, so the scan is the safe reader there.
    ///
    /// THE OUTCOME DECIDES, NOT THE CLOSE. Nothing on this path completes
    /// the step: `car::edge_return` is `car::gate_return`'s judged pair,
    /// and the edge is not `parent_step_id`, so the subjob rule that
    /// completes a parent on ANY close never sees this gate-run.
    async fn return_gate(
        &self,
        gate_run: &str,
        verdict: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let run = get_json(
            &self.client,
            &format!("{}/api/jobs/{gate_run}", self.base()),
            rule,
        )
        .await?;
        let edge = match boss_jobs::fulfils::of(run.get("metadata").unwrap_or(&Value::Null)) {
            Ok(Some(edge)) => edge,
            Ok(None) => return self.hand_gate_back(gate_run, verdict, rule).await,
            Err(why) => {
                tracing::warn!(rule = %rule, gate_run = %gate_run,
                    "{why} — handing the gate back by the scan, which is judged on the holder");
                return self.hand_gate_back(gate_run, verdict, rule).await;
            }
        };
        let held = get_json(
            &self.client,
            &format!("{}/api/jobs/{}", self.base(), edge.job),
            rule,
        )
        .await?;
        let writes = match car::edge_return(&held, &edge, gate_run, verdict) {
            Ok(writes) => writes,
            Err(why) => {
                tracing::warn!(rule = %rule, gate_run = %gate_run, verdict = %verdict,
                    "the gate ended {verdict} and its edge names no gate to hand back: {why}");
                return Ok(());
            }
        };
        if writes.is_empty() {
            tracing::info!(rule = %rule, car = %edge.job, gate_run = %gate_run,
                "the gate ended {verdict}, and its car's gate step is no longer held by it — \
                 nothing is handed back");
            return Ok(());
        }
        for w in writes {
            if self.send_judged(&w, rule).await? == Judged::Moved {
                tracing::info!(rule = %rule, car = %edge.job, gate_run = %gate_run,
                    "the car's gate step changed hands after this verdict's read — nothing is \
                     handed back");
                return Ok(());
            }
        }
        tracing::info!(rule = %rule, car = %edge.job, gate_run = %gate_run, verdict = %verdict,
            "the gate ended {verdict}: the gate step its edge names is handed back and waits \
             for its next gate");
        Ok(())
    }

    /// One [`car::JudgedWrite`], sent with its expectation.
    async fn send_judged(&self, w: &car::JudgedWrite, rule: &str) -> Result<Judged, HandlerError> {
        let method = match w.method {
            "PUT" => reqwest::Method::PUT,
            _ => reqwest::Method::PATCH,
        };
        write_json_expecting(
            &self.client,
            method,
            &format!("{}{}", self.base(), w.path),
            &w.body,
            &w.expect,
            rule,
        )
        .await
    }

    /// The writes that finish a car's steps on a green, in order — the
    /// `gate` step's two ALWAYS carrying `gate_expect`
    /// (`car::green_binding`, Codex review of car 0d04ee3e R2): the
    /// holder and head when the car was bound, the binding's ABSENCE when
    /// it was not, so a launch that stamped or restamped the step after
    /// the binding was judged makes them land on nothing.
    /// [`Judged::Moved`] when that happened; the steps before the gate
    /// were written. A green that took the step over from a lost run
    /// renames its holder in the same merge (`car::green_gate_writes`,
    /// backlog 84e46932).
    async fn finish_steps(
        &self,
        car_id: &str,
        writes: Vec<car::StepWrite>,
        gate_expect: &serde_json::Map<String, Value>,
        gate_run: &str,
        rule: &str,
    ) -> Result<Judged, HandlerError> {
        for w in writes {
            if w.title == car::GATE {
                for jw in car::green_gate_writes(car_id, &w, gate_expect, gate_run) {
                    if self.send_judged(&jw, rule).await? == Judged::Moved {
                        return Ok(Judged::Moved);
                    }
                }
                continue;
            }
            let (merge_path, status_path) = (w.merge_path(car_id), w.status_path(car_id));
            let sends = [
                (reqwest::Method::PATCH, merge_path, w.metadata),
                (reqwest::Method::PUT, status_path, w.status_body),
            ];
            for (method, path, body) in sends {
                write_json(
                    &self.client,
                    method,
                    &format!("{}{path}", self.base()),
                    &body,
                    rule,
                )
                .await?;
            }
        }
        Ok(Judged::Written)
    }

    /// A green that may not finish the car it found — another gate-run
    /// holds its gate step — files nothing and says so ON THE GATE-RUN,
    /// the way a landed or boarded branch's green does: the verdict
    /// names why it moved no car, so nobody re-derives it.
    async fn green_held_elsewhere(
        &self,
        gate_run: &str,
        why: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        tracing::warn!(rule = %rule, gate_run = %gate_run, "{why}");
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{}/api/jobs/{gate_run}/metadata", self.base()),
            &held_elsewhere_skip(why),
            rule,
        )
        .await
    }

    /// Put the gate's `--hold` onto `car`'s review step ([`hold_write`]),
    /// or, for an unheld gate, carry a live review hold to the refreshed
    /// head (backlog 6b12a70f) or do nothing. A car that cannot carry the hold
    /// is a PERMANENT refusal — redelivering cannot give it a review
    /// step — and a failed write propagates, so either way the caller
    /// stops before anything makes the car boardable.
    ///
    /// A write that carries a hold's head was decided on this handler's
    /// read of the car, and it is sent with the hold and head it read
    /// (backlog cd1f68c6). A review step that moved in between — a
    /// release, a person's hold, a red-train hold — refuses it and keeps
    /// what it holds: nothing is written, and the refusal is retryable,
    /// so a redelivery reads the car again and judges the hold it now
    /// carries.
    async fn hold_car(
        &self,
        car: &Value,
        inputs: &AutoParkInputs,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let Some((path, body, judged)) =
            hold_write(car, inputs).map_err(HandlerError::Permanent)?
        else {
            return Ok(());
        };
        let url = format!("{}{path}", self.base());
        match judged {
            None => write_json(&self.client, reqwest::Method::PATCH, &url, &body, rule).await?,
            Some(judged) => {
                let landed = write_json_expecting(
                    &self.client,
                    reqwest::Method::PATCH,
                    &url,
                    &body,
                    &judged,
                    rule,
                )
                .await?;
                if landed == Judged::Moved {
                    let car_id = car.get("id").and_then(Value::as_str).unwrap_or_default();
                    return Err(HandlerError::Downstream(format!(
                        "car {car_id}'s review step changed between this refresh's read and its \
                         hold write — its hold or hold_sha is no longer the one judged \
                         ({judged:?}), so nothing was written and the car was not refreshed; a \
                         redelivery judges the hold it carries now (backlog cd1f68c6)"
                    )));
                }
            }
        }
        match inputs.hold.as_deref() {
            Some(hold) => tracing::info!(rule = %rule, branch = %inputs.branch, hold = %hold,
                "the gate carried --hold: the car stands at the dock held until boss release"),
            None => tracing::info!(rule = %rule, branch = %inputs.branch,
                head = %inputs.receipt.head,
                "the car's review hold now names the refreshed head: a review of it releases \
                 the car (backlog 6b12a70f)"),
        }
        Ok(())
    }

    /// BEST-EFFORT: state the linked item's ROUTE, because parking this
    /// car is what decided it.
    ///
    /// `--park-backlog-item <id>` says this car is that item's build.
    /// The item's `build` step only OPENS once its triage records
    /// `disposition = build`, so until now a car parked against an
    /// un-triaged item linked to a step nothing could advance: the
    /// change shipped, landed and was proven while the item still read
    /// as undecided work, and the arrival rule found nothing to
    /// complete (backlog ca76d8f9, measured twice in an hour on
    /// 2026-09-09). The DECISION is `car::triage_on_park`, shared with
    /// `boss park` so the two park paths cannot disagree about it, and
    /// idempotent — a triage a person already completed answers `None`.
    ///
    /// NOTHING HERE MAY FAIL THE PARK. By the time this runs the car is
    /// filed and its steps are complete; a routing write that cannot
    /// reach the system of record is worth a warning, not a redelivery
    /// that would re-do the whole park. CLAUDE.md records a fallible
    /// write inside a reconcile loop freezing ALL landings — so every
    /// failure here is logged and swallowed.
    async fn triage_linked_item(&self, item_id: &str, car_id: &str, branch: &str, rule: &str) {
        // A stored edge may be a >= 8-char prefix — seven cars have one
        // — and `GET /api/jobs/{prefix}` is a permanent 400. Same test
        // the arrival rule applies to the same edge, one definition
        // (CLAUDE.md §9a).
        if let Some(why) = super::jobs_complete_linked_step::unusable_link(item_id) {
            tracing::warn!(rule = %rule, car = %car_id,
                "park left the linked item un-routed — {why}");
            return;
        }
        let item = match get_json(
            &self.client,
            &format!("{}/api/jobs/{}", self.base(), item_id),
            rule,
        )
        .await
        {
            Ok(item) => item,
            Err(e) => {
                tracing::warn!(rule = %rule, car = %car_id, item = %item_id,
                    "park could not read the linked item to route it: {e}");
                return;
            }
        };
        let Some(write) = car::triage_on_park(&item, car_id, branch) else {
            return;
        };
        // The route through the step merge door, THEN a status-only PUT —
        // a PUT carrying metadata replaces the step's stored keys
        // wholesale, so the old read-then-PUT dropped anything written
        // between the two (backlog e39a9d2a). Merge first: the step's
        // required-at-done fields are validated on the flip.
        let merged = write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{}{}", self.base(), write.merge_path(item_id)),
            &write.metadata,
            rule,
        )
        .await;
        let routed = match merged {
            Ok(()) => {
                write_json(
                    &self.client,
                    reqwest::Method::PUT,
                    &format!("{}{}", self.base(), write.status_path(item_id)),
                    &write.status_body,
                    rule,
                )
                .await
            }
            Err(e) => Err(e),
        };
        match routed {
            Ok(()) => tracing::info!(rule = %rule, car = %car_id, item = %item_id,
                "routed the linked item to `build`: the car that names it IS its build"),
            Err(e) => tracing::warn!(rule = %rule, car = %car_id, item = %item_id,
                "park could not route the linked item to `build`: {e}"),
        }
    }
}

/// The inputs a green-with-intent gate-run yields for filing its car.
#[derive(Debug, PartialEq, Eq)]
struct AutoParkInputs {
    branch: String,
    summary: String,
    excludes: String,
    test: String,
    verified: String,
    backlog_item: Option<String>,
    /// The item provenance that does NOT authorise a close: the item
    /// this car is one piece of (`car::PARTIAL_ITEM`), or the reason it
    /// names none (`car::NO_ITEM_REASON`). Copied onto the car verbatim
    /// and read by nothing downstream — which is the point, because the
    /// arrival rule follows `backlog_item` alone.
    item_provenance: serde_json::Map<String, Value>,
    /// The car this one must land BEHIND (`--park-after`), copied onto
    /// the car as the declared `car::BOARDS_AFTER` job edge. The
    /// conductor's boarding filter reads it and refuses to board this car
    /// until the named one has landed (d3320278).
    boards_after: Option<String>,
    delivery_channel: Option<String>,
    receipt: Receipt,
    /// The car's proof intent, copied VERBATIM from the gate-run's
    /// `park_probe` / `park_expect` / `park_proof_event` onto the car's
    /// `proof_*` keys (28ac45ab) — the same copy-don't-rebuild rule the
    /// receipt lives by. Empty for a car whose builder recorded none.
    proof: serde_json::Map<String, Value>,
    /// THE FLAKE THIS GREEN PROVED, if it re-gated a red at the same
    /// head: `flake_of` + `flaky_checks` (`boss_jobs::flake`), carried
    /// onto the car beside its receipt so the record of the car says
    /// its first red was not its own. Empty for a plain green — absent,
    /// never nulled, so a refresh does not strip an earlier stamp.
    flake: serde_json::Map<String, Value>,
    /// THE RUN THAT BUILT THIS CAR (design c87fb59b car 3, backlog
    /// cb78818d): the gate-run's `agent_run` — stamped by `boss gate`
    /// from BOSS_AGENT_RUN — copied onto the car under the same key so
    /// `agent-run-lands-on-car-merged` can land the run on the car's
    /// arrival, the way `agent-run-lands-on-gate-green` lands it on the
    /// gate. Empty for a hand-launched gate: absent, never nulled, like
    /// `flake`. Measured on car 2's own loose end: the landing rule keyed
    /// on the gate-run only, so a car that arrived through a re-gate or
    /// a rerail left its run at `building`.
    agent_run: serde_json::Map<String, Value>,
    /// THE TIERS THIS CAR TOUCHES (ba429e7f, design 01c3cc3f): the
    /// gate-run's `software_tiers` + `software_tier`, stamped by `boss
    /// gate` from the same diff as `delivery_channel` and copied onto
    /// the car verbatim (`car::tier_stamps`) so the arrival can
    /// aggregate a train's tiers without a checkout. Empty when the
    /// gate could not classify the diff: absent, never nulled.
    tiers: serde_json::Map<String, Value>,
    /// THE WAIT THE BUILDER DECLARED AT PARK (backlog e9b164a1 piece 3):
    /// the gate-run's `park_waits_on` object, as stamped. Not a ready
    /// patch like the maps above, because a car that already exists may
    /// carry fields the gate did not state — an `owner` added by hand —
    /// and [`waits_on_patch`] merges into those rather than over them.
    waits_on: Option<Value>,
    /// THE HOLD THE GATE CARRIED BESIDE ITS PARK INTENT (backlog
    /// 486dde37): the gate-run's `hold`, read by the one definition of
    /// the marker (`stranded::hold_reason`). `Some` = the car is filed
    /// already held — see [`hold_write`].
    hold: Option<String>,
    /// Whether that hold is a REVIEW hold, bound to the head it judged:
    /// false only when the gate-run says its hold is an unread diff
    /// alone (`car::HOLD_UNBOUND`, backlog b7b02024 review F1).
    hold_bound: bool,
}

/// A review-step merge write: its path, its body, and the review-step
/// keys it was judged on, if it was judged on any ([`hold_write`]).
type HoldWrite = (String, Value, Option<serde_json::Map<String, Value>>);

/// PURE: the review-step merge write that puts a gate's `--hold` onto
/// its car — `(path, body, judged)` — or `None` when the gate carried no hold.
/// An unheld green over a car already held for a review writes that
/// hold's new head instead (`car::hold_follows_head`, backlog 6b12a70f).
///
/// THE REVIEW STEP, BECAUSE THAT IS WHERE A HOLD IS READ. `boss hold`
/// writes `hold: <reason>` there; the conductor's `parked_ready`, the
/// loading-dock station row and `boss orient` all read it there, through
/// `stranded::hold_reason`. So the car a held gate files is held by the
/// same brake an operator would have applied, and `boss release` takes
/// it off — no second mechanism.
///
/// An open review only (pending on a car being filed, ready on one at
/// the dock): the step API refuses a write to a finished step, and a
/// car past review has boarded — holding it is no longer possible. That
/// case REFUSES rather than parking the car as though it were held,
/// because a hold that silently did not land is a trust-boundary car
/// boarding unreviewed.
///
/// The third element is what the write was JUDGED on, when its body
/// depends on the review step this read: every write whose `hold_sha`
/// `car::hold_follows_head` decided carries `car::hold_as_judged`, and
/// the merge door lands it only on a step that still reads so (backlog
/// cd1f68c6). A gate's own bound hold names its own head and needs none.
fn hold_write(car: &Value, inputs: &AutoParkInputs) -> Result<Option<HoldWrite>, String> {
    let car_id = car.get("id").and_then(Value::as_str).unwrap_or_default();
    let open = car::find_step(car, car::REVIEW_SLUG, car::REVIEW).filter(|s| {
        !matches!(
            s.get("status").and_then(Value::as_str),
            Some("completed" | "skipped")
        )
    });
    let review = open
        .and_then(|s| s.get("id").and_then(Value::as_str))
        .filter(|_| !car_id.is_empty());
    let review_md = open.and_then(|s| s.get("metadata")).unwrap_or(&Value::Null);
    let head = inputs.receipt.head.trim();
    // A REFRESH MOVES A LIVE REVIEW HOLD'S HEAD (backlog 6b12a70f): a
    // green with no hold of its own, refreshing a car already held for a
    // review, carries that hold to the head the green vouches for, so the
    // review of that head releases it. `car::hold_follows_head` decides
    // it for `boss rerail --finish` too.
    let Some(reason) = inputs.hold.as_deref() else {
        return Ok(review.and_then(|step_id| {
            car::hold_follows_head(review_md, head).map(|body| {
                (
                    format!("/api/jobs/{car_id}/steps/{step_id}/metadata"),
                    body,
                    Some(car::hold_as_judged(review_md)),
                )
            })
        }));
    };
    let Some(step_id) = review else {
        return Err(format!(
            "the gate carried --hold ({reason}) but car {car_id} has no open review step to \
             carry it — refusing to park the car unheld; nothing further was written to it"
        ));
    };
    // THE HEAD THE HOLD WAS JUDGED AT rides in the same write (design
    // 7cedfa29 D2, backlog b7b02024): the receipt's head, which is the
    // head the gate's hold was stamped on. The conductor then boards
    // this car only on a release recorded at its current head, so a
    // release given before a repair is no release of the repair.
    // An unbound hold over a released car deletes the `hold_sha` that
    // release lifted, so it is not read as judged there (backlog
    // 6c463c66, `car::hold_body`).
    let mut body = car::hold_body(review_md, reason);
    let mut judged = None;
    if inputs.hold_bound && !head.is_empty() {
        body[car::HOLD_SHA] = json!(head);
    } else if let Some(moved) = car::hold_follows_head(review_md, head) {
        // An unbound hold written over a live review hold keeps it a
        // review hold (`car::hold_body`), so its head follows the refresh
        // too (backlog 6b12a70f) — but only over the review hold this
        // read: a release landing first leaves a hold this write must
        // not bind to the new head (backlog cd1f68c6).
        body[car::HOLD_SHA] = moved[car::HOLD_SHA].clone();
        judged = Some(car::hold_as_judged(review_md));
    }
    Ok(Some((
        format!("/api/jobs/{car_id}/steps/{step_id}/metadata"),
        body,
        judged,
    )))
}

/// PURE: the `waits_on` a park writes onto `existing` (the car's own
/// job, or `None` for a car this green is about to file) — the gate's
/// declaration MERGED into the car's (`car::merge_waits_on`), or nothing
/// when the gate stated none. Absent, never nulled, like `tiers`: a
/// re-gate with no `--park-waits-on` leaves a recorded wait alone.
fn waits_on_patch(
    existing: Option<&Value>,
    inputs: &AutoParkInputs,
) -> serde_json::Map<String, Value> {
    let recorded = existing.and_then(|c| c.pointer(&format!("/metadata/{}", car::WAITS_ON)));
    inputs
        .waits_on
        .as_ref()
        .and_then(|stated| car::merge_waits_on(recorded, stated))
        .map(|w| serde_json::Map::from_iter([(car::WAITS_ON.to_string(), w)]))
        .unwrap_or_default()
}

/// PURE: what a green verdict stamps on a gate-run that re-gated a red
/// at the same head — `None` for a red (a persistent red is the
/// branch's) and for a green that re-gated nothing. Backlog 36cc4913:
/// the relationship is the fact that turns a flake into a count, and
/// the verdict is the only thing that settles it. Decided by the
/// record, never by the check's name.
fn flake_stamp(gate_run: &Value, verdict_meta: &serde_json::Map<String, Value>) -> Option<Value> {
    if verdict_meta.get("verdict").and_then(Value::as_str) != Some("green") {
        return None;
    }
    let md = gate_run.get("metadata")?;
    // The head the green VERIFIED, from its receipt: a relation decided
    // at a different requested head is not this green's flake (90f8e64c).
    let gated = gated_head(verdict_meta);
    if let Some((requested, verified)) = boss_jobs::flake::regate_head_moved(md, gated.as_deref()) {
        tracing::warn!(
            "not a flake: the re-gate's relation was decided at {requested} but this green \
             verified {verified} — the branch moved between the launch and the clone"
        );
    }
    boss_jobs::flake::flake_patch(md, gated.as_deref())
}

/// The head a verdict's receipt vouches for, if it names one. The
/// receipt rides the verdict step verbatim as a JSON string.
fn gated_head(verdict_meta: &serde_json::Map<String, Value>) -> Option<String> {
    let raw = verdict_meta.get("receipt").and_then(Value::as_str)?;
    let parsed: Value = serde_json::from_str(raw).ok()?;
    parsed
        .get("head")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// PURE: the metadata patch a re-gate writes onto the car already at
/// the dock — everything the LATEST green stamped, so the car carries
/// what it is now, not what its first build was.
///
/// The receipt and the cleared skip (`car::regate_patch`); the current
/// proof intent, since the builder may have written or fixed the probe
/// on the re-gate; the car's current account of itself (c30e6276) —
/// the scope/build/gate steps are frozen at the first park's words, so
/// a rebuilt car's summary/excludes/test/verified ride the job under
/// `regate_*` beside the receipt that superseded the first one; and the
/// item provenance, since a re-gate may be the run where the builder
/// first said which item this car is a piece of.
fn refresh_patch(car: &Value, inputs: &AutoParkInputs, note: &str) -> Value {
    let mut patch = car::regate_patch(&inputs.receipt, note, inputs.delivery_channel.as_deref());
    if let Some(m) = patch.as_object_mut() {
        m.extend(inputs.proof.clone());
        m.extend(inputs.flake.clone());
        m.extend(inputs.agent_run.clone());
        m.extend(inputs.tiers.clone());
        m.extend(waits_on_patch(Some(car), inputs));
        m.extend(car::regate_prose(
            &inputs.summary,
            &inputs.excludes,
            &inputs.test,
            &inputs.verified,
        ));
        m.extend(inputs.item_provenance.clone());
        // The latest explicit answer also governs a parked car: retaining
        // its old closing edge would bury the remaining work (d8a73b73).
        m.extend(clear_stale_item_answer(car, inputs));
        m.extend(supersede(car, inputs));
    }
    patch
}

/// PURE: why a green verdict must NOT park — the shared predicate
/// `car::declared_no_estate` (backlog 3e63662c), asked of the receipt
/// the verdict step carries. It is one of three readers that turn a
/// green into a car or refresh one; `boss park` and `boss rerail
/// --finish` ask the same predicate through `park::receipt_for`. Here a
/// refusal is a permanent error whose dead-letter lands on the
/// gate-run, never a quiet no-op. A verdict with NO receipt is `None`:
/// `auto_park_inputs` files nothing without one.
fn declared_no_estate(verdict_meta: &serde_json::Map<String, Value>) -> Option<String> {
    let raw = verdict_meta.get("receipt").and_then(Value::as_str)?;
    car::declared_no_estate(raw)
}

/// PURE: read the auto-park inputs from a gate-run packet and its
/// verdict step's metadata. `None` = not an auto-park (verdict not green,
/// or no park intent stamped) — a no-op the caller returns `Ok(())` for.
/// Split out so the gating and extraction are unit-tested without HTTP.
fn auto_park_inputs(
    gate_run: &Value,
    verdict_meta: &serde_json::Map<String, Value>,
) -> Option<AutoParkInputs> {
    // Green only. A failed or lost gate does not park.
    if verdict_meta.get("verdict").and_then(Value::as_str) != Some("green") {
        return None;
    }
    let md = gate_run.get("metadata").and_then(Value::as_object)?;
    // No `park_summary` = a manual gate (no `--park-*` intent). Do not
    // auto-park; the branch is gated but its author did not ask for it.
    let summary = md
        .get(car::PARK_SUMMARY)
        .and_then(Value::as_str)?
        .to_string();
    let branch = md.get("branch").and_then(Value::as_str)?.to_string();
    let field = |k: &str| {
        md.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    // The receipt rides on the verdict step, VERBATIM as a JSON string;
    // head and mode are read out of it for the car builder without
    // rebuilding the string.
    let raw = verdict_meta
        .get("receipt")
        .and_then(Value::as_str)?
        .to_string();
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let receipt = Receipt {
        raw,
        head: parsed
            .get("head")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        mode: parsed
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    };
    Some(AutoParkInputs {
        branch,
        summary,
        excludes: field(car::PARK_EXCLUDES),
        test: field(car::PARK_TEST),
        verified: field(car::PARK_VERIFIED),
        delivery_channel: md
            .get("delivery_channel")
            .and_then(Value::as_str)
            .map(str::to_string),
        backlog_item: md
            .get(car::PARK_BACKLOG_ITEM)
            .and_then(Value::as_str)
            .map(str::to_string),
        // The two answers that are NOT the closing edge, copied under
        // the core keys nothing downstream follows. Read here so the
        // gate's `park_partial_item` / `park_no_item` cannot silently go
        // missing between the gate-run and the car.
        //
        // Every OTHER item the car answers (a994f533) rides in the same
        // map, because it is the same kind of fact — what the car says
        // about items beyond its one closing edge — and the map is what
        // all three park paths already carry, so no path can drop it.
        item_provenance: {
            let mut p = car::item_provenance(
                md.get(car::PARK_PARTIAL_ITEM).and_then(Value::as_str),
                md.get(car::PARK_NO_ITEM).and_then(Value::as_str),
            );
            p.extend(car::also_answers(md.get(car::PARK_ALSO_ANSWERS)));
            p
        },
        // The ordering edge the gate's `--park-after` stamped. Blank is
        // no edge: the ref check reads `''` as "no claim to check", so a
        // blank written onto the car would be a constraint the dock
        // cannot see — `boss gate` refuses one, and this is the second
        // door on the same rule.
        boards_after: md
            .get(car::PARK_BOARDS_AFTER)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        receipt,
        proof: car::proof_intent_of_park(md),
        // The verdict is green by the guard at the top of this function.
        flake: flake_stamp(gate_run, verdict_meta)
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
        agent_run: agent_run_of(md),
        tiers: car::tier_stamps(md),
        waits_on: md
            .get(car::PARK_WAITS_ON)
            .filter(|w| w.is_object())
            .cloned(),
        hold: gate_run
            .get("metadata")
            .and_then(boss_jobs::stranded::hold_reason),
        hold_bound: gate_run
            .pointer(&format!("/metadata/{}", car::HOLD_UNBOUND))
            .and_then(Value::as_bool)
            != Some(true),
    })
}

/// PURE: why this green may NOT leave a car at the dock — the shared
/// predicate `car::unproven_park` (backlog 031a3ca9), asked of the proof
/// the gate-run's park stated and the car it lands on (`None` for a car
/// this green would file). `boss gate` refuses such a park at launch;
/// this is the second door, for an intent stamped before that refusal
/// existed or by anything other than the verb.
fn unproven_refusal(existing: Option<&Value>, inputs: &AutoParkInputs) -> Option<String> {
    car::unproven_park(existing, &inputs.proof).map(|why| {
        format!(
            "auto-park: no car parked for {}: {why}\n\nRe-gate it with its park flags and \
             a probe; this green stays on its gate-run.",
            inputs.branch
        )
    })
}

/// PURE: the `agent_run` stamp a gate-run carries, as the map the three
/// park paths merge — one entry, or none. A blank value is none: `boss
/// gate` refuses to stamp one, and this is the second door on the rule.
fn agent_run_of(md: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    md.get("agent_run")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|id| {
            let mut out = serde_json::Map::new();
            out.insert("agent_run".to_string(), json!(id));
            out
        })
        .unwrap_or_default()
}

/// PURE: the record written on the gate-run INSTEAD of a car, when a car
/// for this branch already carried it to main. `None` = nothing landed;
/// file as usual.
///
/// A LANDED BRANCH GETS NO TWIN. Measured 2026-09-08 (backlog 610537b2):
/// a re-gate of fix/boot-never-refuses-over-an-unviable-workflow — already
/// on main via train #259 — reused gate-run 57f49a80, which still carried
/// the park intent stamped before the landing. On green this handler
/// filed twin car dfb98d07 for content already merged; the conductor
/// boarded it as train #261, whose CI went red on an empty diff, and the
/// train was cancelled and the twin abandoned by hand. Stale intent on a
/// reused packet is real; the defence is here, at the one place a car is
/// filed. The dispatcher runs without git, so "landed" is read from the
/// system of record (`boss_jobs::car::landed_car_for`, the same test
/// `boss gate` runs), not from ancestry.
fn landed_skip(cars: &[Value], branch: &str) -> Option<Value> {
    let car = car::landed_car_for(cars, branch)?;
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    let md = |k: &str| {
        car.pointer(&format!("/metadata/{k}"))
            .and_then(Value::as_str)
            .unwrap_or("?")
    };
    let train = md("train");
    Some(json!({
        "park_skipped": "landed",
        "park_skipped_note": format!(
            "no car filed: car {} already carried {branch} to main as {} (train {}) — \
             this green re-gated landed content, and a twin would board an empty diff \
             (backlog 610537b2)",
            &id[..8.min(id.len())],
            md("merge_ref"),
            &train[..8.min(train.len())],
        ),
    }))
}

/// PURE: the record written on the gate-run INSTEAD of a car, when this
/// branch's car is already ABOARD a train. `None` = nothing is riding;
/// carry on.
///
/// A BRANCH IN TRANSIT GETS NO TWIN EITHER. Measured 2026-09-08
/// (backlog 02165b1d): car d08a6418 for
/// feat/a-human-only-step-refuses-an-agent boarded train #274 at
/// 20:30:54, and at 20:31:20 a duplicate gate-run went green and this
/// handler filed car ad54e95c for the same branch — the builder session
/// that owned the branch had ended, but its detached retry loop kept
/// re-gating. The twin sat on the loading dock while the real car rode,
/// and was abandoned by hand. `parked_car_for` answered `None` — truly,
/// the first car was no longer parked — and "no parked car" was read as
/// "no car". The landed guard did not cover it: the branch was green and
/// in transit, not yet merged. So the question asked here is the wider
/// one, `open_car_for`.
fn boarded_skip(cars: &[Value], branch: &str) -> Option<Value> {
    let car = car::open_car_for(cars, branch).filter(|c| car::is_boarded(c))?;
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    let train = car
        .pointer("/metadata/train")
        .and_then(Value::as_str)
        .unwrap_or("?");
    Some(json!({
        "park_skipped": "boarded",
        "park_skipped_note": format!(
            "no car filed: car {} for {branch} is already aboard train {} — a second car \
             for a branch in transit sits on the dock until someone abandons it by hand \
             (2026-09-08, cars d08a6418/ad54e95c; backlog 02165b1d)",
            &id[..8.min(id.len())],
            &train[..8.min(train.len())],
        ),
    }))
}

/// PURE: the record written on the gate-run when its green may not
/// finish the branch's car because another gate-run holds that car's
/// gate step (`car::green_binding`).
fn held_elsewhere_skip(why: &str) -> Value {
    json!({
        "park_skipped": "gate-held-elsewhere",
        "park_skipped_note": format!(
            "no car finished: {why} (Codex review of car 0d04ee3e, R2)"
        ),
    })
}

/// PURE: the record written on the gate-run when its green FOLLOWED its
/// edge and may not finish what the edge names — the car is not open or
/// not on the branch this gate-run gated, the step is not its gate, or
/// this gate-run does not hold that gate. Nothing
/// is filed instead: one car per branch is the invariant the edge exists
/// for, and the verdict that holds the step moves the car.
fn edge_refused_skip(edge: &boss_jobs::fulfils::Fulfils, why: &str) -> Value {
    json!({
        "park_skipped": "edge-refused",
        "park_skipped_note": format!(
            "no car finished: this green followed its edge to car {} and {why} (backlog \
             a3a367e4)",
            &edge.job[..8.min(edge.job.len())],
        ),
    })
}

/// PURE: what this green does about `branch` — the whole decision, in
/// one place, so "does it ever file a second car" is a unit test rather
/// than a live incident.
///
/// `open` is the branch's open cars; `all` adds the closed ones (a
/// landing is usually closed). The order is the order the failures were
/// measured in: landed first (610537b2 — the operator needs the merge
/// ref), then boarded (02165b1d), then the parked refresh (d052afad),
/// and only a branch with no live car at all files one.
#[derive(Debug)]
enum ParkAction<'a> {
    /// File nothing; record this on the gate-run instead.
    Skip(Value),
    /// Refresh the car already at the dock with the fresh receipt.
    Refresh(&'a Value),
    /// FINISH the car the builder opened at build start: complete the
    /// steps their open left open, and carry the gate's own facts onto
    /// the packet that already exists.
    Adopt(&'a Value),
    /// No live car for this branch: file one.
    File,
}

fn park_action<'a>(open: &'a [Value], all: &[Value], branch: &str, edged: bool) -> ParkAction<'a> {
    // A GATE-RUN THAT CARRIES AN EDGE IS FINISHED BY ITS VERDICT, never
    // by this scan (design 689d1f7f car 2, backlog a3a367e4). The handler
    // follows a readable edge before it reaches here, so the one edged
    // gate-run that does is one whose edge cannot be read — and the
    // answer is still Skip, first, before any car is looked at: the edge
    // path and an Adopt must never both finish one gate step, and a
    // half-written edge must not quietly become a scan that refreshes or
    // files a second car for the branch.
    if edged {
        return ParkAction::Skip(json!({
            "park_skipped": "finished-by-its-verdict",
            "park_skipped_note": "no car filed, refreshed or adopted: this gate-run carries a \
                `fulfils` edge, so its verdict is followed to the step it names and never \
                matched to a car by branch — and this edge could not be read (backlog a3a367e4)",
        }));
    }
    if let Some(patch) = landed_skip(all, branch) {
        return ParkAction::Skip(patch);
    }
    if let Some(patch) = boarded_skip(open, branch) {
        return ParkAction::Skip(patch);
    }
    if let Some(car) = car::parked_car_for(open, branch) {
        return ParkAction::Refresh(car);
    }
    // A GREEN FINISHES THE CAR THE BUILDER OPENED (backlog be025b44).
    // Asked last, so every guard measured before it answers first — and
    // asked at all, because with `boss car open` in the builders' hands
    // this is the COMMON case, not an edge: the branch's packet exists
    // from the build's first minute and its `gate` step has not
    // reported, so none of the questions above matches it. `File` here
    // would be the fourth twin of the same shape.
    match car::building_car_for(open, branch) {
        Some(car) => ParkAction::Adopt(car),
        None => ParkAction::File,
    }
}

/// PURE: the job-metadata patch an ADOPT owes the car it finishes.
///
/// A car filed at green gets the gate's facts in its `car_body`; a car
/// opened at build start was filed before those facts existed, so they
/// arrive here instead — the proof intent copied verbatim from the
/// gate-run's `park_*` keys (`boss prove --from-car` and the arrival rule
/// read it back), the item provenance that does NOT authorise a close
/// (`car::PARTIAL_ITEM` / `car::NO_ITEM_REASON`), and the delivery
/// channel classified from the diff.
///
/// THE PROVENANCE IS CARRIED BY ALL THREE PARK PATHS OR BY NONE. The
/// file path merges it into `car_body_with_proof` and the refresh path
/// into `regate_patch`; an adopt that did not would silently drop a
/// `--park-partial-item` answer for every car a builder opened — which
/// is to say, for every car, once they do. The textual merge of these
/// two changes did not catch that, because the two edits are in
/// different functions.
///
/// NOTHING IS NULLED and nothing the open recorded is restated, with the
/// one deliberate exception `supersede` below carries. The job-metadata
/// door DELETES a key set to null, so a channel the open recorded must
/// not be stripped by a green that could not classify one.
///
/// THE CLOSING EDGE IS NOT ADDED HERE, and that is deliberate —
/// [`adopt_edge_patch`] says why.
fn adopt_patch(car: &Value, inputs: &AutoParkInputs) -> Value {
    let mut patch = serde_json::Map::new();
    patch.extend(inputs.proof.clone());
    patch.extend(inputs.item_provenance.clone());
    patch.extend(inputs.flake.clone());
    patch.extend(inputs.agent_run.clone());
    patch.extend(inputs.tiers.clone());
    patch.extend(waits_on_patch(Some(car), inputs));
    patch.extend(clear_stale_item_answer(car, inputs));
    if let Some(dc) = inputs.delivery_channel.as_deref() {
        patch.insert("delivery_channel".to_string(), json!(dc));
    }
    // A CAR ADMITTED BY DISPATCH WAS SCOPED BEFORE ITS BUILDER WROTE THE
    // BOUNDARY (backlog 68a8726e): its scope step carries the item's
    // title and a "not yet stated" excludes, and a completed step cannot
    // be rewritten. The builder's own summary and excludes arrive here,
    // with the green, and ride the car's metadata — so the boundary is on
    // the car, not only on the gate-run.
    if car
        .pointer(&format!("/metadata/{}", car::ADMITTED_BY))
        .is_some()
    {
        patch.insert("summary".to_string(), json!(inputs.summary));
        patch.insert("excludes".to_string(), json!(inputs.excludes));
    }
    // LAST, so the id `supersede` preserves wins over a clear: the two
    // overlap only on a hand-edited packet, and keeping the id is the
    // whole point of superseding rather than dropping.
    for (k, v) in supersede(car, inputs) {
        patch.insert(k, v);
    }
    Value::Object(patch)
}

/// PURE: the non-closing item answers an OPEN recorded that the gate has
/// since answered DIFFERENTLY — nulled, which is how the job-metadata
/// door deletes a key.
///
/// ONE CAR, ONE ITEM ANSWER. Since `boss car open` gained the same three
/// answers the gate has (backlog 90d291bb), a car can arrive at its green
/// already carrying `PARTIAL_ITEM` or `NO_ITEM_REASON`. When the gate
/// restates the same answer — the common case, and the point of stating
/// it at build start — nothing here fires and the recorded value stands.
/// When the gate answers something else, its answer is written and the
/// open's must go, or the car reads "this car answers no item" beside the
/// edge that closes one.
///
/// THIS IS RESIDUE, NOT THE HARM `supersede` PREVENTS. The dangerous
/// direction is an open that named the CLOSING edge against a gate that
/// said do not close it, because that buries work; `supersede` owns that
/// case and records it in prose. This one only leaves a packet saying two
/// things, which costs the next reader a re-derivation rather than a
/// buried piece of work — so it is a quiet delete plus a warn, not a
/// narrative.
///
/// NOTHING IS DELETED UNLESS THE GATE REALLY STATED AN ANSWER. The three
/// `--park-*` answers are mutually exclusive and one is required, so a
/// gate-run with no answer at all can only come from a hand-edited
/// packet — and there the recorded answer is the only answer there is. A
/// destructive write that fires when it should not is worse than the
/// residue it removes.
fn clear_stale_item_answer(car: &Value, inputs: &AutoParkInputs) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    if inputs.backlog_item.is_none() && !states_a_non_closing_answer(inputs) {
        return out;
    }
    for key in [car::PARTIAL_ITEM, car::NO_ITEM_REASON] {
        let recorded = car
            .pointer(&format!("/metadata/{key}"))
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty());
        if recorded && !inputs.item_provenance.contains_key(key) {
            out.insert(key.to_string(), Value::Null);
        }
    }
    out
}

/// Did the gate state one of the two NON-closing item answers? Asked by
/// key, not by the provenance map's emptiness, because the map also
/// carries `also_answers` (a994f533) — which rides beside an answer and
/// is never one itself, so a hand-edited gate-run carrying only that
/// list must not read as an answer that clears or supersedes anything.
fn states_a_non_closing_answer(inputs: &AutoParkInputs) -> bool {
    [car::PARTIAL_ITEM, car::NO_ITEM_REASON]
        .iter()
        .any(|k| inputs.item_provenance.contains_key(*k))
}

/// PURE: the one case where an adopt OVERWRITES what the open recorded —
/// the builder's two item answers contradict each other.
///
/// HOW IT ARISES. `boss car open --backlog-item X` writes the CLOSING
/// edge on the car at build start, ref-checked at the POST. The gate is
/// then given `--park-partial-item X` or `--park-no-item <reason>`,
/// which both say the opposite: do not close that item. Nothing rejects
/// the pair, because the two statements are made by two verbs an hour
/// apart, and the arrival rule follows `metadata.backlog_item` alone —
/// so left as-is the car would close an item whose builder had just
/// said it has work outstanding. That is the exact harm e1325456 was
/// filed for (item cf0f5e2d was three pieces; closing it would have
/// buried piece 3, which was waiting on an operator's yes).
///
/// WHY THE GATE'S ANSWER WINS. It is the later statement, made with the
/// finished diff in hand, and the two errors are not symmetrical: an
/// item left open when it could have closed costs one human close,
/// while an item closed with work outstanding buries the work. So the
/// closing edge is deleted (present-and-null — the metadata door's
/// delete) and the same id is preserved under `PARTIAL_ITEM`, the key
/// the landed design put there for exactly this provenance-without-a-
/// close purpose. Nothing is lost: the car still names the item, under
/// a name no rule follows.
///
/// NOT SILENT. `item_answer_superseded` records the contradiction in
/// prose on the car, and the handler logs it — a reader who wonders why
/// the edge they typed at open is gone finds the answer on the packet,
/// rather than re-deriving it.
///
/// EMPTY whenever there is no contradiction: a gate that names the
/// closing edge (the three `--park-*` answers are mutually exclusive, so
/// provenance is then empty anyway), a gate with no provenance, or an
/// opened car that carries no edge to supersede.
fn supersede(car: &Value, inputs: &AutoParkInputs) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    // A gate that DOES name the closing edge is not a contradiction —
    // and must never lose it. Belt and braces: the flags are mutually
    // exclusive at the gate, so this can only fire on a packet edited by
    // hand, where the closing edge is the safer thing to keep.
    if inputs.backlog_item.is_some() || !states_a_non_closing_answer(inputs) {
        return out;
    }
    let Some(open_edge) = car
        .pointer("/metadata/backlog_item")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    else {
        return out;
    };
    let answer = if let Some(reason) = inputs
        .item_provenance
        .get(car::NO_ITEM_REASON)
        .and_then(Value::as_str)
    {
        format!("--park-no-item \"{reason}\"")
    } else {
        let partial = inputs
            .item_provenance
            .get(car::PARTIAL_ITEM)
            .and_then(Value::as_str)
            .unwrap_or("?");
        format!("--park-partial-item {}", &partial[..8.min(partial.len())])
    };
    out.insert(car::BACKLOG_ITEM.to_string(), Value::Null);
    // Keep the id the open named, unless the gate already named one.
    if !inputs.item_provenance.contains_key(car::PARTIAL_ITEM) {
        out.insert(car::PARTIAL_ITEM.to_string(), json!(open_edge));
    }
    out.insert(
        "item_answer_superseded".to_string(),
        json!(format!(
            "the open named backlog_item {} (the edge the arrival rule closes) and the \
             gate then answered {answer}, which says the opposite. The gate's answer is \
             the later one and made with the finished diff, and closing an item with work \
             outstanding buries that work (e1325456), so the closing edge was removed and \
             the id kept under `{}` — provenance without a close. If the item really is \
             this car's whole build, re-gate with --park-backlog-item.",
            &open_edge[..8.min(open_edge.len())],
            car::PARTIAL_ITEM,
        )),
    );
    out
}

/// PURE: the backlog edge an ADOPT adds, when the gate's intent named one
/// the opened car does not already carry. `None` = nothing to write.
///
/// A SEPARATE, BEST-EFFORT WRITE, because `backlog_item` is a DECLARED
/// job edge: a DB trigger refuses an id that does not resolve, with a
/// **400**, and `write_json` maps anything but a 422 to `Downstream` —
/// which redelivers. So an unresolvable edge folded into `adopt_patch`
/// would retry the adopt forever and, worse, cost the car its proof
/// intent on every attempt. CLAUDE.md records what a fallible write
/// inside a loop does: a single observability write froze ALL landings.
/// The receipt and the steps are what a car must not lose; an edge that
/// will not resolve is worth a warning.
///
/// Skipped when the car already carries the same edge — an open that was
/// given `--backlog-item` wrote it at POST, where it was ref-checked.
fn adopt_edge_patch(car: &Value, inputs: &AutoParkInputs) -> Option<Value> {
    let item = inputs.backlog_item.as_deref()?;
    if car
        .get("metadata")
        .and_then(|m| m.get(car::BACKLOG_ITEM))
        .and_then(Value::as_str)
        == Some(item)
    {
        return None;
    }
    Some(json!({ car::BACKLOG_ITEM: item }))
}

/// PURE: the ORDERING edge the gate's `--park-after` adds, for either way
/// a car arrives — `existing` is the car an open already filed, or `None`
/// for one this green is about to create. `None` = nothing to write.
///
/// A SEPARATE, BEST-EFFORT WRITE, for exactly the reason
/// [`adopt_edge_patch`] is: `boards_after` is a DECLARED job edge, so an
/// id that will not resolve is a 400, and `write_json` maps anything but
/// a 422 to `Downstream`, which REDELIVERS. Folded into the car's POST it
/// would cost the car itself on every attempt; folded into `adopt_patch`
/// it would cost the car its receipt and its proof. A car that exists
/// with a missing ordering edge is repairable by one metadata PATCH; a
/// car that does not exist is not. So the edge goes last and a failure is
/// a warning naming the repair.
///
/// It is also why the gate refuses a BLANK `--park-after` rather than
/// letting it through: `''` resolves as "no claim to check", which would
/// succeed here and leave a constraint the dock cannot see.
fn boards_after_patch(existing: Option<&Value>, inputs: &AutoParkInputs) -> Option<Value> {
    let after = inputs.boards_after.as_deref()?;
    // Already carried (an open given `--after`, or a re-gate restating
    // the same edge) — the value was ref-checked when it was written.
    if existing
        .and_then(|c| c.pointer(&format!("/metadata/{}", car::BOARDS_AFTER)))
        .and_then(Value::as_str)
        == Some(after)
    {
        return None;
    }
    Some(json!({ car::BOARDS_AFTER: after }))
}

/// How many cars one page of the open-car read asks for. The dock and
/// the trains together hold 22 open cars today, so one page answers —
/// but a page is still a page, and the read below walks `total`
/// (a-limit-is-not-a-filter).
/// Every open car, paged on `total` — see [`open_cars_url`] for why it
/// does not narrow by subject. `full` keeps each step's metadata. Shared
/// with `jobs.end_unlaunched_car`, which reads the same set.
pub(crate) async fn list_open_cars(
    client: &boss_core::machine_token::Client,
    base: &str,
    full: bool,
    rule: &str,
) -> Result<Vec<Value>, HandlerError> {
    let mut cars: Vec<Value> = Vec::new();
    let mut offset = 0usize;
    loop {
        let mut url = open_cars_url(base, offset);
        if full {
            url.push_str("&full=true");
        }
        let page = get_json(client, &url, rule).await?;
        // A page with no `data` array is NO ANSWER. Read as zero
        // cars it sent `park_action` to `File` — a twin of the car
        // at the dock — so this read fails toward a write, and it
        // refuses rather than guess (backlog 37fc5837).
        let rows: Vec<Value> = rows_or_refuse(&page, "the open-car read (GET /api/jobs)")
            .map_err(HandlerError::Downstream)?;
        let got = rows.len();
        cars.extend(rows);
        let total = page
            .get("total")
            .and_then(Value::as_u64)
            .unwrap_or(cars.len() as u64) as usize;
        // Stop on a short page as well as on the count: a `total`
        // that never shrinks would otherwise spin forever.
        if got == 0 || cars.len() >= total {
            break;
        }
        offset += CARS_PAGE;
    }
    Ok(cars)
}

const CARS_PAGE: usize = 200;

/// PURE: the listing query for the open cars a park decision reads.
///
/// KEYED ON THE BRANCH, NOT THE SUBJECT — and that is the whole of the
/// third measured twin (backlog 02165b1d, 2026-09-08 21:53 UTC). This
/// read used to narrow server-side with `subject_id={branch}`, on the
/// assumption that a car's subject IS its branch. It is only its
/// FILING branch: `boss rerail` repoints `metadata.branch` at the
/// re-railed branch and leaves the subject where the car was filed, so
/// car 538775dd (subject `feat/arrival-runs-the-probe`, branch
/// `feat/arrival-runs-the-probe-rerail`) never answered the narrowed
/// query. The parked car was invisible, "no parked car" read as "no
/// car" for the third time, and the handler filed twin ef4eff3c for a
/// branch that already had one.
///
/// So the query asks the wide question and the shared
/// `metadata.branch` predicates in `boss_jobs::car` decide — the same
/// way `boss gate`'s own launch guard already reads the dock
/// (`sor_client::all_open_cars`). One question, one answer, no field that
/// can drift out from under it (CLAUDE.md §9a).
fn open_cars_url(base: &str, offset: usize) -> String {
    format!("{base}/api/jobs?kind=ship-a-change&status=open&limit={CARS_PAGE}&offset={offset}")
}

/// PURE: the car body, with the proof intent and the item provenance
/// merged into its metadata. The shared builder owns the packet shape;
/// these keys are added here rather than threaded through its signature
/// because `boss park` (the hand verb auto-park replaces) has no probe
/// to pass.
fn car_body_with_proof(inputs: &AutoParkInputs, owner: &str) -> Value {
    let mut body = car::car_body(
        &inputs.branch,
        &inputs.summary,
        inputs.backlog_item.as_deref(),
        inputs.delivery_channel.as_deref(),
        owner,
    );
    if let Some(md) = body.get_mut("metadata").and_then(Value::as_object_mut) {
        md.extend(inputs.proof.clone());
        md.extend(inputs.item_provenance.clone());
        md.extend(inputs.flake.clone());
        md.extend(inputs.agent_run.clone());
        md.extend(inputs.tiers.clone());
        md.extend(waits_on_patch(None, inputs));
    }
    body
}

/// POST a body and return the response JSON — the create needs the new
/// car's id back, which `common::post_json` (fire-and-forget) discards.
/// Same header + 422-is-permanent contract as the shared helpers.
async fn post_json_return(
    client: &boss_core::machine_token::Client,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<Value, HandlerError> {
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", super::common::sim_origin_value())
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            HandlerError::Permanent(format!("POST {url} returned {status}: {text}"))
        } else {
            HandlerError::Downstream(format!("POST {url} returned {status}: {text}"))
        });
    }
    resp.json()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url} not JSON: {e}")))
}

#[async_trait]
impl Handler for JobsAutoPark {
    fn name(&self) -> &'static str {
        "jobs.auto-park"
    }

    async fn invoke(
        &self,
        // The rule engine's Value, not serde_json's — this handler takes
        // no args (it reads everything from the event + the gate-run).
        _args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;

        // Only a green verdict parks. Any other verdict hands back the
        // gate step of the car this gate-run holds, if it holds one
        // (backlog 68a8726e) — and parks nothing.
        match ev.metadata.get("verdict").and_then(Value::as_str) {
            Some("green") => {}
            Some(verdict) => {
                return self.return_gate(ev.job_id, verdict, &ctx.rule_name).await;
            }
            None => return Ok(()),
        }

        // A green whose live comparisons were declared away is not a
        // green this pipeline may act on (see `declared_no_estate`).
        if let Some(why) = declared_no_estate(ev.metadata) {
            tracing::error!(rule = %ctx.rule_name, gate_run = %ev.job_id, "{why}");
            return Err(HandlerError::Permanent(why));
        }

        // The gate-run packet carries the branch + the `park_*` intent.
        let gate_run = get_json(
            &self.client,
            &format!("{}/api/jobs/{}", self.base(), ev.job_id),
            &ctx.rule_name,
        )
        .await?;
        // THE EDGE IS READ OFF THE RAW METADATA, before the projection
        // below narrows it to the immutable submission source: `boss gate`
        // writes it AFTER the submission is accepted, once the car reads
        // back holding the gate (design 689d1f7f car 2). It is a locator,
        // not an authority — every write it leads to is judged on the
        // holder the launch stamped on the car's step (`car::edge_green`).
        let edge = boss_jobs::fulfils::of(gate_run.get("metadata").unwrap_or(&Value::Null));
        let source = boss_jobs::gate_reservation::source_metadata(
            gate_run.get("metadata").unwrap_or(&Value::Null),
        )
        .map_err(HandlerError::Permanent)?
        .clone();
        let gate_run = {
            let mut projected = gate_run;
            projected["metadata"] = source;
            projected
        };

        // A GREEN AFTER A RED AT THE SAME HEAD IS A FLAKE, and this is
        // the one actor that reads every green verdict, so it is where
        // the record says so (backlog 36cc4913). `boss gate` stamped
        // `regate_of` at launch; the green settles it. On the GATE-RUN
        // first, before the park-intent gate below: a builder's plain
        // re-gate — no `--park-*` — proves a flake exactly as a parking
        // one does, and the count on `boss orient` reads gate-runs, not
        // cars. Best effort: a failed stamp costs the count one row, and
        // must not cost the car its filing.
        if let Some(stamp) = flake_stamp(&gate_run, ev.metadata) {
            let prior = stamp[boss_jobs::flake::FLAKE_OF]
                .as_str()
                .unwrap_or_default()
                .to_string();
            match write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!("{}/api/jobs/{}/metadata", self.base(), ev.job_id),
                &stamp,
                &ctx.rule_name,
            )
            .await
            {
                Ok(()) => tracing::info!(rule = %ctx.rule_name, gate_run = %ev.job_id,
                    flake_of = %prior, "green at the head {prior} was red on — recorded as a flake"),
                Err(e) => tracing::warn!(rule = %ctx.rule_name, gate_run = %ev.job_id,
                    "could not stamp flake_of={prior} on the gate-run: {e} — the flake count \
                     is one short; PATCH /api/jobs/{}/metadata with the stamp", ev.job_id),
            }
        }

        let Some(inputs) = auto_park_inputs(&gate_run, ev.metadata) else {
            // Green, but no park intent — a manual gate. Nothing to do.
            return Ok(());
        };

        // THE VERDICT COMPLETES THE STEP ITS GATE-RUN FULFILS (design
        // 689d1f7f car 2, backlog a3a367e4). A gate launched on a car
        // records the R8 edge to that car's gate step, so its green reads
        // the ONE car the edge names — no open-car scan, no landed or
        // boarded read, no refresh, no filing — and finishes it through
        // the same writes an Adopt makes, the gate step's judged on the
        // holder the launch stamped. A gate-run with no edge (launched
        // before this deployed, or whose edge write was lost) takes the
        // scan below, unchanged until car 3.
        let relaunch_of = gate_run
            .pointer(&format!("/metadata/{}", boss_jobs::lost_gate::RELAUNCH_OF))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
        let by_edge = match &edge {
            Ok(Some(edge)) => {
                let full = get_json(
                    &self.client,
                    &format!("{}/api/jobs/{}", self.base(), edge.job),
                    &ctx.rule_name,
                )
                .await?;
                match car::edge_green(
                    &full,
                    edge,
                    &inputs.branch,
                    ev.job_id,
                    relaunch_of.as_deref(),
                    &inputs.receipt.head,
                ) {
                    Ok(car::EdgeGreen::Finish(expect)) => Some((full, expect)),
                    Ok(car::EdgeGreen::Finished) => {
                        tracing::info!(rule = %ctx.rule_name, gate_run = %ev.job_id,
                            car = %edge.job, "the gate step this green fulfils is already \
                            finished — nothing to do");
                        return Ok(());
                    }
                    Err(why) => {
                        tracing::warn!(rule = %ctx.rule_name, gate_run = %ev.job_id, "{why}");
                        write_json(
                            &self.client,
                            reqwest::Method::PATCH,
                            &format!("{}/api/jobs/{}/metadata", self.base(), ev.job_id),
                            &edge_refused_skip(edge, &why),
                            &ctx.rule_name,
                        )
                        .await?;
                        return Ok(());
                    }
                }
            }
            Ok(None) | Err(_) => None,
        };

        // A RE-GATE NEVER FILES A TWIN. Measured 2026-09-05 (backlog
        // d052afad): this handler filed a car per green gate, so the dock
        // held 10 cars for 6 branches and each first car sat with a
        // receipt for a vanished head, left behind by every train. A car
        // still at the dock is refreshed instead — the fresh receipt
        // rides it as `regate_receipt` (the rerail write, one builder in
        // core) and the skip clears. A car already ABOARD a train is left
        // alone entirely (02165b1d). Only a closed or past-review car is
        // spent history; then, and only then, a new car.
        //
        // ONE READ OF THE OPEN CARS serves all three questions below.
        // Every open car, paged — see `open_cars_url` for why it does
        // not narrow by subject. A green that follows its edge reads
        // neither list: the edge already named its car.
        let cars = match by_edge {
            Some(_) => Vec::new(),
            None => self.open_cars(false, &ctx.rule_name).await?,
        };

        // A LANDED BRANCH GETS NO TWIN — checked before the parked-car
        // refresh, because a still-parked car for a landed branch IS a
        // twin and must not be kept fresh either. Closed cars are the
        // usual landing; an open car with the `merged` marker is one
        // the dispatcher has not closed yet (see `landed_skip`).
        //
        // THIS read stays narrowed by subject, and deliberately: 807
        // ship-a-change packets are closed (measured 2026-09-08), so
        // reading them all per green gate is not a trade worth making
        // for a hole the open read now covers. What it leaves: a
        // RE-RAILED car that has both landed AND been closed answers
        // neither `subject_id={branch}` nor the open read, so a stray
        // re-gate of that branch could still file one twin. The window
        // is minutes wide — the conductor stamps `merged` on the car
        // while it is still open, and `landed_car_for` reads that
        // marker off the open page above — and it has not been
        // measured. Say so rather than pay for it.
        let landed: Vec<Value> = match by_edge {
            Some(_) => Vec::new(),
            None => {
                let closed = get_json(
                    &self.client,
                    &format!(
                        "{}/api/jobs?kind=ship-a-change&status=closed&subject_id={}&limit=50",
                        self.base(),
                        inputs.branch
                    ),
                    &ctx.rule_name,
                )
                .await?;
                // The same refusal, for the same reason: read as "nothing
                // landed", an error-shaped body files a car for work
                // already on main (backlog 37fc5837).
                rows_or_refuse(&closed, "the landed-car read (GET /api/jobs)")
                    .map_err(HandlerError::Downstream)?
            }
        };
        let all: Vec<Value> = cars.iter().cloned().chain(landed).collect();
        // ONE DECISION, taken on what the SoR holds: follow the edge,
        // skip (landed, or aboard a train, or an edge that cannot be
        // read), refresh the car at the dock, finish the car a builder
        // opened, or file.
        let (parked, adopt, edge_expect) = match by_edge {
            Some((full, expect)) => (None, Some(full), Some(expect)),
            None => match park_action(&cars, &all, &inputs.branch, edge.is_err()) {
                ParkAction::Skip(patch) => {
                    write_json(
                        &self.client,
                        reqwest::Method::PATCH,
                        &format!("{}/api/jobs/{}/metadata", self.base(), ev.job_id),
                        &patch,
                        &ctx.rule_name,
                    )
                    .await?;
                    return Ok(());
                }
                ParkAction::Refresh(car) => (Some(car), None, None),
                ParkAction::Adopt(car) => (None, Some(car.clone()), None),
                ParkAction::File => (None, None, None),
            },
        };

        // NO CAR REACHES THE DOCK OWING A PROOF NOTHING CAN SETTLE
        // (backlog 031a3ca9). Judged on the car this green lands on —
        // the edge's, the parked one, the one the builder opened — or on
        // the intent alone for a car it would file, and refused like a
        // declared-away estate: a permanent error whose dead-letter lands
        // on the gate-run, before any write to any car.
        if let Some(why) = unproven_refusal(parked.or(adopt.as_ref()), &inputs) {
            tracing::error!(rule = %ctx.rule_name, gate_run = %ev.job_id, "{why}");
            return Err(HandlerError::Permanent(why));
        }

        if let Some(parked) = parked {
            let id = parked.get("id").and_then(Value::as_str).ok_or_else(|| {
                HandlerError::Downstream("auto-park: parked car has no id".into())
            })?;
            // A held re-gate holds the car it refreshes, FIRST: the car
            // is already at the dock, so the fresh receipt must not make
            // it boardable a moment before the brake is on.
            self.hold_car(parked, &inputs, &ctx.rule_name).await?;
            let note = format!(
                "re-gated in place: green at {} (gate-run {}) — receipt machine-copied to \
                 regate_receipt by the auto-park handler; the frozen gate step stays as the \
                 original head's record",
                &inputs.receipt.head[..12.min(inputs.receipt.head.len())],
                ev.job_id
            );
            let patch = refresh_patch(parked, &inputs, &note);
            write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!("{}/api/jobs/{}/metadata", self.base(), id),
                &patch,
                &ctx.rule_name,
            )
            .await?;
            // A re-gate may also be the run where the builder first said
            // which car this one lands behind. Separate and best-effort
            // for the reason `boards_after_patch` documents: this packet
            // has just had a fresh receipt written onto it and must not
            // lose that to a redelivery over an edge.
            if let Some(edge) = boards_after_patch(Some(parked), &inputs)
                && let Err(e) = write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{}/metadata", self.base(), id),
                    &edge,
                    &ctx.rule_name,
                )
                .await
            {
                tracing::warn!(rule = %ctx.rule_name, car = %id,
                    "the re-gate's --park-after edge would not attach: {e} — this car is NOT \
                     held behind that one; PATCH /api/jobs/{id}/metadata with boards_after");
            }
            // A re-gate restates the route too: the item may still be
            // un-triaged from the first park (this is new), and a
            // second pass over an item already routed writes nothing.
            if let Some(item) = inputs.backlog_item.as_deref() {
                self.triage_linked_item(item, id, &inputs.branch, &ctx.rule_name)
                    .await;
            }
            return Ok(());
        }

        // A GREEN FINISHES ONLY THE GATE ITS RUN HOLDS (Codex review of car
        // 0d04ee3e, R2). A car `boss gate` moved into its gate names the
        // gate-run holding the step and the head it launched at; a green
        // from any other gate-run — an older one delivered after a re-gate
        // launched — finishes nothing and says so on its gate-run. Judged
        // on a FULL read of the car (the open list is slim, without step
        // metadata), BEFORE any write to it; and the gate step's own writes
        // carry the same judgement to the jobs API, so a launch landing
        // after this read makes them land on nothing. A car with no
        // binding is judged on that ABSENCE (round 2 R2): a launch that
        // binds it after this read is a change of hands too. A car this
        // green files below has never been launched on, so it is judged
        // on absence as well.
        let gate_expect = match (edge_expect, &adopt) {
            // Judged already, on the car the edge named (`car::edge_green`).
            (Some(expect), _) => expect,
            (None, Some(existing)) => {
                let id = existing
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let full = get_json(
                    &self.client,
                    &format!("{}/api/jobs/{id}", self.base()),
                    &ctx.rule_name,
                )
                .await?;
                // A machine relaunch names the lost gate-run it replaces,
                // and succeeds to the gate step that run held (backlog
                // 64d417ca, `car::green_binding`).
                match car::green_binding(
                    &full,
                    ev.job_id,
                    relaunch_of.as_deref(),
                    &inputs.receipt.head,
                ) {
                    Ok(expect) => expect,
                    Err(why) => {
                        return self
                            .green_held_elsewhere(ev.job_id, &why, &ctx.rule_name)
                            .await;
                    }
                }
            }
            (None, None) => car::unbound_gate(),
        };

        // THE CAR'S STAMPS ARE THE GREEN'S INSTANT (backlog 2b03a2df).
        // The gate step's `completed_at` is one end of the dock-queue-time
        // measurement (`review − gate`), so it is the instant the gate
        // went green — the verdict event's own timestamp — not the moment
        // this handler consumed it, which a redelivery or a replay moves.
        // A firing with no instant is refused before anything is filed.
        let now = ctx.firing_instant()?;

        // ONE CAR, TWO WAYS IN. Either the builder opened it when the
        // build started and this green FINISHES it, or no car exists and
        // this green files one. Past this point the sequence is identical:
        // read the packet, then complete the steps it has not already
        // completed — which is why the step writes are decided in core
        // (`car::finish_writes`), shared with `boss park`.
        let car_id = match &adopt {
            Some(existing) => {
                let id = existing
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        HandlerError::Downstream("auto-park: the building car has no id".into())
                    })?
                    .to_string();
                // The gate's own facts — proof intent, item provenance,
                // delivery channel — onto the packet that already exists.
                // Before the steps, so a car whose arrival rule fires off
                // the gate completion already carries its probe AND the
                // item answer that decides whether it closes anything.
                let url = format!("{}/api/jobs/{}/metadata", self.base(), id);
                let patch = adopt_patch(existing, &inputs);
                if let Some(why) = patch
                    .pointer("/item_answer_superseded")
                    .and_then(Value::as_str)
                {
                    tracing::warn!(rule = %ctx.rule_name, car = %id, "{why}");
                }
                // The quieter half of the same disagreement: the open
                // recorded a non-closing answer and the gate answered
                // otherwise, so the stale one is being removed. Said out
                // loud, because a key that vanishes without a record is a
                // re-derivation for whoever notices.
                for key in [car::PARTIAL_ITEM, car::NO_ITEM_REASON] {
                    if patch.as_object().and_then(|m| m.get(key)) == Some(&Value::Null) {
                        tracing::warn!(rule = %ctx.rule_name, car = %id,
                            "the open recorded `{key}` and the gate answered differently — \
                             the stale answer was removed so the car states one item \
                             answer, not two");
                    }
                }
                if patch.as_object().is_some_and(|m| !m.is_empty()) {
                    write_json(
                        &self.client,
                        reqwest::Method::PATCH,
                        &url,
                        &patch,
                        &ctx.rule_name,
                    )
                    .await?;
                }
                // The declared edge, separately and best-effort: an id
                // that will not resolve is a 400, which redelivers, and
                // a redelivery over an edge must not cost the car the
                // write above (see `adopt_edge_patch`).
                if let Some(edge) = adopt_edge_patch(existing, &inputs)
                    && let Err(e) = write_json(
                        &self.client,
                        reqwest::Method::PATCH,
                        &url,
                        &edge,
                        &ctx.rule_name,
                    )
                    .await
                {
                    tracing::warn!(rule = %ctx.rule_name, car = %id,
                        "the gate's backlog edge would not attach to the opened car: {e} — \
                         the car is finished; link it by hand");
                }
                tracing::info!(rule = %ctx.rule_name, car = %id, branch = %inputs.branch,
                    "finishing the car its builder opened at build start — no second car");
                id
            }
            None => {
                let owner = owner_for_filing(self.owner.as_ref(), &ctx.rule_name).await;
                let body = car_body_with_proof(&inputs, &owner);
                let created = post_json_return(
                    &self.client,
                    &format!("{}/api/jobs", self.base()),
                    &body,
                    &ctx.rule_name,
                )
                .await?;
                created
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        HandlerError::Downstream("auto-park: create returned no id".into())
                    })?
                    .to_string()
            }
        };
        let car_id = car_id.as_str();

        let car = get_json(
            &self.client,
            &format!("{}/api/jobs/{}", self.base(), car_id),
            &ctx.rule_name,
        )
        .await?;

        // THE ORDERING EDGE, BEFORE THE CAR IS PARKED. The gate step's
        // completion below is what makes `review` ready, which is what
        // puts the car on the dock — so the edge has to be on the packet
        // by now or the first board after this could ride a car whose
        // constraint had not landed yet. Best-effort and last among the
        // metadata writes: see `boards_after_patch` for why a declared
        // edge must never ride inside a write the car cannot afford to
        // lose.
        if let Some(edge) = boards_after_patch(Some(&car), &inputs)
            && let Err(e) = write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!("{}/api/jobs/{}/metadata", self.base(), car_id),
                &edge,
                &ctx.rule_name,
            )
            .await
        {
            tracing::warn!(rule = %ctx.rule_name, car = %car_id,
                after = %inputs.boards_after.as_deref().unwrap_or_default(),
                "the gate's --park-after edge would not attach: {e} — this car is NOT held \
                 behind that one and will board in the next window; PATCH \
                 /api/jobs/{car_id}/metadata with boards_after to restore it");
        }

        // THE HOLD, BEFORE THE CAR IS PARKED — for the reason the edge
        // above gives, and harder: the gate step's completion makes
        // review ready, and a car whose review is ready and unheld is
        // boardable on the next tick. Unlike the edge this write is NOT
        // best-effort: a trust-boundary car that lost its hold boards
        // unreviewed, so a failure stops here, the gate step stays open,
        // and the car stays short of the dock (backlog 486dde37).
        self.hold_car(&car, &inputs, &ctx.rule_name).await?;

        // In order (scope → build → gate): each completion re-evaluates
        // readiness so the next is ready. A step the OPEN already
        // completed is skipped — the step API refuses a metadata write to
        // a completed step, so re-sending `scope` would 409 on every car
        // a builder opened. Each is the evidence through the step merge
        // door, THEN a status-only PUT — a PUT carrying metadata replaces
        // the step's stored keys wholesale (backlog e39a9d2a).
        let writes = car::finish_writes(
            &car,
            &inputs.summary,
            &inputs.excludes,
            &inputs.test,
            &inputs.verified,
            &inputs.receipt,
            now,
        )
        .map_err(HandlerError::Downstream)?;
        if self
            .finish_steps(car_id, writes, &gate_expect, ev.job_id, &ctx.rule_name)
            .await?
            == Judged::Moved
        {
            return self
                .green_held_elsewhere(
                    ev.job_id,
                    &format!(
                        "car {}'s gate step changed hands after this green judged it — a \
                         newer gate-run holds it, and the gate writes landed on nothing",
                        &car_id[..8.min(car_id.len())]
                    ),
                    &ctx.rule_name,
                )
                .await;
        }

        // THE CAR IS THE ITEM'S BUILD — say so on the item, last, so a
        // failure here costs the routing and not the car.
        if let Some(item) = inputs.backlog_item.as_deref() {
            self.triage_linked_item(item, car_id, &inputs.branch, &ctx.rule_name)
                .await;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn post_launch_raw_drift_cannot_replace_the_original_hold_probe_or_partial_answer() {
        use axum::{
            Json, Router,
            extract::Request,
            http::{Method, StatusCode},
            response::IntoResponse,
        };
        let source = json!({"branch":"fix/x","sha":"deadbeef","park_summary":"original work",
            "park_excludes":"original exclusions","park_test":"actual original tests","park_verified":"original observations",
            "park_probe":"printf original-proof","park_expect":"original-proof","park_proof_event":"proof.original",
            "park_partial_item":"11111111-1111-4111-8111-111111111111","hold":"original required review",
            "agent_run":"22222222-2222-4222-8222-222222222222","worktree":"/scratch/original"});
        let envelope = serde_json::to_value(boss_jobs::gate_reservation::Submission {
            contract_version: 1,
            command: uuid::Uuid::new_v4(),
            submitted_by: "agent:fixture".into(),
            supervisor: "automation:train-conductor".into(),
            intent: boss_jobs::gate_reservation::LaunchIntent {
                contract_version: 1,
                packet: "gate-fixture".into(),
                namespace: "fixture".into(),
                name: "fixture-retained-job".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
                transition_generation: None,
                file_id: "fixture-file".into(),
                sha256: "a".repeat(64),
                size_bytes: 1,
            },
            source: source.as_object().unwrap().clone(),
        })
        .unwrap();
        let mut metadata = source.clone();
        metadata["gate_submission"] = envelope.clone();
        metadata["_immutable_metadata"] = json!({"gate_submission":envelope});
        metadata["gate_launch_observation"] =
            json!({"uid":"native-observed-before-drift","request_sha256":"frozen"});
        metadata["park_summary"] = json!("later raw text");
        metadata["park_probe"] = json!("printf wrong-proof");
        metadata["park_expect"] = json!("wrong-proof");
        metadata["hold"] = Value::Null;
        metadata
            .as_object_mut()
            .unwrap()
            .remove("park_partial_item");
        metadata["park_backlog_item"] = json!("33333333-3333-4333-8333-333333333333");
        let gate =
            json!({"id":"gate-fixture","kind":"gate-run","status":"closed","metadata":metadata});
        let car = json!({"id":"car-fixture","kind":"ship-a-change","status":"open","metadata":{"branch":"fix/x"},
            "steps":[{"id":"review-fixture","spec_slug":"review","status":"ready","metadata":{}}]});
        type Writes = Arc<std::sync::Mutex<Vec<(String, Value)>>>;
        let writes: Writes = Arc::default();
        let retained = writes.clone();
        let app = Router::new().fallback(move |request: Request| {
            let (gate, car, writes) = (gate.clone(), car.clone(), retained.clone());
            async move {
                let path = request.uri().path().to_string();
                let query = request.uri().query().unwrap_or_default().to_string();
                if request.method() == Method::GET {
                    return if path == "/api/jobs/gate-fixture" {
                        Json(gate).into_response()
                    } else if path == "/api/jobs" {
                        Json(if query.contains("status=closed") {
                            json!({"data":[],"total":0})
                        } else {
                            json!({"data":[car],"total":1})
                        })
                        .into_response()
                    } else {
                        Json(json!({"id":"item","kind":"backlog-item","steps":[]})).into_response()
                    };
                }
                let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
                    .await
                    .unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                writes.lock().unwrap().push((path, body));
                StatusCode::NO_CONTENT.into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let handler = JobsAutoPark::new(
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        let ctx = InvocationContext {
            rule_name: "jobs.auto-park".into(),
            triggering_event_id: "evt-original-green".into(),
            triggering_topic: "step.done.gate-verdict".into(),
            event_timestamp: None,
            event_payload: json!({"job_id":"gate-fixture","step_id":"verdict","kind":"gate-verdict","metadata":green_step_meta()}),
        };
        let result = handler.invoke(&[], &ctx).await;
        server.abort();
        result.unwrap();
        let writes = writes.lock().unwrap();
        eprintln!(
            "actual post-launch auto-park writes {}",
            serde_json::to_string(&*writes).unwrap()
        );
        let hold = writes
            .iter()
            .find(|(path, _)| path == "/api/jobs/car-fixture/steps/review-fixture/metadata")
            .map(|(_, body)| body);
        assert_eq!(
            hold.and_then(|body| body.get("hold")),
            Some(&json!("original required review"))
        );
        let patch = &writes
            .iter()
            .find(|(path, _)| path == "/api/jobs/car-fixture/metadata")
            .unwrap()
            .1;
        assert_eq!(patch["proof_probe"], source["park_probe"]);
        assert_eq!(patch["proof_expect"], source["park_expect"]);
        assert_eq!(patch[car::PARTIAL_ITEM], source["park_partial_item"]);
        assert_eq!(patch["agent_run"], source["agent_run"]);
        assert!(patch.get(car::BACKLOG_ITEM).is_none_or(Value::is_null));
        assert_eq!(patch["regate_receipt"], green_step_meta()["receipt"]);
    }

    #[test]
    fn refresh_reconciles_a_later_partial_answer_without_losing_the_receipt() {
        let parent = "46c18d11-4edf-4dfb-99ad-1d4e3a77336a";
        let car = json!({"metadata": {"backlog_item": parent}});
        let inputs = auto_park_inputs(
            &gate_run(json!({
                "park_summary": "partial repair", "park_partial_item": parent,
                "park_probe": "echo source", "park_expect": "source", "hold": "review",
            })),
            &green_step_meta(),
        )
        .unwrap();
        let patch = refresh_patch(&car, &inputs, "refreshed");
        assert_eq!(
            patch.get(car::BACKLOG_ITEM),
            Some(&Value::Null),
            "the metadata merge must delete the old closing edge"
        );
        assert_eq!(patch[car::PARTIAL_ITEM], parent);
        assert!(
            patch["item_answer_superseded"]
                .as_str()
                .unwrap()
                .contains("46c18d11")
        );
        assert_eq!(patch["regate_receipt"], inputs.receipt.raw);
        assert_eq!(patch[car::PROOF_PROBE], "echo source");
    }

    #[test]
    fn refresh_reconciles_only_an_explicit_changed_answer() {
        let car = json!({"metadata": {"backlog_item": "old-parent"}});
        for intent in [
            json!({}),
            json!({"park_backlog_item": "old-parent"}),
            json!({"park_also_answers": ["other"]}),
        ] {
            let mut gr = gate_run(intent);
            gr["metadata"]["park_summary"] = json!("repair");
            let inputs = auto_park_inputs(&gr, &green_step_meta()).unwrap();
            let patch = refresh_patch(&car, &inputs, "n");
            assert!(patch.get(car::BACKLOG_ITEM).is_none());
            assert!(patch.get("item_answer_superseded").is_none());
        }
        let inputs = auto_park_inputs(
            &gate_run(json!({
                "park_summary": "repair", "park_no_item": "no closing answer",
            })),
            &green_step_meta(),
        )
        .unwrap();
        let patch = refresh_patch(&car, &inputs, "n");
        assert_eq!(patch.get(car::BACKLOG_ITEM), Some(&Value::Null));
        assert_eq!(patch[car::PARTIAL_ITEM], "old-parent");
    }

    fn gate_run(extra_md: Value) -> Value {
        let mut md = json!({ "branch": "fix/x", "sha": "abc" });
        if let (Some(dst), Some(src)) = (md.as_object_mut(), extra_md.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        json!({ "kind": "gate-run", "metadata": md })
    }

    fn green_step_meta() -> serde_json::Map<String, Value> {
        json!({
            "verdict": "green",
            "receipt": "{\"verdict\":\"green\",\"head\":\"deadbeef\",\"mode\":\"full\",\"fails\":[]}"
        })
        .as_object()
        .unwrap()
        .clone()
    }

    /// Measured 2026-09-16 (c30e6276): a re-gate wrote the fresh
    /// receipt, note and probe and left summary/excludes/test/verified
    /// at the first park's values — the car described its first build.
    /// The refresh now carries the latest green's prose under `regate_*`.
    #[test]
    fn the_refresh_carries_the_re_gates_prose_beside_the_receipt() {
        let gr = gate_run(json!({
            "park_summary": "rebuilt: now does the other thing. detail.",
            "park_excludes": "still not that",
            "park_test": "ran it again",
            "park_verified": "the other thing is observable",
            "park_probe": "echo ok",
            "park_expect": "ok",
        }));
        let inputs = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        let patch = refresh_patch(&json!({}), &inputs, "re-gated in place");
        assert_eq!(
            patch["regate_summary"],
            "rebuilt: now does the other thing. detail."
        );
        assert_eq!(patch["regate_excludes"], "still not that");
        assert_eq!(patch["regate_test"], "ran it again");
        assert_eq!(patch["regate_verified"], "the other thing is observable");
        assert!(
            patch["regate_receipt"]
                .as_str()
                .unwrap()
                .contains("deadbeef"),
            "the receipt still rides: {patch}"
        );
        assert_eq!(patch["regate_note"], "re-gated in place");
        assert_eq!(patch["skip_reason"], Value::Null);
        assert_eq!(
            patch["proof_probe"], "echo ok",
            "the proof still rides: {patch}"
        );
        // Prose the re-gate did not state is ABSENT, never null: the
        // metadata door deletes a null key, and an earlier re-gate's
        // words must survive a later one that said nothing.
        let bare = gate_run(json!({ "park_summary": "only a summary." }));
        let inputs = auto_park_inputs(&bare, &green_step_meta()).expect("parks");
        let patch = refresh_patch(&json!({}), &inputs, "n");
        assert_eq!(patch["regate_summary"], "only a summary.");
        assert!(patch.get("regate_test").is_none(), "{patch}");
        assert!(patch.get("regate_verified").is_none(), "{patch}");
    }

    /// Backlog 36cc4913: a GREEN gate-run carrying `regate_of` (the red
    /// it re-gated at the same head) is the definition of a flake. The
    /// stamp names the prior and copies its failing checks, on the
    /// gate-run and on the car — filed fresh or refreshed at the dock.
    #[test]
    fn a_green_after_a_red_at_the_same_head_stamps_flake_of_on_the_run_and_the_car() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "regate_of": "aaaa1111-0000", "prior_failed": ["test"],
        }));
        let meta = green_step_meta();
        assert_eq!(
            flake_stamp(&gr, &meta),
            Some(json!({ "flake_of": "aaaa1111-0000", "flaky_checks": ["test"] }))
        );
        let inputs = auto_park_inputs(&gr, &meta).expect("parks");
        let body = car_body_with_proof(&inputs, "emp-owner");
        assert_eq!(body["metadata"]["flake_of"], "aaaa1111-0000");
        assert_eq!(body["metadata"]["flaky_checks"], json!(["test"]));
        let patch = refresh_patch(&json!({}), &inputs, "re-gated in place");
        assert_eq!(patch["flake_of"], "aaaa1111-0000");
        assert_eq!(patch["flaky_checks"], json!(["test"]));
    }

    /// Backlog 90f8e64c: the relation was decided at the REQUESTED head
    /// (`regate_head`), the green verifies the GATED one (its receipt's
    /// `head`). When they differ the branch moved mid-launch, and the
    /// green is not a flake of that red.
    #[test]
    fn a_green_that_verified_another_head_stamps_no_flake() {
        let moved = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "regate_of": "aaaa1111-0000", "prior_failed": ["test"], "regate_head": "cafe",
        }));
        assert_eq!(flake_stamp(&moved, &green_step_meta()), None);
        let same = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "regate_of": "aaaa1111-0000", "prior_failed": ["test"], "regate_head": "deadbeef",
        }));
        assert!(flake_stamp(&same, &green_step_meta()).is_some());
    }

    /// A red after a red is the branch's: nothing is stamped. And a
    /// green that re-gated nothing carries no flake keys at all — absent,
    /// never null, so a car's earlier stamp is not stripped.
    #[test]
    fn a_red_after_a_red_and_a_plain_green_stamp_nothing() {
        let regated = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "regate_of": "aaaa1111-0000", "prior_failed": ["test"],
        }));
        let mut red = green_step_meta();
        red.insert("verdict".into(), json!("failed"));
        assert_eq!(
            flake_stamp(&regated, &red),
            None,
            "a persistent red is the branch's"
        );
        let plain = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
        }));
        assert_eq!(flake_stamp(&plain, &green_step_meta()), None);
        let inputs = auto_park_inputs(&plain, &green_step_meta()).expect("parks");
        let body = car_body_with_proof(&inputs, "emp-owner");
        assert!(body["metadata"].get("flake_of").is_none(), "{body}");
        assert!(
            refresh_patch(&json!({}), &inputs, "n")
                .get("flake_of")
                .is_none()
        );
    }

    #[test]
    fn a_green_gate_with_full_intent_yields_a_car() {
        let gr = gate_run(json!({
            "park_summary": "does a thing. and more.",
            "park_excludes": "not that",
            "park_test": "ran it",
            "park_verified": "seen working",
            "park_backlog_item": "7c9e376d",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("green+intent parks");
        assert_eq!(got.branch, "fix/x");
        assert_eq!(got.summary, "does a thing. and more.");
        assert_eq!(got.backlog_item.as_deref(), Some("7c9e376d"));
        // Receipt copied verbatim; head/mode read out of it.
        assert_eq!(got.receipt.head, "deadbeef");
        assert_eq!(got.receipt.mode, "full");
        assert!(got.receipt.raw.contains("\"fails\":[]"));
    }

    /// THE WIDE RECEIPT PARKS THE SAME WAY.
    ///
    /// The gate runner reduced `infra/gate.sh`'s account of a run to
    /// `{verdict, head, mode, fails}` before reporting it until
    /// 2026-09-09; it now reports the whole receipt. Auto-park copies it
    /// VERBATIM onto the car and reads only `head` and `mode` out, so the
    /// widening must be a no-op here — a claim worth a test, because this
    /// handler and `boss park` are the two verbs that parse the receipt
    /// on the way to a train.
    #[test]
    fn a_wide_receipt_parks_verbatim_like_the_four_field_one() {
        const WIDE: &str = "{\"verdict\":\"green\",\"mode\":\"auto\",\"scope\":\"\",\
            \"head\":\"deadbeef\",\"dirty\":false,\"host\":\"gate-runner-abc\",\"ci\":true,\
            \"free_gb\":91,\"unverifiable\":[],\
            \"report\":{\"attempts\":4,\"waited_s\":63,\"sor_unreachable\":true},\
            \"checks\":[{\"name\":\"fmt\",\"result\":\"pass\",\"seconds\":3}]}";
        let meta = json!({ "verdict": "green", "receipt": WIDE })
            .as_object()
            .unwrap()
            .clone();
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
        }));
        let got = auto_park_inputs(&gr, &meta).expect("a wide green receipt still parks");
        assert_eq!(got.receipt.raw, WIDE, "verbatim, never rebuilt from parts");
        assert_eq!(got.receipt.head, "deadbeef");
        assert_eq!(got.receipt.mode, "auto");
    }

    /// THE CAR CARRIES ITS PROBE (28ac45ab). The gate's `park_probe` /
    /// `park_expect` land on the car as `proof_probe` / `proof_expect`,
    /// verbatim, under the keys core defines — the pair `boss prove
    /// --from-car` and the arrival rule read back.
    #[test]
    fn a_park_probe_rides_onto_the_car_verbatim() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_probe": "kubectl -n boss-dev exec deploy/boss-conductor -- boss gate --help | grep -m1 park-probe",
            "park_expect": "park-probe",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        let body = car_body_with_proof(&got, "emp-owner");
        let md = &body["metadata"];
        assert_eq!(
            md[car::PROOF_PROBE],
            "kubectl -n boss-dev exec deploy/boss-conductor -- boss gate --help | grep -m1 park-probe"
        );
        assert_eq!(md[car::PROOF_EXPECT], "park-probe");
        assert!(md.get(car::PROOF_EVENT).is_none());
        // The rest of the body is the shared builder's, untouched.
        assert_eq!(md["branch"], "fix/x");
        assert_eq!(body["kind"], "ship-a-change");
    }

    /// THE RUN THAT BUILT THE CAR RIDES ONTO IT (design c87fb59b car 3,
    /// backlog cb78818d). `boss gate` stamps `agent_run` on the gate-run
    /// when a dispatched builder exports BOSS_AGENT_RUN; car 2's landing
    /// rule followed that edge on the gate-run ONLY, and auto-park built
    /// the car from `park_*` keys and never carried it — so a car's
    /// arrival could not land the run. Now the key rides the car on all
    /// three park paths (file, refresh, adopt), absent-never-nulled like
    /// `flake_of`, and `agent-run-lands-on-car-merged` follows it.
    #[test]
    fn the_gates_agent_run_rides_onto_the_car_on_every_park_path() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "agent_run": "5b1d2c3e-0000-4000-8000-000000000001",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        let body = car_body_with_proof(&got, "emp-owner");
        assert_eq!(
            body["metadata"]["agent_run"],
            "5b1d2c3e-0000-4000-8000-000000000001"
        );
        assert_eq!(
            refresh_patch(&json!({}), &got, "n")["agent_run"],
            "5b1d2c3e-0000-4000-8000-000000000001"
        );
        let car = json!({ "metadata": { "branch": "fix/x" } });
        assert_eq!(
            adopt_patch(&car, &got)["agent_run"],
            "5b1d2c3e-0000-4000-8000-000000000001"
        );

        // A hand-launched gate names no run: nothing written, nothing
        // nulled — a blank export is the same as none.
        for md in [json!({}), json!({ "agent_run": "  " })] {
            let mut plain = json!({
                "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            });
            plain
                .as_object_mut()
                .unwrap()
                .extend(md.as_object().unwrap().clone());
            let got = auto_park_inputs(&gate_run(plain), &green_step_meta()).expect("parks");
            assert!(
                car_body_with_proof(&got, "o")["metadata"]
                    .get("agent_run")
                    .is_none()
            );
            assert!(
                refresh_patch(&json!({}), &got, "n")
                    .get("agent_run")
                    .is_none()
            );
            assert!(adopt_patch(&car, &got).get("agent_run").is_none());
        }
    }

    /// THE TIERS THE GATE READ RIDE ONTO THE CAR (ba429e7f, design
    /// 01c3cc3f). `boss gate` stamps `software_tiers` + `software_tier`
    /// beside `delivery_channel` from the same diff; the handler has no
    /// checkout to re-derive them, so they are COPIED — on all three park
    /// paths, verbatim, the empty set included — and a gate-run without
    /// them writes nothing and nulls nothing.
    #[test]
    fn the_gates_tier_stamps_ride_onto_the_car_on_every_park_path() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "software_tiers": ["core", "frontend"], "software_tier": "core",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        let car = json!({ "metadata": { "branch": "fix/x" } });
        for md in [
            car_body_with_proof(&got, "emp-owner")["metadata"].clone(),
            refresh_patch(&json!({}), &got, "n"),
            adopt_patch(&car, &got),
        ] {
            assert_eq!(md["software_tiers"], json!(["core", "frontend"]));
            assert_eq!(md["software_tier"], "core");
        }

        let plain = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
        }));
        let got = auto_park_inputs(&plain, &green_step_meta()).expect("parks");
        for md in [
            car_body_with_proof(&got, "o")["metadata"].clone(),
            refresh_patch(&json!({}), &got, "n"),
            adopt_patch(&car, &got),
        ] {
            assert!(md.get("software_tiers").is_none());
            assert!(md.get("software_tier").is_none());
        }
    }

    /// THE DECLARED WAIT RIDES ONTO THE CAR, MERGED (backlog e9b164a1
    /// piece 3). `boss gate --park-waits-on` stamps `park_waits_on`; the
    /// car must end up carrying it as `waits_on`, the key the shed reads.
    /// A filed car takes it as stated. A car that already exists — at the
    /// dock for a refresh, opened at build start for an adopt — keeps any
    /// field the gate did not state, because an `owner` added to it by
    /// hand must not be dropped by the builder's re-gate: a wait without
    /// an owner reads as ours. A gate-run with no wait writes nothing
    /// and nulls nothing.
    #[test]
    fn the_gates_declared_wait_is_merged_onto_the_car_on_every_park_path() {
        let stated = json!({"on": "a release", "seen": "true"});
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            (car::PARK_WAITS_ON): stated.clone(),
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        assert_eq!(
            car_body_with_proof(&got, "o")["metadata"][car::WAITS_ON],
            stated
        );
        let car = json!({ "metadata": {
            "branch": "fix/x",
            (car::WAITS_ON): {"on": "an older wording", "seen": null, (car::WAITS_ON_OWNER): "emp-david"},
        }});
        for md in [refresh_patch(&car, &got, "n"), adopt_patch(&car, &got)] {
            assert_eq!(
                md[car::WAITS_ON],
                json!({"on": "a release", "seen": "true", (car::WAITS_ON_OWNER): "emp-david"})
            );
        }

        let plain = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
        }));
        let got = auto_park_inputs(&plain, &green_step_meta()).expect("parks");
        for md in [
            car_body_with_proof(&got, "o")["metadata"].clone(),
            refresh_patch(&car, &got, "n"),
            adopt_patch(&car, &got),
        ] {
            assert!(md.get(car::WAITS_ON).is_none(), "{md}");
        }
    }

    /// THE ORDERING EDGE REACHES THE CAR (d3320278). `--park-after`
    /// stamps `park_boards_after` on the gate-run; the car must end up
    /// carrying `boards_after`, because that is the key the conductor's
    /// boarding filter reads and an edge that stops at the gate-run is a
    /// constraint nobody enforces.
    ///
    /// NOT in the car BODY, and that is deliberate: a declared edge whose
    /// id will not resolve is a 400, `write_json` redelivers anything but
    /// a 422, and a POST that keeps failing means NO CAR. A car filed
    /// without its edge is repairable with one PATCH; a car that was never
    /// filed is not. So it rides a separate best-effort write.
    #[test]
    fn an_ordering_edge_reaches_the_car_but_not_through_its_body() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            car::PARK_BOARDS_AFTER: "a1b2c3d4",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        assert_eq!(got.boards_after.as_deref(), Some("a1b2c3d4"));
        assert!(
            car_body_with_proof(&got, "emp-owner")["metadata"]
                .get(car::BOARDS_AFTER)
                .is_none(),
            "the edge must not ride the POST: an unresolvable id would redeliver forever \
             and the car would never exist"
        );
        let patch = boards_after_patch(None, &got).expect("a fresh car needs the edge written");
        assert_eq!(patch[car::BOARDS_AFTER], "a1b2c3d4");
    }

    /// IDEMPOTENT, AND NOTHING WRITTEN WHEN NOTHING WAS SAID. A re-gate
    /// restating the same edge writes nothing (the value was ref-checked
    /// when it was first stored), and a gate with no `--park-after` must
    /// not touch an edge a car already carries — an absent flag is not an
    /// instruction to clear.
    #[test]
    fn the_ordering_edge_is_written_once_and_never_invented() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            car::PARK_BOARDS_AFTER: "a1b2c3d4",
        }));
        let with_edge = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        let already = json!({"metadata": {car::BOARDS_AFTER: "a1b2c3d4"}});
        assert_eq!(
            boards_after_patch(Some(&already), &with_edge),
            None,
            "the same edge restated is not a write"
        );
        let moved = json!({"metadata": {car::BOARDS_AFTER: "99999999"}});
        assert!(
            boards_after_patch(Some(&moved), &with_edge).is_some(),
            "a DIFFERENT edge on the latest green supersedes the old one"
        );

        // No flag at all — the overwhelmingly common case, and the
        // regression that would matter most.
        let plain = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
        }));
        let no_edge = auto_park_inputs(&plain, &green_step_meta()).expect("parks");
        assert_eq!(no_edge.boards_after, None);
        assert_eq!(boards_after_patch(Some(&already), &no_edge), None);
        assert_eq!(boards_after_patch(None, &no_edge), None);

        // A BLANK is no edge: the ref check reads `''` as "no claim to
        // check", so writing one would file a constraint the dock cannot
        // see. `boss gate` refuses it; this is the second door.
        let blank = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            car::PARK_BOARDS_AFTER: "  ",
        }));
        let blank = auto_park_inputs(&blank, &green_step_meta()).expect("parks");
        assert_eq!(blank.boards_after, None);
    }

    /// An event-bound car records the event and no probe key; a park
    /// with NO proof intent is REFUSED (backlog 031a3ca9). This test
    /// pinned the opposite until 2026-11-15 — the plain intent parked,
    /// and the car carried no proof key at all, which is how 65 of 81
    /// landed cars owed a proof came to carry nothing that could settle
    /// it. On a car that already carries its probe (the dock's own
    /// re-gate stamps only a summary) the same bare intent is a refresh.
    #[test]
    fn an_event_bound_car_records_its_event_and_an_unprobed_park_is_refused() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_proof_event": "event-bound — needs a stalled train",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        assert_eq!(unproven_refusal(None, &got), None);
        let md = car_body_with_proof(&got, "emp-owner")["metadata"].clone();
        assert_eq!(md[car::PROOF_EVENT], "event-bound — needs a stalled train");
        assert!(md.get(car::PROOF_PROBE).is_none());

        let plain = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
        }));
        let got = auto_park_inputs(&plain, &green_step_meta()).expect("reads the intent");
        assert!(got.proof.is_empty());
        let why = unproven_refusal(None, &got).expect("no car is filed without a proof");
        assert!(why.contains("fix/x"), "the refusal names the branch: {why}");
        assert!(why.contains(car::UNPROVEN_PARK), "{why}");

        let probed_car =
            json!({"metadata": {car::PROOF_PROBE: "echo ok", car::PROOF_EXPECT: "ok"}});
        assert_eq!(
            unproven_refusal(Some(&probed_car), &got),
            None,
            "a bare re-gate of a probed car refreshes it"
        );
        let bare_car = json!({"metadata": {"branch": "fix/x"}});
        assert!(unproven_refusal(Some(&bare_car), &got).is_some());
    }

    /// THE PARTIAL EDGE RIDES ONTO THE CAR, AND THE CLOSING KEY DOES
    /// NOT (e1325456).
    ///
    /// `--park-partial-item` says this car is one piece of a
    /// several-piece item: record which item, do not authorise its
    /// close. The inertness is structural, not a second rule — the
    /// arrival rule reads `metadata.backlog_item` and nothing else, and
    /// `triage_linked_item` is called only for `inputs.backlog_item`, so
    /// a `None` there is the whole guarantee that parking writes nothing
    /// on the item either.
    #[test]
    fn a_partial_item_rides_onto_the_car_without_the_closing_edge() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_partial_item": "cf0f5e2d-0000-0000-0000-000000000000",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        assert_eq!(
            got.backlog_item, None,
            "a partial item is not the closing edge — and this None is what stops the \
             park-time triage write too"
        );
        let md = car_body_with_proof(&got, "emp-owner")["metadata"].clone();
        assert_eq!(
            md[car::PARTIAL_ITEM],
            "cf0f5e2d-0000-0000-0000-000000000000"
        );
        assert!(
            md.get("backlog_item").is_none(),
            "the key the arrival rule follows must be absent: {md}"
        );
    }

    /// AN ITEM-LESS CAR CARRIES ITS REASON. `--park-no-item` is the
    /// escape for a car that genuinely answers no item, and the reason
    /// is what distinguishes it from a forgotten edge when someone reads
    /// the car later.
    #[test]
    fn an_item_less_car_carries_the_reason_it_names_none() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_no_item": "David asked for this in conversation",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        assert_eq!(got.backlog_item, None);
        let md = car_body_with_proof(&got, "emp-owner")["metadata"].clone();
        assert_eq!(
            md[car::NO_ITEM_REASON],
            "David asked for this in conversation"
        );
        assert!(md.get(car::PARTIAL_ITEM).is_none());
    }

    /// EVERY OTHER ITEM THE CAR ANSWERS RIDES ONTO IT, BY ALL THREE PARK
    /// PATHS (a994f533, the writer half). The arrival rule already closes
    /// each id in `also_answers` (its `also_link`, landed in #586), but
    /// nothing wrote the key: `boss gate --park-also-answers` stamps
    /// `park_also_answers`, and this is the hop that carries it onto the
    /// car — the file, the refresh and the adopt alike, or a car the
    /// builder opened early would drop it (the reason the provenance is
    /// carried by all three or by none).
    #[test]
    fn every_other_item_the_car_answers_rides_onto_it_by_every_path() {
        let also = json!([
            "5994de6d-0000-0000-0000-000000000000",
            "cab50f4c-0000-0000-0000-000000000000"
        ]);
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_backlog_item": "d4698bc2-0000-0000-0000-000000000000",
            "park_also_answers": also,
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        assert_eq!(
            got.backlog_item.as_deref(),
            Some("d4698bc2-0000-0000-0000-000000000000"),
            "the primary edge is untouched"
        );
        let md = car_body_with_proof(&got, "emp-owner")["metadata"].clone();
        assert_eq!(md[car::ALSO_ANSWERS], also, "filed: {md}");
        let car = json!({ "id": "c", "metadata": { "branch": "feat/x" } });
        let refreshed = refresh_patch(&car, &got, "note");
        assert_eq!(refreshed[car::ALSO_ANSWERS], also, "refreshed: {refreshed}");
        let adopted = adopt_patch(&car, &got);
        assert_eq!(adopted[car::ALSO_ANSWERS], also, "adopted: {adopted}");

        // Stated none: the key is absent everywhere, never nulled.
        let plain = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_backlog_item": "d4698bc2-0000-0000-0000-000000000000",
        }));
        let got = auto_park_inputs(&plain, &green_step_meta()).expect("parks");
        let md = car_body_with_proof(&got, "emp-owner")["metadata"].clone();
        assert!(md.get(car::ALSO_ANSWERS).is_none(), "{md}");
        assert!(
            refresh_patch(&car, &got, "n")
                .get(car::ALSO_ANSWERS)
                .is_none()
        );
        assert!(adopt_patch(&car, &got).get(car::ALSO_ANSWERS).is_none());
    }

    /// A car that IS an item's build is unchanged, and carries neither
    /// provenance key — absent, not null, like the proof keys.
    #[test]
    fn the_closing_edge_still_rides_alone() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_backlog_item": "7c9e376d-0000-0000-0000-000000000000",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        assert_eq!(
            got.backlog_item.as_deref(),
            Some("7c9e376d-0000-0000-0000-000000000000")
        );
        assert!(got.item_provenance.is_empty());
        let md = car_body_with_proof(&got, "emp-owner")["metadata"].clone();
        assert_eq!(md["backlog_item"], "7c9e376d-0000-0000-0000-000000000000");
        for k in [car::PARTIAL_ITEM, car::NO_ITEM_REASON] {
            assert!(md.get(k).is_none(), "{k} must be absent, not null");
        }
    }

    #[test]
    fn a_non_green_verdict_is_a_no_op() {
        let gr = gate_run(json!({ "park_summary": "does a thing" }));
        let mut meta = green_step_meta();
        meta.insert("verdict".into(), json!("failed"));
        assert!(auto_park_inputs(&gr, &meta).is_none());
    }

    /// THE BACKSTOP (the adversarial review of c9439bd1, backlog
    /// 3e63662c). A receipt carrying `estate` was written by a gate run
    /// that declared it had no estate, so its live comparisons were never
    /// made. Only the public mirror may declare that, and the mirror
    /// parks nothing, so such a green reaching this handler is a leak:
    /// it is REFUSED by name, never filed as a car and never a quiet
    /// no-op. A receipt without the key is untouched.
    #[test]
    fn a_green_whose_receipt_declares_no_estate_is_refused() {
        let mut meta = green_step_meta();
        meta.insert(
            "receipt".into(),
            json!(
                "{\"verdict\":\"green\",\"head\":\"deadbeef\",\"mode\":\"auto\",\
                 \"estate\":{\"declared\":\"none\",\"live_not_run\":[\"the-live-rules-are-the-authored-rules\"]},\
                 \"checks\":[]}"
            ),
        );
        let why =
            declared_no_estate(&meta).expect("a green whose receipt carries `estate` is refused");
        assert!(
            why.contains("none") && why.contains("the-live-rules-are-the-authored-rules"),
            "the refusal names the declared word and the live checks that did not run: {why}"
        );
        assert_eq!(
            declared_no_estate(&green_step_meta()),
            None,
            "a receipt with no `estate` is every cluster gate's, and parks as before"
        );
        // FAIL CLOSED (the re-review of cc7273c0): a receipt that does
        // not parse cannot say whether it declared, so it is refused.
        let mut garbage = green_step_meta();
        garbage.insert("receipt".into(), json!("not json at all"));
        assert!(
            declared_no_estate(&garbage).is_some(),
            "an unreadable receipt is refused, not waved through"
        );
    }

    #[test]
    fn a_green_gate_with_no_park_intent_is_a_no_op() {
        // A plain `boss gate` (manual) stamps no `park_*` keys.
        let gr = gate_run(json!({}));
        assert!(auto_park_inputs(&gr, &green_step_meta()).is_none());
    }

    /// THE TWIN (610537b2). A merged car for the branch means this green
    /// re-gated landed content: no car, and the gate-run says why.
    #[test]
    fn a_landed_branch_records_a_skip_instead_of_a_twin() {
        let landed = json!({
            "id": "670087f4-0000-0000-0000-000000000000",
            "status": "closed",
            "metadata": {
                "branch": "fix/x", "merged": "true", "merge_ref": "b641f3adcf47",
                "outcome": "merged", "train": "3a476b50-7a3a-409f-a9bc-59266e44f331"
            }
        });
        let patch = landed_skip(&[landed], "fix/x").expect("a landed branch is skipped");
        assert_eq!(patch["park_skipped"], "landed");
        let note = patch["park_skipped_note"].as_str().unwrap();
        assert!(note.contains("670087f4"), "names the car: {note}");
        assert!(note.contains("b641f3adcf47"), "names the merge: {note}");
        assert!(note.contains("3a476b50"), "names the train: {note}");
    }

    /// An abandoned twin or a parked car is not a landing — the branch
    /// still needs its car (a parked one is refreshed, not duplicated).
    #[test]
    fn spent_or_parked_cars_do_not_block_the_park() {
        let abandoned = json!({
            "id": "dfb98d07", "status": "closed",
            "metadata": { "branch": "fix/x", "outcome": "abandoned" }
        });
        let parked = json!({
            "id": "p", "status": "open",
            "metadata": { "branch": "fix/x" },
            "steps": [{"spec_slug": "review", "title": car::REVIEW, "status": "ready"}]
        });
        assert!(landed_skip(&[abandoned, parked], "fix/x").is_none());
        assert!(landed_skip(&[], "fix/x").is_none());
    }

    #[test]
    fn a_backlog_item_is_optional_but_the_receipt_is_not() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v"
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks without a backlog item");
        assert_eq!(got.backlog_item, None);

        // No receipt on the verdict step → cannot file a car (the whole
        // point of a car is the receipt), so it is a no-op rather than a
        // car with an empty receipt.
        let mut no_receipt = green_step_meta();
        no_receipt.remove("receipt");
        assert!(auto_park_inputs(&gr, &no_receipt).is_none());
    }
}

#[cfg(test)]
mod twin_tests {
    use super::*;

    const BRANCH: &str = "feat/a-human-only-step-refuses-an-agent";

    /// Car d08a6418 at 20:31:20 UTC on 2026-09-08: boarded train #274
    /// twenty-six seconds earlier, review still waiting.
    fn boarded() -> Value {
        json!({
            "id": "d08a6418-e7af-484b-82bf-ab043229bfd6",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": {
                "branch": BRANCH,
                "boarded_head": "cd0c4f7bdadf2306a589c922f240a7ff963f70a7",
                "train": "d72ecdb9-c5a1-4d16-8a00-d934fd565002"
            },
            "steps": [
                {"spec_slug": "gate", "title": car::GATE, "status": "completed"},
                {"spec_slug": "review", "title": car::REVIEW, "status": "ready"},
            ]
        })
    }

    /// Car ad54e95c — the twin this handler filed for the same branch,
    /// as it looked on the dock before it was abandoned by hand.
    fn parked() -> Value {
        json!({
            "id": "ad54e95c-b48a-4a4c-87f8-7aa8e4e19eff",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": BRANCH },
            "steps": [
                {"spec_slug": "gate", "title": car::GATE, "status": "completed"},
                {"spec_slug": "review", "title": car::REVIEW, "status": "ready"},
            ]
        })
    }

    /// THE MEASURED CASE (02165b1d). A car already aboard a train means
    /// the branch has its car: nothing is filed, and the gate-run says
    /// which car and which train.
    #[test]
    fn a_boarded_car_is_never_twinned() {
        let open = vec![boarded()];
        match park_action(&open, &open, BRANCH, false) {
            ParkAction::Skip(patch) => {
                assert_eq!(patch["park_skipped"], "boarded");
                let note = patch["park_skipped_note"].as_str().unwrap();
                assert!(note.contains("d08a6418"), "names the car: {note}");
                assert!(note.contains("d72ecdb9"), "names the train: {note}");
                assert!(note.contains(BRANCH), "names the branch: {note}");
            }
            other => panic!("a boarded branch must file nothing: {other:?}"),
        }
    }

    /// A car still at the dock is REFRESHED — one car, a fresh receipt
    /// on it. The behaviour that already held, pinned: whatever else
    /// changes, this branch never ends with two cars.
    #[test]
    fn a_parked_car_is_refreshed_never_duplicated() {
        let open = vec![parked()];
        match park_action(&open, &open, BRANCH, false) {
            ParkAction::Refresh(car) => {
                assert_eq!(car["id"], "ad54e95c-b48a-4a4c-87f8-7aa8e4e19eff")
            }
            other => panic!("a parked car is refreshed, not twinned: {other:?}"),
        }
        // And with the twin already there beside the boarded car — the
        // 20:31:20 state — the answer is still never `File`.
        let both = vec![parked(), boarded()];
        assert!(
            !matches!(park_action(&both, &both, BRANCH, false), ParkAction::File),
            "a branch with any live car never files a second one"
        );
    }

    #[test]
    fn a_branch_with_no_car_files_one() {
        assert!(matches!(
            park_action(&[], &[], BRANCH, false),
            ParkAction::File
        ));
        let elsewhere = vec![boarded()];
        assert!(matches!(
            park_action(&elsewhere, &elsewhere, "feat/other", false),
            ParkAction::File
        ));
    }

    /// The landed skip (610537b2) is asked FIRST: a branch already on
    /// main is reported as landed, not as boarded, because the operator
    /// needs the merge ref, not the train that carried it.
    #[test]
    fn a_landed_branch_is_still_reported_as_landed() {
        let landed = json!({
            "id": "670087f4-0000-0000-0000-000000000000",
            "status": "open",
            "metadata": {
                "branch": BRANCH, "merged": "true", "merge_ref": "75e4d3c59387",
                "train": "d72ecdb9-c5a1-4d16-8a00-d934fd565002"
            },
            "steps": [
                {"spec_slug": "review", "title": car::REVIEW, "status": "ready"},
            ]
        });
        let all = vec![landed];
        match park_action(&all, &all, BRANCH, false) {
            ParkAction::Skip(patch) => {
                assert_eq!(patch["park_skipped"], "landed");
                assert!(
                    patch["park_skipped_note"]
                        .as_str()
                        .unwrap()
                        .contains("75e4d3c59387")
                );
            }
            other => panic!("a landed branch records the landing: {other:?}"),
        }
    }
}

/// THE THIRD SHAPE (02165b1d, annotation `third_instance_2026_09_08_2200Z`):
/// a PARKED car whose branch head MOVED. The operator's documented
/// recovery for "gate receipt is for X but the branch boards Y" is to
/// re-gate with `--park-*`, and that re-gate filed a second car instead
/// of refreshing the first — for the third time, by a route the two
/// landed guards do not cover.
#[cfg(test)]
mod a_moved_head_tests {
    use super::*;

    /// The branch of the re-railed car, measured 2026-09-08 21:53 UTC.
    const BRANCH: &str = "feat/arrival-runs-the-probe-rerail";
    /// Its SUBJECT — the branch it was FILED under, before `boss rerail`
    /// repointed `metadata.branch`. The two disagree, and the read that
    /// trusted them to agree is the defect.
    const SUBJECT: &str = "feat/arrival-runs-the-probe";
    /// The head its receipt vouched for, and the head the branch moved
    /// to when a colliding migration prefix was renumbered and pushed.
    const OLD_HEAD: &str = "fb973bd0d1d0da9c25fd64a22263de3f4baa1fef";
    const NEW_HEAD: &str = "0ec4521fd4a5c632e01d6e6ed9926b1bf300fe6e";

    /// Car 538775dd as it stood when the re-gate went green: parked at
    /// the dock, receipt for a head the branch had left, boarding
    /// refusing it with a `skip_reason` — and its subject still the
    /// pre-rerail branch.
    fn rerailed_parked_car() -> Value {
        json!({
            "id": "538775dd-64ee-4897-b9d6-b49137eb6512",
            "kind": "ship-a-change",
            "status": "open",
            "subject": { "subject_kind": "custom", "id": SUBJECT },
            "metadata": {
                "branch": BRANCH,
                "regate_receipt": format!(
                    "{{\"verdict\": \"green\", \"head\": \"{OLD_HEAD}\", \"mode\": \"full\", \"fails\": []}}"
                ),
                "skip_reason":
                    "gate receipt is for fb973bd0 but the branch boards 0ec4521f — gated, then changed",
            },
            "steps": [
                {"spec_slug": "gate", "title": car::GATE, "status": "completed"},
                {"spec_slug": "review", "title": car::REVIEW, "status": "ready"},
            ]
        })
    }

    /// Another branch's car, so the page proves a branch FILTER and not
    /// merely "the one row we asked the server for".
    fn someone_elses_car() -> Value {
        json!({
            "id": "aaaaaaaa-0000-0000-0000-000000000000",
            "kind": "ship-a-change",
            "status": "open",
            "subject": { "subject_kind": "custom", "id": "feat/other" },
            "metadata": { "branch": "feat/other" },
            "steps": [
                {"spec_slug": "gate", "title": car::GATE, "status": "completed"},
                {"spec_slug": "review", "title": car::REVIEW, "status": "ready"},
            ]
        })
    }

    /// THE MEASURED CASE. Given the whole open page, the car whose head
    /// moved is REFRESHED. This is the ordinary state of affairs after
    /// a rebase, a re-rail, or a renumbered migration — not an edge.
    #[test]
    fn a_parked_car_whose_head_moved_is_refreshed_not_twinned() {
        let page = [someone_elses_car(), rerailed_parked_car()];
        match park_action(&page, &page, BRANCH, false) {
            ParkAction::Refresh(car) => assert_eq!(
                car["id"], "538775dd-64ee-4897-b9d6-b49137eb6512",
                "the moved-head car at the dock is the one refreshed"
            ),
            other => panic!("a re-gate after the head moved refreshes the parked car: {other:?}"),
        }
    }

    /// WHAT THE HANDLER USED TO SEE, and why it filed twin ef4eff3c.
    /// The open read was narrowed server-side by `subject_id`, which a
    /// re-railed car does not answer to. The narrowed page is empty,
    /// "no parked car" reads as "no car", and a second car is filed.
    #[test]
    fn the_subject_keyed_read_is_what_filed_the_twin() {
        let page = [someone_elses_car(), rerailed_parked_car()];
        let subject_keyed: Vec<Value> = page
            .iter()
            .filter(|c| c.pointer("/subject/id").and_then(Value::as_str) == Some(BRANCH))
            .cloned()
            .collect();
        assert!(
            subject_keyed.is_empty(),
            "a re-railed car does not answer subject_id={BRANCH} — that is the miss"
        );
        assert!(
            matches!(
                park_action(&subject_keyed, &subject_keyed, BRANCH, false),
                ParkAction::File
            ),
            "the subject-narrowed page files a twin — which is what happened at 21:53"
        );
    }

    /// So the query must not narrow by subject, and must page.
    #[test]
    fn the_open_car_query_is_keyed_on_the_branch_not_the_subject() {
        let url = open_cars_url("http://jobs", 0);
        assert!(
            !url.contains("subject_id"),
            "the open-car read must not narrow by subject: {url}"
        );
        assert!(url.contains("kind=ship-a-change"), "{url}");
        assert!(url.contains("status=open"), "{url}");
        assert!(url.contains("offset=0"), "{url}");
        assert!(
            open_cars_url("http://jobs", CARS_PAGE).contains(&format!("offset={CARS_PAGE}")),
            "a dock deeper than one page is still read whole (a-limit-is-not-a-filter)"
        );
    }

    /// The refresh writes the CURRENT receipt and DELETES the stale
    /// skip — `skip_reason` present-and-null, which the metadata door
    /// removes. Without both, the car keeps refusing to board for a
    /// reason that stopped being true.
    #[test]
    fn the_refresh_supersedes_the_receipt_and_clears_the_skip() {
        let fresh = Receipt {
            raw: format!(
                "{{\"verdict\": \"green\", \"head\": \"{NEW_HEAD}\", \"mode\": \"full\", \"fails\": []}}"
            ),
            head: NEW_HEAD.to_string(),
            mode: "full".to_string(),
        };
        let patch = car::regate_patch(&fresh, "re-gated in place", None);
        assert_eq!(patch["regate_receipt"], fresh.raw);
        assert!(
            patch["regate_receipt"].as_str().unwrap().contains(NEW_HEAD),
            "the fresh head, not {OLD_HEAD}"
        );
        assert_eq!(
            patch["skip_reason"],
            Value::Null,
            "a null key is deleted by the metadata door — the stale skip goes with the stale receipt"
        );
    }
}

/// THE CAR IS THE ITEM'S BUILD (backlog ca76d8f9).
#[cfg(test)]
mod park_routes_its_item_tests {
    use super::*;

    const ITEM: &str = "5942f205-0f0e-4a51-9a31-2f8f3b0b7a11";
    const CAR_ID: &str = "9442139b-1616-4b8d-8a7a-d1e34ff96486";

    /// Every step write the stand-in received, in arrival order:
    /// `(method, step_id, body)`.
    type StepLog = Arc<std::sync::Mutex<Vec<(&'static str, String, Value)>>>;

    /// Stand-in for jobs-api holding one un-triaged backlog item, and
    /// recording every step write — the merge-door PATCH and the step
    /// PUT — so the routing writes can be read back in order. `status`
    /// is the code the item GET answers with — 500 stands in for an SoR
    /// that cannot be reached.
    async fn mock_item(item: Option<Value>, status: axum::http::StatusCode) -> (String, StepLog) {
        use axum::{Json, Router, extract::Path, routing::get};
        let puts: StepLog = Arc::new(std::sync::Mutex::new(Vec::new()));
        let put_log = puts.clone();
        let patch_log = puts.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move || {
                    let item = item.clone();
                    async move {
                        match item {
                            Some(item) if status.is_success() => Ok(Json(item)),
                            _ => Err(status),
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}",
                axum::routing::put(
                    move |Path((_id, step_id)): Path<(String, String)>, Json(body): Json<Value>| {
                        let puts = put_log.clone();
                        async move {
                            puts.lock().unwrap().push(("PUT", step_id, body));
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}/metadata",
                axum::routing::patch(
                    move |Path((_id, step_id)): Path<(String, String)>, Json(body): Json<Value>| {
                        let log = patch_log.clone();
                        async move {
                            log.lock().unwrap().push(("PATCH", step_id, body));
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), puts)
    }

    fn untriaged_item() -> Value {
        json!({
            "id": ITEM,
            "kind": "backlog-item",
            "status": "open",
            "steps": [
                { "id": "s-filed", "spec_slug": "filed", "status": "completed", "metadata": {} },
                { "id": "s-triage", "spec_slug": "triage", "status": "ready", "metadata": {} },
                { "id": "s-build", "spec_slug": "build", "status": "pending", "metadata": {} },
            ],
        })
    }

    /// THE WRITE. Parking a car that names an un-triaged item completes
    /// that item's triage with `disposition = build` and evidence naming
    /// the car — so the `build` step the arrival rule completes actually
    /// opens, instead of the item sitting at triage while the change is
    /// live.
    #[tokio::test]
    async fn parking_routes_the_linked_item_to_build() {
        let (base, puts) = mock_item(Some(untriaged_item()), axum::http::StatusCode::OK).await;
        let h = JobsAutoPark::new(
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        h.triage_linked_item(ITEM, CAR_ID, "fix/x", "jobs.auto-park")
            .await;

        // TWO writes, merge first (backlog e39a9d2a): the route through
        // the step merge door, then a PUT carrying the status alone — a
        // PUT with metadata replaces the step's stored keys wholesale.
        let writes = puts.lock().unwrap().clone();
        assert_eq!(writes.len(), 2, "merge, then status: {writes:?}");
        let (method, step_id, merged) = &writes[0];
        assert_eq!((*method, step_id.as_str()), ("PATCH", "s-triage"));
        assert_eq!(merged["disposition"], "build");
        let evidence = merged["evidence"].as_str().unwrap_or_default();
        assert!(
            evidence.contains("9442139b") && evidence.contains("fix/x"),
            "the evidence names the car and its branch: {evidence}"
        );
        let (method, step_id, put) = &writes[1];
        assert_eq!((*method, step_id.as_str()), ("PUT", "s-triage"));
        assert_eq!(put, &json!({"status": "completed"}));
    }

    /// IDEMPOTENT ON A RE-GATE. The refresh path runs this too, and an
    /// item a person already routed keeps their disposition — nothing is
    /// written at all.
    #[tokio::test]
    async fn a_regate_never_re_routes_an_item_someone_decided() {
        let mut decided = untriaged_item();
        decided["steps"][1]["status"] = json!("completed");
        decided["steps"][1]["metadata"] = json!({ "disposition": "verify", "evidence": "by hand" });
        let (base, puts) = mock_item(Some(decided), axum::http::StatusCode::OK).await;
        let h = JobsAutoPark::new(
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        h.triage_linked_item(ITEM, CAR_ID, "fix/x", "jobs.auto-park")
            .await;
        assert!(
            puts.lock().unwrap().is_empty(),
            "a person's disposition is never overwritten"
        );
    }

    /// BEST-EFFORT, AND THAT IS THE POINT. An unreachable system of
    /// record costs the routing write, never the park: the car is
    /// already filed and green by the time this runs, and a fallible
    /// write in a loop like this one froze every landing once
    /// (CLAUDE.md §Diagnosis). The same holds for an edge stored as an
    /// 8-char prefix, which would be a permanent 400.
    #[tokio::test]
    async fn a_routing_write_that_cannot_land_does_not_fail_the_park() {
        let (base, puts) = mock_item(None, axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
        let h = JobsAutoPark::new(
            base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        // Returns () — there is no error for the park to propagate.
        h.triage_linked_item(ITEM, CAR_ID, "fix/x", "jobs.auto-park")
            .await;
        assert!(puts.lock().unwrap().is_empty());

        let h = JobsAutoPark::new(
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        h.triage_linked_item("5942f205", CAR_ID, "fix/x", "jobs.auto-park")
            .await;
        assert!(
            puts.lock().unwrap().is_empty(),
            "a prefix edge is not followed — it is a permanent 400"
        );
    }
}

/// A GREEN FINISHES THE CAR THE BUILDER OPENED (backlog be025b44).
///
/// `boss car open` files the `ship-a-change` packet at build START, so
/// by the time this handler sees a green the branch usually ALREADY has
/// a car — one whose `gate` step has not reported, which is to say one
/// that is neither parked nor boarded. Every earlier guard answered that
/// shape with `File`, and filing there would be the fourth twin of the
/// same kind (d052afad, 02165b1d, 6790175e): a packet recording a build
/// that already has one, on the dock beside it.
#[cfg(test)]
mod building_car_tests {
    use super::*;
    // PRESENT-AND-NULL IS THE DOOR'S DELETE, and `patch[key].is_null()`
    // cannot see the difference between that and a key the patch never
    // wrote (backlog 2e4c200f). These two say which instruction is meant.
    use boss_testing::{assert_absent, assert_explicit_null};

    const BRANCH: &str = "feat/a-car-opens-when-the-build-starts";

    /// A car as `boss car open` leaves it: scope declared with the brief,
    /// build CLAIMED by the builder, gate and review still ahead.
    fn building() -> Value {
        json!({
            "id": "be025b44-0000-0000-0000-000000000000",
            "kind": "ship-a-change",
            "status": "open",
            "subject": { "subject_kind": "custom", "id": BRANCH },
            "metadata": {
                "branch": BRANCH,
                "build_started_at": "2026-09-10T18:00:00Z",
                "built_by": "claude@algedonic.dev",
                "build_host": "boss-dev-0",
            },
            "steps": [
                {"id": "s-opened", "spec_slug": "opened", "title": car::OPENED,
                 "status": "completed"},
                {"id": "s-scope", "spec_slug": "scope", "title": car::SCOPE,
                 "status": "completed",
                 "metadata": {"summary": "s", "excludes": "e"}},
                {"id": "s-build", "spec_slug": "build", "title": car::BUILD,
                 "status": "active", "assignee_id": "claude@algedonic.dev"},
                {"id": "s-gate", "spec_slug": "gate", "title": car::GATE, "status": "pending"},
                {"id": "s-review", "spec_slug": "review", "title": car::REVIEW,
                 "status": "pending"},
            ],
        })
    }

    /// A car `boss dispatch --started` admitted and `boss gate` moved
    /// into its gate: build completed at launch, gate ACTIVE and held by
    /// the gate-run (backlog 68a8726e).
    fn at_gate(gate_run: &str) -> Value {
        let mut c = building();
        c["metadata"][car::ADMITTED_BY] = json!("boss dispatch --started");
        c["steps"][2]["status"] = json!("completed");
        c["steps"][3]["status"] = json!("active");
        c["steps"][3]["metadata"] = json!({ car::GATE_RUN: gate_run });
        c
    }

    /// THE GREEN FINISHES THE CAR IN THE GATE — one car at review, no
    /// twin (backlog 68a8726e). A car whose gate step is ACTIVE is still
    /// the branch's building car, so the decision is Adopt, and the only
    /// write the green owes it is the gate step itself.
    #[test]
    fn a_green_adopts_the_car_standing_at_its_gate() {
        let open = [at_gate("gr-1")];
        let adopted = match park_action(&open, &open, BRANCH, false) {
            ParkAction::Adopt(c) => c,
            other => panic!("the car in the gate is finished, not twinned: {other:?}"),
        };
        let receipt = Receipt {
            raw: "{}".into(),
            head: "deadbeef".into(),
            mode: "full".into(),
        };
        let writes =
            car::finish_writes(adopted, "s", "e", "t", "v", &receipt, chrono::Utc::now()).unwrap();
        assert_eq!(
            writes
                .iter()
                .map(|w| w.step_id.as_str())
                .collect::<Vec<_>>(),
            ["s-gate"],
            "build was handed in at launch; the green completes the gate alone"
        );
    }

    /// AUTO-PARK IS A NO-OP FOR A GATE-RUN THAT CARRIES AN EDGE (design
    /// 689d1f7f car 2, backlog a3a367e4). Its verdict finishes the step
    /// the edge names, so the scan never adopts, refreshes or files for
    /// it — whatever the dock holds — and the edge path and an Adopt
    /// cannot both finish one gate. Without the edge — an old dispatcher
    /// reading an edged gate-run, which ignores the key — the same car
    /// is still Adopted: the edge is additive at the cutover.
    #[test]
    fn an_edged_gate_run_is_never_adopted_refreshed_or_filed_by_the_scan() {
        let open = [at_gate("gr-1")];
        for (cars, what) in [(&open[..], "a car at its gate"), (&[][..], "no car at all")] {
            match park_action(cars, cars, BRANCH, true) {
                ParkAction::Skip(patch) => {
                    assert_eq!(patch["park_skipped"], "finished-by-its-verdict", "{what}")
                }
                other => panic!("{what}: an edged gate-run is the verdict's: {other:?}"),
            }
        }
        assert!(matches!(
            park_action(&open, &open, BRANCH, false),
            ParkAction::Adopt(_)
        ));
    }

    /// THE GREEN FOLLOWS THE EDGE AND READS NO DOCK. A stub whose open-
    /// and closed-car listings answer 500: a green that scanned would
    /// fail, so passing proves the edge path reads only the gate-run and
    /// the one car it names — and writes only that car.
    #[tokio::test]
    async fn a_green_with_an_edge_reads_the_car_it_names_and_no_listing() {
        use axum::{
            Json, Router,
            extract::Request,
            http::{Method, StatusCode},
            response::IntoResponse,
        };
        const G1: &str = "1a1d2e3f-4a5b-4c6d-8e7f-0000000000e1";
        let car_id = "be025b44-0000-0000-0000-000000000000";
        let mut held = at_gate(G1);
        held["steps"][3]["metadata"]["gated_head"] = json!("1111111aaaa");
        let gate_run = json!({
            "id": G1, "kind": "gate-run",
            "metadata": {
                "branch": BRANCH, "sha": "1111111aaaa",
                "park_summary": "s", "park_excludes": "e", "park_test": "t",
                "park_verified": "v",
                "park_probe": "echo ok", "park_expect": "ok",
                "park_partial_item": "b8e75382-0000-0000-0000-000000000000",
                boss_jobs::fulfils::KEY: {"job": car_id, "step": "s-gate"},
            },
        });
        type Writes = Arc<std::sync::Mutex<Vec<(String, String, Value)>>>;
        let writes: Writes = Arc::default();
        let retained = writes.clone();
        let app = Router::new().fallback(move |request: Request| {
            let (held, gate_run, writes) = (held.clone(), gate_run.clone(), retained.clone());
            async move {
                let method = request.method().clone();
                let path = request.uri().path().to_string();
                if method == Method::GET {
                    return if path == format!("/api/jobs/{G1}") {
                        Json(gate_run).into_response()
                    } else if path == format!("/api/jobs/{car_id}") {
                        Json(held).into_response()
                    } else {
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    };
                }
                let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
                    .await
                    .unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                writes
                    .lock()
                    .unwrap()
                    .push((method.to_string(), path, body));
                StatusCode::NO_CONTENT.into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let handler = JobsAutoPark::new(
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        let receipt = r#"{"verdict":"green","head":"1111111aaaa","mode":"full"}"#;
        let ctx = InvocationContext {
            rule_name: "auto-park-on-gate-green".into(),
            triggering_event_id: "evt-edge-green".into(),
            triggering_topic: "step.done.gate-verdict".into(),
            event_timestamp: Some(chrono::Utc::now()),
            event_payload: json!({"job_id": G1, "step_id": "verdict", "kind": "gate-verdict",
                "metadata": {"verdict": "green", "receipt": receipt}}),
        };
        let result = handler.invoke(&[], &ctx).await;
        server.abort();
        result.expect("the edge path reads no listing");
        let writes = writes.lock().unwrap().clone();
        assert!(
            writes
                .iter()
                .all(|(_, path, _)| path.starts_with(&format!("/api/jobs/{car_id}"))),
            "only the car the edge names is written: {writes:#?}"
        );
        assert_eq!(
            writes
                .last()
                .map(|(m, p, b)| (m.as_str(), p.as_str(), b.clone())),
            Some((
                "PUT",
                format!("/api/jobs/{car_id}/steps/s-gate").as_str(),
                json!({"status": "completed"})
            )),
            "{writes:#?}"
        );
    }

    /// An ADMITTED car was scoped before its builder wrote the boundary,
    /// so the green carries the builder's summary and excludes onto it.
    /// A car a builder opened by hand already has them, and keeps them.
    #[test]
    fn an_admitted_car_takes_its_builders_boundary_at_green() {
        let inputs = AutoParkInputs {
            branch: BRANCH.to_string(),
            summary: "what it does".into(),
            excludes: "what it leaves out".into(),
            test: "t".into(),
            verified: "v".into(),
            backlog_item: None,
            item_provenance: serde_json::Map::new(),
            boards_after: None,
            delivery_channel: None,
            receipt: Receipt {
                raw: "{}".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
            },
            proof: serde_json::Map::new(),
            flake: serde_json::Map::new(),
            agent_run: serde_json::Map::new(),
            tiers: serde_json::Map::new(),
            waits_on: None,
            hold: None,
            hold_bound: true,
        };
        let patch = adopt_patch(&at_gate("gr-1"), &inputs);
        assert_eq!(patch["summary"], "what it does");
        assert_eq!(patch["excludes"], "what it leaves out");
        let opened = adopt_patch(&building(), &inputs);
        assert!(opened.get("summary").is_none(), "{opened:#}");
    }

    /// A RED HANDS THE GATE BACK (backlog 68a8726e), through the
    /// handler's own invoke: the car whose gate step this gate-run holds
    /// gets the verdict merged onto that step and is PUT back to ready —
    /// nothing is filed, and a car another gate-run holds is untouched.
    #[tokio::test]
    async fn a_red_verdict_hands_back_only_the_gate_its_run_holds() {
        use axum::{
            Json, Router,
            extract::Request,
            http::{Method, StatusCode},
            response::IntoResponse,
        };
        let mut other = at_gate("gr-other");
        other["id"] = json!("other-car");
        let cars = json!({"data": [at_gate("gr-red"), other], "total": 2});
        type Writes = Arc<std::sync::Mutex<Vec<(String, String, Value)>>>;
        let writes: Writes = Arc::default();
        let retained = writes.clone();
        let app = Router::new().fallback(move |request: Request| {
            let (cars, writes) = (cars.clone(), retained.clone());
            async move {
                let method = request.method().clone();
                let path = request.uri().path().to_string();
                if method == Method::GET {
                    return Json(cars).into_response();
                }
                let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
                    .await
                    .unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                writes
                    .lock()
                    .unwrap()
                    .push((method.to_string(), path, body));
                StatusCode::NO_CONTENT.into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let handler = JobsAutoPark::new(
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        let ctx = InvocationContext {
            rule_name: "auto-park-on-gate-green".into(),
            triggering_event_id: "evt-red".into(),
            triggering_topic: "step.done.gate-verdict".into(),
            event_timestamp: None,
            event_payload: json!({"job_id": "gr-red", "step_id": "verdict",
                "kind": "gate-verdict", "metadata": {"verdict": "failed"}}),
        };
        let result = handler.invoke(&[], &ctx).await;
        server.abort();
        result.unwrap();
        let writes = writes.lock().unwrap().clone();
        let car_id = "be025b44-0000-0000-0000-000000000000";
        assert_eq!(
            writes,
            vec![
                (
                    "PATCH".to_string(),
                    format!("/api/jobs/{car_id}/steps/s-gate/metadata"),
                    json!({car::LAST_VERDICT: {car::GATE_RUN: "gr-red", "verdict": "failed"}}),
                ),
                (
                    "PUT".to_string(),
                    format!("/api/jobs/{car_id}/steps/s-gate"),
                    json!({"status": "ready"}),
                ),
            ],
            "the held car's gate goes back to ready; nothing is filed, the other car is left"
        );
    }

    /// THE CAR THIS CAR EXISTS FOR. A green whose branch already has a
    /// building car ADOPTS it — it does not file a second.
    #[test]
    fn a_green_adopts_the_car_the_builder_opened() {
        let open = [building()];
        match park_action(&open, &open, BRANCH, false) {
            ParkAction::Adopt(car) => {
                assert_eq!(car["id"], "be025b44-0000-0000-0000-000000000000")
            }
            other => panic!("a building car is finished, not twinned: {other:?}"),
        }
    }

    /// And what it writes onto that car is exactly the steps the open
    /// did NOT already complete — the step API refuses a metadata write
    /// to a completed step, so re-sending `scope` would 409 every time.
    #[test]
    fn adopting_writes_the_steps_the_open_left_open() {
        let inputs = AutoParkInputs {
            branch: BRANCH.to_string(),
            summary: "s".into(),
            excludes: "e".into(),
            test: "ran the tests".into(),
            verified: "seen working".into(),
            backlog_item: None,
            item_provenance: serde_json::Map::new(),
            boards_after: None,
            delivery_channel: Some("software".into()),
            receipt: Receipt {
                raw: "{\"verdict\":\"green\",\"head\":\"deadbeef\",\"mode\":\"full\"}".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
            },
            proof: car::proof_intent(Some("echo hi"), Some("hi"), None),
            flake: serde_json::Map::new(),
            agent_run: serde_json::Map::new(),
            tiers: serde_json::Map::new(),
            waits_on: None,
            hold: None,
            hold_bound: true,
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-10T18:30:00Z")
            .unwrap()
            .into();
        let writes = car::finish_writes(
            &building(),
            &inputs.summary,
            &inputs.excludes,
            &inputs.test,
            &inputs.verified,
            &inputs.receipt,
            now,
        )
        .expect("an opened car can be finished");
        let ids: Vec<&str> = writes.iter().map(|w| w.step_id.as_str()).collect();
        assert_eq!(ids, vec!["s-build", "s-gate"], "scope is already declared");
        assert_eq!(writes[1].metadata["receipt"], inputs.receipt.raw);

        // The proof intent and the channel ride the JOB, not a step, so
        // an adopted car carries what a freshly filed one would.
        let patch = adopt_patch(&building(), &inputs);
        assert_eq!(patch[car::PROOF_PROBE], "echo hi");
        assert_eq!(patch[car::PROOF_EXPECT], "hi");
        assert_eq!(patch["delivery_channel"], "software");
        assert!(
            patch.get("summary").is_none(),
            "the open already recorded the summary; an adopt does not restate it"
        );
    }

    /// THE MERGE HAZARD, PINNED. The file path merges the item
    /// provenance into `car_body_with_proof` and the refresh path into
    /// `regate_patch`; the adopt path must too, or a
    /// `--park-partial-item` answer is silently dropped for every car a
    /// builder opened. The two changes landed in different functions, so
    /// a textual merge could not see it — this is the test that can.
    #[test]
    fn an_adopt_carries_the_item_provenance_every_other_park_path_carries() {
        let inputs = AutoParkInputs {
            branch: BRANCH.to_string(),
            summary: "s".into(),
            excludes: "e".into(),
            test: "t".into(),
            verified: "v".into(),
            backlog_item: None,
            item_provenance: car::item_provenance(
                Some("cf0f5e2d-0000-0000-0000-000000000000"),
                None,
            ),
            boards_after: None,
            delivery_channel: None,
            receipt: Receipt {
                raw: "{}".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
            },
            proof: serde_json::Map::new(),
            flake: serde_json::Map::new(),
            agent_run: serde_json::Map::new(),
            tiers: serde_json::Map::new(),
            waits_on: None,
            hold: None,
            hold_bound: true,
        };
        let patch = adopt_patch(&building(), &inputs);
        assert_eq!(
            patch[car::PARTIAL_ITEM],
            "cf0f5e2d-0000-0000-0000-000000000000"
        );
        assert!(
            patch.get("backlog_item").is_none(),
            "a partial item must never become the key the arrival rule closes on: {patch}"
        );
        // And the same for a deliberately item-less car.
        let mut none = inputs;
        none.item_provenance = car::item_provenance(None, Some("David asked in conversation"));
        let patch = adopt_patch(&building(), &none);
        assert_eq!(patch[car::NO_ITEM_REASON], "David asked in conversation");
        assert!(patch.get("backlog_item").is_none());
    }

    /// THE TWO ANSWERS CAN CONTRADICT EACH OTHER, and the contradiction
    /// must not default to closing an item with work outstanding.
    ///
    /// `boss car open --backlog-item X` writes the CLOSING edge at build
    /// start; the gate is then given `--park-partial-item` or
    /// `--park-no-item`, which say the opposite. The gate's answer is the
    /// later one and made with the finished diff, and the two errors are
    /// not symmetrical — an item left open costs one human close, an item
    /// closed with work outstanding buries the work (e1325456). So the
    /// edge goes, the id survives under `PARTIAL_ITEM`, and the car says
    /// in prose why.
    #[test]
    fn a_gate_that_says_do_not_close_supersedes_an_edge_the_open_wrote() {
        let opened = {
            let mut c = building();
            c["metadata"]["backlog_item"] = json!("cf0f5e2d-0000-0000-0000-000000000000");
            c
        };
        let mut inputs = AutoParkInputs {
            branch: BRANCH.to_string(),
            summary: "s".into(),
            excludes: "e".into(),
            test: "t".into(),
            verified: "v".into(),
            backlog_item: None,
            item_provenance: car::item_provenance(None, Some("one piece of a bigger item")),
            boards_after: None,
            delivery_channel: None,
            receipt: Receipt {
                raw: "{}".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
            },
            proof: serde_json::Map::new(),
            flake: serde_json::Map::new(),
            agent_run: serde_json::Map::new(),
            tiers: serde_json::Map::new(),
            waits_on: None,
            hold: None,
            hold_bound: true,
        };
        let patch = adopt_patch(&opened, &inputs);
        assert_explicit_null!(
            patch,
            "backlog_item",
            "the arrival rule must find no edge to close"
        );
        assert_eq!(
            patch[car::PARTIAL_ITEM],
            "cf0f5e2d-0000-0000-0000-000000000000",
            "the id the open named survives under the key no rule follows"
        );
        let why = patch["item_answer_superseded"].as_str().unwrap_or_default();
        assert!(why.contains("cf0f5e2d"), "names the item: {why}");
        assert!(
            why.contains("--park-no-item") && why.contains("--park-backlog-item"),
            "names what was answered and how to undo it: {why}"
        );

        // A gate that names its own partial item keeps ITS id, not the
        // open's — the later, more specific statement.
        inputs.item_provenance =
            car::item_provenance(Some("aaaa1111-0000-0000-0000-000000000000"), None);
        let patch = adopt_patch(&opened, &inputs);
        assert_eq!(
            patch[car::PARTIAL_ITEM],
            "aaaa1111-0000-0000-0000-000000000000"
        );
        assert_explicit_null!(patch, "backlog_item");
    }

    /// AN ANSWER STATED AT OPEN SURVIVES THE ADOPT, UNCHANGED.
    ///
    /// `boss car open --partial-item X` / `--no-item "<reason>"` writes
    /// the answer on the car at build start, where the `scope` and
    /// `build` steps are completed too. The green then ADOPTS that
    /// packet, and an adopt that dropped the answer would lose the
    /// provenance for the whole build — which is what a sibling car was
    /// found doing with the gate's copy of exactly these keys.
    ///
    /// The common case is AGREEMENT: the builder states the answer once
    /// at open and `--park-*` confirms it. Verified, not assumed.
    #[test]
    fn an_item_answer_stated_at_open_survives_the_adopt() {
        let partial = "cf0f5e2d-0000-0000-0000-000000000000";
        let opened = |key: &str, value: &str| {
            let mut c = building();
            c["metadata"][key] = json!(value);
            c
        };
        let inputs = |provenance: serde_json::Map<String, Value>| AutoParkInputs {
            branch: BRANCH.to_string(),
            summary: "s".into(),
            excludes: "e".into(),
            test: "t".into(),
            verified: "v".into(),
            backlog_item: None,
            item_provenance: provenance,
            boards_after: None,
            delivery_channel: None,
            receipt: Receipt {
                raw: "{}".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
            },
            proof: serde_json::Map::new(),
            flake: serde_json::Map::new(),
            agent_run: serde_json::Map::new(),
            tiers: serde_json::Map::new(),
            waits_on: None,
            hold: None,
            hold_bound: true,
        };

        // ONE PIECE, stated at open and confirmed at the gate.
        let car = opened(car::PARTIAL_ITEM, partial);
        let same = inputs(car::item_provenance(Some(partial), None));
        assert!(
            supersede(&car, &same).is_empty(),
            "agreement is not a contradiction"
        );
        let patch = adopt_patch(&car, &same);
        assert_eq!(
            patch[car::PARTIAL_ITEM],
            partial,
            "the answer the open recorded is still on the car after the adopt: {patch}"
        );
        assert!(
            patch.get("backlog_item").is_none(),
            "and the adopt never turns it into the key that closes the item: {patch}"
        );
        assert!(patch.get(car::NO_ITEM_REASON).is_none(), "{patch}");

        // NO ITEM, stated at open and confirmed at the gate.
        let reason = "David asked for this in conversation";
        let car = opened(car::NO_ITEM_REASON, reason);
        let same = inputs(car::item_provenance(None, Some(reason)));
        assert!(supersede(&car, &same).is_empty());
        let patch = adopt_patch(&car, &same);
        assert_eq!(patch[car::NO_ITEM_REASON], reason, "{patch}");
        assert!(patch.get("backlog_item").is_none(), "{patch}");
        assert!(patch.get(car::PARTIAL_ITEM).is_none(), "{patch}");
    }

    /// AFTER AN ADOPT A CAR STATES ONE ITEM ANSWER, NOT TWO.
    ///
    /// Now that an open can record a NON-closing answer, the
    /// contradiction has a second direction `supersede` does not cover:
    /// the open said `--no-item "<reason>"` (or `--partial-item X`) and
    /// the gate then said something else. The gate's answer is written,
    /// and without this the open's would simply stay beside it — a car
    /// reading "this car answers no item" next to the edge that closes
    /// one. Nothing is buried, so this is residue rather than harm, but
    /// residue on a packet is read as fact by the next session.
    ///
    /// DELETED ONLY WHEN THE GATE REALLY STATED AN ANSWER. A destructive
    /// write that fires when it should not is worse than the residue it
    /// removes, so a gate-run carrying no item answer at all (only
    /// reachable by hand-editing the packet) removes nothing.
    #[test]
    fn a_gate_that_answers_differently_leaves_the_car_stating_one_answer() {
        let partial = "cf0f5e2d-0000-0000-0000-000000000000";
        let closing = "90d291bb-0000-0000-0000-000000000000";
        let reason = "found while building something else";
        let inputs = |backlog_item: Option<&str>,
                      provenance: serde_json::Map<String, Value>|
         -> AutoParkInputs {
            AutoParkInputs {
                branch: BRANCH.to_string(),
                summary: "s".into(),
                excludes: "e".into(),
                test: "t".into(),
                verified: "v".into(),
                backlog_item: backlog_item.map(str::to_string),
                item_provenance: provenance,
                boards_after: None,
                delivery_channel: None,
                receipt: Receipt {
                    raw: "{}".into(),
                    head: "deadbeef".into(),
                    mode: "full".into(),
                },
                proof: serde_json::Map::new(),
                flake: serde_json::Map::new(),
                agent_run: serde_json::Map::new(),
                tiers: serde_json::Map::new(),
                waits_on: None,
                hold: None,
                hold_bound: true,
            }
        };
        let opened = |key: &str, value: &str| {
            let mut c = building();
            c["metadata"][key] = json!(value);
            c
        };

        // The open said NO ITEM; the gate names the closing edge. The
        // edge is written by `adopt_edge_patch`; the stale reason must go.
        let car = opened(car::NO_ITEM_REASON, reason);
        let patch = adopt_patch(&car, &inputs(Some(closing), serde_json::Map::new()));
        assert_explicit_null!(
            patch,
            car::NO_ITEM_REASON,
            "a car cannot both answer no item and carry the edge that closes one"
        );

        // The open said ONE PIECE; the gate says no item at all.
        let car = opened(car::PARTIAL_ITEM, partial);
        let patch = adopt_patch(
            &car,
            &inputs(None, car::item_provenance(None, Some(reason))),
        );
        assert_eq!(patch[car::NO_ITEM_REASON], reason, "{patch}");
        assert_explicit_null!(
            patch,
            car::PARTIAL_ITEM,
            "the gate's answer replaces the open's, rather than sitting next to it"
        );

        // A gate that states NOTHING about the item deletes nothing — the
        // hand-edited-packet case, where the recorded answer is the only
        // answer there is.
        let car = opened(car::PARTIAL_ITEM, partial);
        let patch = adopt_patch(&car, &inputs(None, serde_json::Map::new()));
        // ABSENT, not deleted: the contrast with the two assertions above
        // is the whole point — `assert_explicit_null!` would pass here if the
        // door's two instructions were interchangeable, and they are not.
        assert_absent!(patch, car::PARTIAL_ITEM, "nothing stated, nothing removed");
    }

    /// AND NOTHING IS SUPERSEDED WHEN THE ANSWERS AGREE. The common case
    /// must not touch the edge: a destructive write that fires when it
    /// should not is worse than the drop it was written to prevent.
    #[test]
    fn an_agreeing_or_absent_answer_supersedes_nothing() {
        let opened = {
            let mut c = building();
            c["metadata"]["backlog_item"] = json!("cf0f5e2d-0000-0000-0000-000000000000");
            c
        };
        let base = |backlog_item: Option<&str>,
                    provenance: serde_json::Map<String, Value>|
         -> AutoParkInputs {
            AutoParkInputs {
                branch: BRANCH.to_string(),
                summary: "s".into(),
                excludes: "e".into(),
                test: "t".into(),
                verified: "v".into(),
                backlog_item: backlog_item.map(str::to_string),
                item_provenance: provenance,
                boards_after: None,
                delivery_channel: None,
                receipt: Receipt {
                    raw: "{}".into(),
                    head: "deadbeef".into(),
                    mode: "full".into(),
                },
                proof: serde_json::Map::new(),
                flake: serde_json::Map::new(),
                agent_run: serde_json::Map::new(),
                tiers: serde_json::Map::new(),
                waits_on: None,
                hold: None,
                hold_bound: true,
            }
        };
        // The gate names the closing edge too: agreement, not conflict.
        let agree = base(
            Some("cf0f5e2d-0000-0000-0000-000000000000"),
            serde_json::Map::new(),
        );
        assert!(supersede(&opened, &agree).is_empty());
        // The gate carries no provenance at all.
        assert!(supersede(&opened, &base(None, serde_json::Map::new())).is_empty());
        // A partial answer, but the open named no edge — nothing to
        // supersede, and the provenance rides on its own.
        let partial = base(
            None,
            car::item_provenance(Some("cf0f5e2d-0000-0000-0000-000000000000"), None),
        );
        assert!(supersede(&building(), &partial).is_empty());
        // Both set at once can only come from a hand-edited packet. The
        // closing edge is the safer thing to keep, so it is kept.
        let both = base(
            Some("cf0f5e2d-0000-0000-0000-000000000000"),
            car::item_provenance(Some("aaaa1111"), None),
        );
        assert!(supersede(&opened, &both).is_empty());
    }

    /// THE DECLARED EDGE RIDES ITS OWN WRITE. `backlog_item` is
    /// ref-checked by a DB trigger that answers 400, and 400 maps to
    /// `Downstream`, which REDELIVERS — so an unresolvable edge folded
    /// into the proof-intent patch would retry the adopt forever and cost
    /// the car its probe on every attempt.
    #[test]
    fn the_backlog_edge_is_a_separate_best_effort_write() {
        let inputs = AutoParkInputs {
            branch: BRANCH.to_string(),
            summary: "s".into(),
            excludes: "e".into(),
            test: "t".into(),
            verified: "v".into(),
            backlog_item: Some("be025b44-2725-4db5-90d9-f16aba3844c6".into()),
            item_provenance: serde_json::Map::new(),
            boards_after: None,
            delivery_channel: None,
            receipt: Receipt {
                raw: "{}".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
            },
            proof: car::proof_intent(Some("echo hi"), Some("hi"), None),
            flake: serde_json::Map::new(),
            agent_run: serde_json::Map::new(),
            tiers: serde_json::Map::new(),
            waits_on: None,
            hold: None,
            hold_bound: true,
        };
        let patch = adopt_patch(&building(), &inputs);
        assert!(
            patch.get("backlog_item").is_none(),
            "a refused edge must not be able to cost the car its proof intent"
        );
        assert_eq!(patch[car::PROOF_PROBE], "echo hi");
        // Nothing is NULLED: the metadata door deletes a null key, and a
        // delivery channel the open recorded must survive a green that
        // could not classify the diff.
        assert!(patch.get("delivery_channel").is_none());

        let edge = adopt_edge_patch(&building(), &inputs).expect("the edge is written");
        assert_eq!(edge["backlog_item"], "be025b44-2725-4db5-90d9-f16aba3844c6");

        // An open given `--backlog-item` already wrote it, ref-checked, at
        // the POST. Nothing to re-write.
        let mut linked = building();
        linked["metadata"]["backlog_item"] = json!("be025b44-2725-4db5-90d9-f16aba3844c6");
        assert!(adopt_edge_patch(&linked, &inputs).is_none());

        // And a green carrying no intent writes no edge at all.
        let mut unlinked = inputs;
        unlinked.backlog_item = None;
        assert!(adopt_edge_patch(&building(), &unlinked).is_none());
    }

    /// The earlier answers do not move. A car aboard a train, at the
    /// dock, or landed is still reported as such — the building case is
    /// asked LAST, after every guard that was measured first.
    #[test]
    fn the_guards_measured_first_still_answer_first() {
        let mut boarded = building();
        boarded["metadata"]["train"] = json!("d72ecdb9");
        boarded["steps"][3]["status"] = json!("completed");
        boarded["steps"][4]["status"] = json!("ready");
        let open = [building(), boarded];
        assert!(
            matches!(
                park_action(&open, &open, BRANCH, false),
                ParkAction::Skip(_)
            ),
            "a branch in transit is still skipped"
        );

        let mut landed = building();
        landed["metadata"]["merged"] = json!("true");
        let all = [landed];
        match park_action(&all, &all, BRANCH, false) {
            ParkAction::Skip(p) => assert_eq!(p["park_skipped"], "landed"),
            other => panic!("a landed branch records the landing: {other:?}"),
        }
    }

    /// A RERAILED building car answers on the branch it CARRIES, not the
    /// one it was filed under — the read this handler was already fixed
    /// once for (car 235157a5, backlog 02165b1d).
    #[test]
    fn a_rerailed_building_car_is_adopted_not_twinned() {
        let mut rerailed = building();
        rerailed["metadata"]["branch"] = json!("feat/rerailed");
        let open = [rerailed];
        match park_action(&open, &open, "feat/rerailed", false) {
            ParkAction::Adopt(c) => {
                assert_eq!(c["id"], "be025b44-0000-0000-0000-000000000000");
                assert_eq!(
                    c["subject"]["id"], BRANCH,
                    "its subject is still the filing branch — which is why the read \
                     must not narrow by subject"
                );
            }
            other => panic!("the rerailed building car is the one to finish: {other:?}"),
        }
    }
}

/// AN ERROR-SHAPED LISTING NEVER PARKS (backlog 37fc5837). Both car
/// reads here fail TOWARD A WRITE: read as "no cars", a body with no
/// `data` array sends `park_action` to `File`, which is a twin of a car
/// already at the dock, or a car for work that has already landed.
/// Driven end to end, so each test fails on its own read site.
#[cfg(test)]
mod no_data_array_tests {
    use super::*;
    use crate::handlers::listing_stub::{
        assert_refused_by_name, empty_listing, no_data_array, serve,
    };

    const GATE_RUN: &str = "0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f";

    fn green_gate_run() -> Value {
        json!({
            "id": GATE_RUN,
            "kind": "gate-run",
            "metadata": {
                "branch": "fix/x",
                "sha": "abc",
                "park_summary": "does a thing.",
                "park_excludes": "not that",
                "park_test": "ran it",
                "park_verified": "seen",
            },
        })
    }

    fn green_verdict() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "jobs.auto-park".into(),
            triggering_event_id: "evt-green-1".into(),
            triggering_topic: "step.done.gate-verdict".into(),
            event_payload: json!({
                "job_id": GATE_RUN,
                "step_id": "s-verdict",
                "kind": "gate-verdict",
                "metadata": {
                    "verdict": "green",
                    "receipt": "{\"verdict\":\"green\",\"head\":\"deadbeef\",\"mode\":\"full\",\"fails\":[]}",
                },
            }),
        }
    }

    fn handler(base: String) -> Arc<JobsAutoPark> {
        JobsAutoPark::new(
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    #[tokio::test]
    async fn an_open_car_read_with_no_data_array_refuses_and_files_nothing() {
        let stub = serve(vec![
            (
                "/api/jobs/0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f",
                green_gate_run(),
            ),
            ("/api/jobs?status=open", no_data_array()),
            ("/api/jobs?status=closed", empty_listing()),
        ])
        .await;
        let res = handler(stub.base.clone())
            .invoke(&[], &green_verdict())
            .await;
        assert_eq!(stub.writes(), Vec::<String>::new(), "no car filed blind");
        assert_refused_by_name(res, "the open-car read");
    }

    #[tokio::test]
    async fn a_landed_car_read_with_no_data_array_refuses_and_files_nothing() {
        let stub = serve(vec![
            (
                "/api/jobs/0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f",
                green_gate_run(),
            ),
            ("/api/jobs?status=open", empty_listing()),
            ("/api/jobs?status=closed", no_data_array()),
        ])
        .await;
        let res = handler(stub.base.clone())
            .invoke(&[], &green_verdict())
            .await;
        assert_eq!(
            stub.writes(),
            Vec::<String>::new(),
            "no car filed for work that may have landed"
        );
        assert_refused_by_name(res, "the landed-car read");
    }

    /// END TO END: a green whose park stated neither a probe nor an
    /// event files NO car, and says why loudly — a permanent error,
    /// whose dead-letter lands on the gate-run (backlog 031a3ca9).
    #[tokio::test]
    async fn a_green_whose_park_states_no_proof_files_no_car() {
        let stub = serve(vec![
            (
                "/api/jobs/0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f",
                green_gate_run(),
            ),
            ("/api/jobs?status=open", empty_listing()),
            ("/api/jobs?status=closed", empty_listing()),
        ])
        .await;
        let res = handler(stub.base.clone())
            .invoke(&[], &green_verdict())
            .await;
        assert_eq!(
            stub.writes(),
            Vec::<String>::new(),
            "no unprovable car filed"
        );
        match res {
            Err(HandlerError::Permanent(why)) => {
                assert!(why.contains(car::UNPROVEN_PARK), "{why}")
            }
            other => panic!("an unprobed park must be refused, got {other:?}"),
        }
    }
}

/// A GATE THAT CARRIES PARK INTENT AND A HOLD FILES A HELD CAR (backlog
/// 486dde37). Until 2026-09-26 `boss gate` refused `--hold` beside
/// `--park-*`, so a trust-boundary car waiting for its adversarial
/// review auto-parked UNHELD and could board on depth before the
/// operator's `boss hold` landed — three such cars on one day. The
/// handler now writes the gate-run's hold reason onto the car's review
/// step, the key `boss hold` writes and the conductor, the dock row and
/// orient read, BEFORE the gate step completes and makes the car
/// boardable.
#[cfg(test)]
mod held_park_tests {
    use super::*;
    use crate::handlers::listing_stub::{empty_listing, serve};

    const GATE_RUN: &str = "5e1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f";
    const REASON: &str = "trust-boundary car: parks after its adversarial review";

    fn held_gate_run() -> Value {
        json!({
            "id": GATE_RUN,
            "kind": "gate-run",
            "metadata": {
                "branch": "fix/x",
                "sha": "abc",
                "park_summary": "does a thing.",
                "park_excludes": "not that",
                "park_test": "ran it",
                "park_verified": "seen",
                "park_probe": "echo ok", "park_expect": "ok",
                "park_partial_item": "b8e75382-0000-0000-0000-000000000000",
                "hold": REASON,
            },
        })
    }

    fn green_meta() -> serde_json::Map<String, Value> {
        json!({
            "verdict": "green",
            "receipt": "{\"verdict\":\"green\",\"head\":\"deadbeef\",\"mode\":\"full\",\"fails\":[]}",
        })
        .as_object()
        .unwrap()
        .clone()
    }

    /// A car as the create answers it: every step still ahead.
    fn fresh_car() -> Value {
        json!({
            "id": "stub-created",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": "fix/x" },
            "steps": [
                {"id": "s-scope", "spec_slug": "scope", "title": car::SCOPE, "status": "ready"},
                {"id": "s-build", "spec_slug": "build", "title": car::BUILD, "status": "pending"},
                {"id": "s-gate", "spec_slug": "gate", "title": car::GATE, "status": "pending"},
                {"id": "s-review", "spec_slug": "review", "title": car::REVIEW, "status": "pending"},
            ]
        })
    }

    #[test]
    fn a_held_gate_still_parks_and_carries_its_reason() {
        let inputs = auto_park_inputs(&held_gate_run(), &green_meta())
            .expect("a hold does not stop the park — it rides it");
        assert_eq!(inputs.hold.as_deref(), Some(REASON));
        // Every receipt field still rides: the hold replaces none of them.
        assert_eq!(
            inputs.item_provenance.get(car::PARTIAL_ITEM),
            Some(&json!("b8e75382-0000-0000-0000-000000000000"))
        );
    }

    #[test]
    fn the_hold_lands_on_the_review_step_in_the_shape_every_reader_reads() {
        let inputs = auto_park_inputs(&held_gate_run(), &green_meta()).unwrap();
        let (path, body, judged) = hold_write(&fresh_car(), &inputs)
            .expect("a pending review takes the hold")
            .expect("a held gate owes the car its hold");
        assert_eq!(path, "/api/jobs/stub-created/steps/s-review/metadata");
        // Read back by the ONE definition of the marker — the function the
        // conductor's `parked_ready`, the dock row and orient all call.
        assert_eq!(
            boss_jobs::stranded::marked(&body, "hold").as_deref(),
            Some(REASON)
        );
        assert_eq!(
            body,
            json!({ "hold": REASON, car::HOLD_SHA: inputs.receipt.head }),
            "the head the hold was judged at rides in the same write (b7b02024)"
        );
        assert_eq!(judged, None, "a gate's own bound hold names its own head");
        assert!(
            !inputs.receipt.head.is_empty(),
            "the fixture's receipt names a head"
        );
        // A car already at the dock (review ready) is held the same way.
        let mut parked = fresh_car();
        parked["steps"][3]["status"] = json!("ready");
        assert!(hold_write(&parked, &inputs).unwrap().is_some());
        // afc5147c: a green refresh must move an existing hold, too.
        parked["steps"][3]["metadata"] = json!({"hold":"old reason", car::HOLD_SHA:"old-head"});
        let (_, refreshed, _) = hold_write(&parked, &inputs).unwrap().unwrap();
        parked["steps"][3]["metadata"] = refreshed;
        assert_eq!(
            boss_jobs::car_release::hold_sha(&parked),
            Some(inputs.receipt.head.as_str())
        );
    }

    /// A gate whose hold is only an UNREAD diff (no --hold, the judge
    /// could not read it) files its car held but not bound to a head, so
    /// a cleared hold is judged again rather than sent to a reviewer
    /// (backlog b7b02024, review F1).
    #[test]
    fn an_unread_diff_hold_is_filed_without_the_head() {
        let mut gr = held_gate_run();
        gr["metadata"][car::HOLD_UNBOUND] = json!(true);
        let inputs = auto_park_inputs(&gr, &green_meta()).unwrap();
        assert!(!inputs.hold_bound);
        let (_, body, judged) = hold_write(&fresh_car(), &inputs).unwrap().unwrap();
        assert_eq!(body, json!({ "hold": REASON }));
        assert_eq!(judged, None, "nothing to carry, so nothing judged");
        // Over a car already released at the dock, it deletes the head
        // that release lifted rather than leave it to read as this
        // hold's (backlog 6c463c66).
        let mut released = fresh_car();
        released["steps"][3]["status"] = json!("ready");
        released["steps"][3]["metadata"] =
            json!({ car::HOLD_SHA: "1111", car::RELEASE: {"sha": "1111"} });
        let (_, body, _) = hold_write(&released, &inputs).unwrap().unwrap();
        assert_eq!(body, json!({ "hold": REASON, car::HOLD_SHA: null }));
        // The mark absent, or false: a review hold, bound.
        gr["metadata"][car::HOLD_UNBOUND] = json!(false);
        assert!(auto_park_inputs(&gr, &green_meta()).unwrap().hold_bound);
    }

    #[test]
    fn an_unheld_gate_writes_no_hold() {
        let mut gr = held_gate_run();
        gr["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("hold")
            .unwrap();
        let inputs = auto_park_inputs(&gr, &green_meta()).unwrap();
        assert_eq!(inputs.hold, None);
        assert_eq!(hold_write(&fresh_car(), &inputs), Ok(None));
        // Nor does a released one (`false` / blank are no marker).
        for released in [json!(false), json!(""), Value::Null] {
            gr["metadata"]["hold"] = released;
            let inputs = auto_park_inputs(&gr, &green_meta()).unwrap();
            assert_eq!(hold_write(&fresh_car(), &inputs), Ok(None));
        }
    }

    /// A REFRESH THAT MOVES THE CAR'S HEAD MOVES ITS HOLD'S HEAD (backlog
    /// 6b12a70f) — the same sequence `boss rerail --finish` is pinned on
    /// (`a_refresh_carries_the_review_hold_to_the_head_it_refreshed`),
    /// through this path's `ParkAction::Refresh`. Car eb86ac0c, released
    /// at R, held by the conductor's re-judge for a review of A, is
    /// refreshed by an UNHELD green at B. The hold stays, and names B, so
    /// the review of B releases it at the head the hold names (the yard's
    /// and the release rule's head-less ask).
    #[test]
    fn an_unheld_refresh_carries_the_review_hold_to_the_head_it_refreshed() {
        let mut gr = held_gate_run();
        gr["metadata"].as_object_mut().unwrap().remove("hold");
        let inputs = auto_park_inputs(&gr, &green_meta()).unwrap();
        let b = inputs.receipt.head.clone();
        let reason = "conductor re-judge at 299433653fb1: released at 2b1591c1983f, head is \
                      299433653fb1: waits for a review of this head";
        let mut parked = fresh_car();
        parked["steps"][3]["status"] = json!("ready");
        parked["steps"][3]["metadata"] = json!({
            "hold": reason,
            car::HOLD_SHA: "299433653fb1",
            car::RELEASE: { "sha": "2b1591c1983f", "review": "2c4bad6a" },
        });
        let run = json!({
            "id": "7cb2438c-0000-4000-8000-000000000000",
            "kind": "agent-run",
            "metadata": { "review": { "car": "stub-created", "verdict": "release",
                                      "reviewed_sha": b } },
        });
        let stale = boss_jobs::car_release::hold_sha(&parked).unwrap();
        assert!(boss_jobs::car_release::vouches(&run, &parked, stale).is_err());
        let (path, body, judged) = hold_write(&parked, &inputs)
            .unwrap()
            .expect("a refresh that moves the car's head moves the hold's head");
        assert_eq!(path, "/api/jobs/stub-created/steps/s-review/metadata");
        assert_eq!(
            body,
            json!({ car::HOLD_SHA: b }),
            "the head alone: the hold's reason is not rewritten"
        );
        assert_eq!(
            judged,
            Some(car::hold_as_judged(&parked["steps"][3]["metadata"])),
            "the head moves only on the hold it read (backlog cd1f68c6)"
        );
        let md = parked["steps"][3]["metadata"].as_object_mut().unwrap();
        md.extend(body.as_object().unwrap().clone());
        assert_eq!(boss_jobs::car_release::hold_sha(&parked), Some(b.as_str()));
        assert_eq!(
            boss_jobs::stranded::marked(&parked["steps"][3]["metadata"], "hold").as_deref(),
            Some(reason)
        );
        boss_jobs::car_release::vouches(&run, &parked, &b)
            .expect("the review of B releases the car refreshed to B");
        // Already at B: nothing to move.
        assert_eq!(hold_write(&parked, &inputs), Ok(None));
        // A hold bound to no head (a hand hold, a red-train hold) binds
        // none on a refresh either: it is not a review's.
        parked["steps"][3]["metadata"] = json!({ "hold": "David only" });
        assert_eq!(hold_write(&parked, &inputs), Ok(None));
    }

    /// A held gate whose car has no open review cannot be held, and must
    /// not be parked as though it were: refused, naming the hold.
    #[test]
    fn a_held_gate_refuses_a_car_it_cannot_hold() {
        let inputs = auto_park_inputs(&held_gate_run(), &green_meta()).unwrap();
        let mut done = fresh_car();
        done["steps"][3]["status"] = json!("completed");
        let err = hold_write(&done, &inputs).unwrap_err();
        assert!(err.contains(REASON), "{err}");
        let mut none = fresh_car();
        none["steps"].as_array_mut().unwrap().pop();
        assert!(hold_write(&none, &inputs).is_err());
    }

    /// END TO END: the car is filed, the hold is written onto its review
    /// step, and only THEN does the gate step complete (which is what
    /// makes review ready and the car boardable). A hold written after
    /// that completion would leave a window in which the dock could board
    /// the car unheld — the race this item exists to close.
    #[tokio::test]
    async fn a_held_gate_files_its_car_held_before_it_can_board() {
        let stub = serve(vec![
            (
                "/api/jobs/5e1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f",
                held_gate_run(),
            ),
            ("/api/jobs/stub-created", fresh_car()),
            ("/api/jobs?status=open", empty_listing()),
            ("/api/jobs?status=closed", empty_listing()),
        ])
        .await;
        let ctx = InvocationContext {
            event_timestamp: Some(chrono::Utc::now()),
            rule_name: "jobs.auto-park".into(),
            triggering_event_id: "evt-green-held".into(),
            triggering_topic: "step.done.gate-verdict".into(),
            event_payload: json!({
                "job_id": GATE_RUN,
                "step_id": "s-verdict",
                "kind": "gate-verdict",
                "metadata": green_meta(),
            }),
        };
        JobsAutoPark::new(
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
        .invoke(&[], &ctx)
        .await
        .expect("a held green parks");
        let sent = stub.sent();
        let at = |w: &str| sent.iter().position(|(k, _)| k == w);
        assert!(at("POST /api/jobs").is_some(), "the car is filed: {sent:?}");
        let hold = at("PATCH /api/jobs/stub-created/steps/s-review/metadata")
            .expect("the hold is written onto the review step");
        assert_eq!(
            sent[hold].1,
            json!({ "hold": REASON, car::HOLD_SHA: "deadbeef" }),
            "the hold and the receipt head it was judged at, in one write (b7b02024)"
        );
        let gate_done = at("PUT /api/jobs/stub-created/steps/s-gate")
            .expect("the gate step still completes: every receipt field rides");
        assert!(
            hold < gate_done,
            "held BEFORE the gate step completes: {sent:?}"
        );
    }

    /// THE CAR'S STEP STAMPS ARE THE GREEN'S INSTANT (backlog 2b03a2df).
    /// `completed_at` on scope, build and gate was read off the clock
    /// service when this handler got round to the event, so a redelivery
    /// or a replay stamped a different time onto the same act. It is the
    /// green verdict event's own timestamp now — and a firing with none
    /// is refused rather than stamped with the consumer's clock.
    #[tokio::test]
    async fn the_cars_steps_are_stamped_with_the_greens_instant() {
        let stub = serve(vec![
            (
                "/api/jobs/5e1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f",
                held_gate_run(),
            ),
            ("/api/jobs/stub-created", fresh_car()),
            ("/api/jobs?status=open", empty_listing()),
            ("/api/jobs?status=closed", empty_listing()),
        ])
        .await;
        let green_at = chrono::DateTime::parse_from_rfc3339("2026-09-10T18:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let ctx = InvocationContext {
            event_timestamp: Some(green_at),
            rule_name: "jobs.auto-park".into(),
            triggering_event_id: "evt-green-at".into(),
            triggering_topic: "step.done.gate-verdict".into(),
            event_payload: json!({
                "job_id": GATE_RUN,
                "step_id": "s-verdict",
                "kind": "gate-verdict",
                "metadata": green_meta(),
            }),
        };
        let park = JobsAutoPark::new(
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        park.invoke(&[], &ctx).await.expect("a green parks");
        let sent = stub.sent();
        for step in ["s-scope", "s-build", "s-gate"] {
            let path = format!("PATCH /api/jobs/stub-created/steps/{step}/metadata");
            let (_, body) = sent
                .iter()
                .find(|(w, _)| *w == path)
                .unwrap_or_else(|| panic!("{step} is filled: {sent:?}"));
            assert_eq!(
                body["completed_at"],
                json!("2026-09-10T18:30:00Z"),
                "{step} carries the green's instant"
            );
        }

        // No instant on the firing: refused by name, and no car is filed
        // for a green whose stamps would have to be invented.
        let dark = serve(vec![
            (
                "/api/jobs/5e1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f",
                held_gate_run(),
            ),
            ("/api/jobs/stub-created", fresh_car()),
            ("/api/jobs?status=open", empty_listing()),
            ("/api/jobs?status=closed", empty_listing()),
        ])
        .await;
        let err = JobsAutoPark::new(
            dark.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
        .invoke(
            &[],
            &InvocationContext {
                event_timestamp: None,
                ..ctx
            },
        )
        .await
        .expect_err("no instant to stamp");
        assert!(err.is_permanent(), "{err}");
        assert!(
            !dark.writes().iter().any(|w| w.starts_with("POST ")),
            "nothing filed: {:?}",
            dark.writes()
        );
    }

    // -- the refresh writes only on the hold it judged (cd1f68c6) -------
    //
    // The REAL jobs router over an in-memory store on a local socket, so
    // the judgement is enforced by the merge door itself, not by a stub
    // that answers the way it is hoped to. The handler judges the car it
    // READ; a concurrent writer moves the review step after that read and
    // before the write — Codex's injection (review 5d605949) — and the
    // write must be refused and land nothing.

    const HELD_AT: &str = "299433653fb1";
    const REVIEW_HOLD: &str = "conductor re-judge at 299433653fb1 — waits for a review of this \
                               head (backlog b7b02024)";

    /// The car as the handler read it (the judged snapshot), and the
    /// store it lives in, served.
    async fn held_car_served() -> (
        String,
        Arc<boss_jobs::InMemoryJobs>,
        Value,
        boss_core::job::StepId,
    ) {
        use boss_jobs::JobsRepository as _;
        use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new());
        let mut job = boss_core::job::Job::new(
            "ship-a-change",
            boss_core::job::Subject::new("custom", "fix/x"),
            "A held car",
            "emp-owner",
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
        );
        job.status = boss_core::job::JobStatus::Open;
        jobs.create_job(&job).await.unwrap();
        let review_md = held_car_metadata();
        let mut step = boss_core::job::Step::new(job.id, "task", car::REVIEW, 4);
        step.spec_slug = Some(car::REVIEW_SLUG.into());
        step.status = boss_core::job::StepStatus::Ready;
        step.metadata = review_md.clone();
        jobs.add_step(&step).await.unwrap();
        let mut policy = FakePolicyClient::builder();
        for (action, resource) in [
            (Action::Read, Resource::job()),
            (Action::Update, Resource::job()),
            (Action::Update, Resource::step()),
        ] {
            policy = policy.allow("platform-admin", action, resource, Scope::All);
        }
        let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let app = boss_jobs::http::router(boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let judged = json!({
            "id": job.id.to_string(),
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": "fix/x" },
            "steps": [{ "id": step.id.to_string(), "spec_slug": car::REVIEW_SLUG,
                        "title": car::REVIEW, "status": "ready", "metadata": review_md }],
        });
        (base, jobs, judged, step.id)
    }

    /// Refresh the judged car through `hold_car` after `concurrent` lands
    /// on its review step (if any); the handler's answer and the review
    /// step's metadata as it then stands.
    async fn refresh_after(
        concurrent: Option<Value>,
        inputs: &AutoParkInputs,
    ) -> (Result<(), HandlerError>, Value) {
        use boss_jobs::JobsRepository as _;
        let (base, jobs, judged, step_id) = held_car_served().await;
        if let Some(Value::Object(write)) = concurrent {
            jobs.merge_step_metadata_at(
                &step_id,
                &write,
                &boss_core::publisher::EventStamp::new(
                    "jobs",
                    boss_core::actor::ActorId::Automation("concurrent".into()),
                ),
            )
            .await
            .unwrap();
        }
        let handler = JobsAutoPark::new(
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        let answer = handler.hold_car(&judged, inputs, "jobs.auto-park").await;
        let md = jobs.get_step(&step_id).await.unwrap().unwrap().metadata;
        (answer, md)
    }

    fn unheld_refresh() -> AutoParkInputs {
        let mut gr = held_gate_run();
        gr["metadata"].as_object_mut().unwrap().remove("hold");
        auto_park_inputs(&gr, &green_meta()).unwrap()
    }

    fn unbound_refresh() -> AutoParkInputs {
        let mut gr = held_gate_run();
        gr["metadata"][car::HOLD_UNBOUND] = json!(true);
        auto_park_inputs(&gr, &green_meta()).unwrap()
    }

    /// The control: nothing moved since the read, so the hold follows the
    /// refresh to its head (backlog 6b12a70f), through both writers.
    #[tokio::test]
    async fn a_refresh_on_the_hold_it_judged_carries_it_to_the_new_head() {
        let (answer, md) = refresh_after(None, &unheld_refresh()).await;
        answer.expect("the row reads as judged");
        assert_eq!(md[car::HOLD_SHA], "deadbeef");
        assert_eq!(md["hold"], REVIEW_HOLD, "the reason stands");
        let (answer, md) = refresh_after(None, &unbound_refresh()).await;
        answer.expect("the row reads as judged");
        assert_eq!(md[car::HOLD_SHA], "deadbeef");
        assert_eq!(md["hold"], REASON);
    }

    /// An unheld refresh (the `hold_follows_head` write alone) whose hold
    /// was released, or replaced by a person's, after it was read: the
    /// refresh is refused in words and nothing is written, so no
    /// `hold_sha` is left beside no hold and no person's hold is bound to
    /// a head a reviewer could release.
    #[tokio::test]
    async fn an_unheld_refresh_does_not_rebind_a_hold_that_moved_after_its_read() {
        for concurrent in [
            json!({ "hold": null, car::RELEASE: { "sha": HELD_AT, "review": "7cb2438c" } }),
            json!({ "hold": "David reads this one first" }),
        ] {
            let mut expected = held_car_metadata();
            expected
                .as_object_mut()
                .unwrap()
                .extend(concurrent.as_object().unwrap().clone());
            expected
                .as_object_mut()
                .unwrap()
                .retain(|_, v| !v.is_null());
            let (answer, md) = refresh_after(Some(concurrent.clone()), &unheld_refresh()).await;
            let err = answer.expect_err("a hold that moved since the read is not carried");
            assert!(!err.is_permanent(), "a redelivery judges again: {err}");
            let said = err.to_string();
            assert!(
                said.contains("changed") && said.contains("nothing was written"),
                "{said}"
            );
            assert_eq!(md, expected, "{concurrent}: nothing written");
            assert_eq!(md[car::HOLD_SHA], HELD_AT);
        }
    }

    /// The gate's unbound hold over a live review hold carries that hold's
    /// head too (`hold_follows_head`, the second writer). A release that
    /// lands between the read and the write must not have its lifted
    /// hold re-bound to the new head: an unread diff's hold is a person's
    /// to judge again, never a reviewer's to release.
    #[tokio::test]
    async fn an_unbound_refresh_does_not_bind_its_hold_over_a_release_it_did_not_read() {
        let released =
            json!({ "hold": null, car::RELEASE: { "sha": HELD_AT, "review": "7cb2438c" } });
        let (answer, md) = refresh_after(Some(released), &unbound_refresh()).await;
        let err = answer.expect_err("the release landed after the read");
        assert!(err.to_string().contains("nothing was written"), "{err}");
        assert!(md.get("hold").is_none(), "{md}");
        assert_eq!(md[car::HOLD_SHA], HELD_AT, "{md}");
    }

    fn held_car_metadata() -> Value {
        json!({
            "hold": REVIEW_HOLD,
            car::HOLD_SHA: HELD_AT,
            car::RELEASE: { "sha": "2b1591c1983f", "review": "2c4bad6a" },
        })
    }
}

/// THE GATE STEP MOVES ONLY FOR THE GATE-RUN THAT HOLDS IT (Codex review
/// of car 0d04ee3e, R2 and R3). Two gates on one branch: G1 launched,
/// then G2 restamped the car's gate step to itself. Neither G1's green
/// nor G1's red may move what G2 now holds — judged on the read, and
/// again at every write, by the jobs API's own merge door and step PUT
/// over a real router, so the judgement is the server's, not a stub's.
#[cfg(test)]
mod gate_binding_tests {
    use super::*;
    use boss_jobs::JobsRepository as _;

    const G1: &str = "1a1d2e3f-4a5b-4c6d-8e7f-000000000001";
    const G2: &str = "2a1d2e3f-4a5b-4c6d-8e7f-000000000002";
    const H1: &str = "1111111aaaa";
    const H2: &str = "2222222bbbb";
    const BRANCH: &str = "feat/two-gates-one-car";

    struct Served {
        base: String,
        jobs: Arc<boss_jobs::InMemoryJobs>,
        car_id: String,
        build: boss_core::job::StepId,
        gate: boss_core::job::StepId,
        armed: Armed,
    }

    /// A launch armed to land INSIDE a handler's `invoke`: once the served
    /// router has answered the request `(method, path)` — and before that
    /// answer reaches the handler — `merge` lands on the car's gate step,
    /// as `boss gate`'s own restamp does. One-shot. It is how a test puts a
    /// restamp between the read a verdict judged and the writes it sends
    /// without driving those writes by hand (review 01fe1c71 of car
    /// 3a7ab114: the edge path reaches the gate writes by its own route,
    /// so only a test through `invoke` pins what that route carries).
    type Armed = Arc<std::sync::Mutex<Option<(axum::http::Method, String, Value)>>>;

    /// Arm `s`: after the router answers `method path`, `merge` lands on
    /// the gate step.
    fn arm(s: &Served, method: axum::http::Method, path: String, merge: Value) {
        *s.armed.lock().unwrap() = Some((method, path, merge));
    }

    /// A car at its gate, ACTIVE and held by `holder` at `head`, in an
    /// in-memory store behind the real jobs router.
    async fn car_held_by(holder: &str, head: &str) -> Served {
        car_with(
            boss_core::job::StepStatus::Completed,
            boss_core::job::StepStatus::Active,
            json!({ car::GATE_RUN: holder, "gated_head": head }),
        )
        .await
    }

    /// A car still AT BUILD — build claimed, gate pending and UNBOUND:
    /// the car `boss dispatch --started` admits, before any launch.
    async fn car_at_build() -> Served {
        car_with(
            boss_core::job::StepStatus::Active,
            boss_core::job::StepStatus::Pending,
            json!({}),
        )
        .await
    }

    async fn car_with(
        build_status: boss_core::job::StepStatus,
        gate_status: boss_core::job::StepStatus,
        gate_md: Value,
    ) -> Served {
        use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new());
        let mut job = boss_core::job::Job::new(
            "ship-a-change",
            boss_core::job::Subject::new("custom", BRANCH),
            "A car at its gate",
            "emp-owner",
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
        );
        job.status = boss_core::job::JobStatus::Open;
        job.metadata = json!({ "branch": BRANCH, car::ADMITTED_BY: "boss dispatch --started" });
        jobs.create_job(&job).await.unwrap();
        let mut scope = boss_core::job::Step::new(job.id, "task", car::SCOPE, 1);
        scope.spec_slug = Some(car::SCOPE_SLUG.into());
        scope.status = boss_core::job::StepStatus::Completed;
        jobs.add_step(&scope).await.unwrap();
        let mut build = boss_core::job::Step::new(job.id, "task", car::BUILD, 2);
        build.spec_slug = Some(car::BUILD_SLUG.into());
        build.status = build_status;
        jobs.add_step(&build).await.unwrap();
        let mut gate = boss_core::job::Step::new(job.id, "task", car::GATE, 3);
        gate.spec_slug = Some(car::GATE_SLUG.into());
        gate.status = gate_status;
        gate.metadata = gate_md;
        jobs.add_step(&gate).await.unwrap();
        let mut policy = FakePolicyClient::builder();
        for (action, resource) in [
            (Action::Read, Resource::job()),
            (Action::Update, Resource::job()),
            (Action::Update, Resource::step()),
        ] {
            policy = policy.allow("platform-admin", action, resource, Scope::All);
        }
        let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let app = boss_jobs::http::router(boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        ));
        let armed: Armed = Arc::default();
        let app = {
            let (armed, jobs, gate_id) = (armed.clone(), jobs.clone(), gate.id);
            app.layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let (armed, jobs) = (armed.clone(), jobs.clone());
                    async move {
                        let fired = {
                            let mut slot = armed.lock().unwrap();
                            let hit = slot.as_ref().is_some_and(|(m, p, _)| {
                                m == request.method() && p == request.uri().path()
                            });
                            if hit { slot.take() } else { None }
                        };
                        let answer = next.run(request).await;
                        if let Some((_, _, merge)) = fired {
                            jobs.merge_step_metadata_at(
                                &gate_id,
                                merge.as_object().unwrap(),
                                &boss_core::publisher::EventStamp::new(
                                    "jobs",
                                    boss_core::actor::ActorId::Automation("boss-gate".into()),
                                ),
                            )
                            .await
                            .unwrap();
                        }
                        answer
                    }
                },
            ))
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Served {
            base,
            jobs,
            car_id: job.id.to_string(),
            build: build.id,
            gate: gate.id,
            armed,
        }
    }

    fn handler(base: &str) -> Arc<JobsAutoPark> {
        JobsAutoPark::new(
            base.to_string(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    /// The car as a handler reads it: `GET /api/jobs/{id}`.
    async fn read(s: &Served) -> Value {
        get_json(
            &handler(&s.base).client,
            &format!("{}/api/jobs/{}", s.base, s.car_id),
            "test",
        )
        .await
        .unwrap()
    }

    /// G2's launch, landing between a handler's read and its write: the
    /// step restamped to G2 and left ACTIVE — `boss gate`'s own merge.
    async fn g2_launches(s: &Served) {
        s.jobs
            .merge_step_metadata_at(
                &s.gate,
                json!({ car::GATE_RUN: G2, "gated_head": H2 })
                    .as_object()
                    .unwrap(),
                &boss_core::publisher::EventStamp::new(
                    "jobs",
                    boss_core::actor::ActorId::Automation("boss-gate".into()),
                ),
            )
            .await
            .unwrap();
    }

    /// A step's status moved under the store's own version compare — the
    /// launch's build completion and gate claim, as a test can drive them.
    async fn set_status(
        s: &Served,
        id: &boss_core::job::StepId,
        status: boss_core::job::StepStatus,
    ) {
        let (mut step, read) = s.jobs.get_step_versioned(id).await.unwrap().unwrap();
        step.status = status;
        s.jobs
            .update_step_if_unchanged_at(&step, read, chrono::Utc::now(), &[])
            .await
            .unwrap();
    }

    async fn gate_step(s: &Served) -> boss_core::job::Step {
        s.jobs.get_step(&s.gate).await.unwrap().unwrap()
    }

    fn held_by_g2_untouched(step: &boss_core::job::Step) {
        assert_eq!(step.status, boss_core::job::StepStatus::Active, "{step:?}");
        assert_eq!(step.metadata[car::GATE_RUN], G2, "{:?}", step.metadata);
        assert!(
            step.metadata.get(car::LAST_VERDICT).is_none(),
            "{:?}",
            step.metadata
        );
        assert!(
            step.metadata.get("receipt").is_none(),
            "{:?}",
            step.metadata
        );
    }

    /// R3, the first window: G2 restamps between G1's read and its MERGE.
    #[tokio::test]
    async fn a_red_does_not_hand_back_a_gate_restamped_before_its_merge() {
        let s = car_held_by(G1, H1).await;
        let judged = read(&s).await;
        g2_launches(&s).await;
        let h = handler(&s.base);
        for w in car::gate_return(&judged, G1, "failed") {
            if h.send_judged(&w, "test").await.unwrap() == Judged::Moved {
                break;
            }
        }
        held_by_g2_untouched(&gate_step(&s).await);
    }

    /// R3, the second window: G1's merge lands, THEN G2 restamps, THEN
    /// G1's status PUT — refused, so G2's step stays ACTIVE.
    #[tokio::test]
    async fn a_red_does_not_hand_back_a_gate_restamped_between_its_writes() {
        let s = car_held_by(G1, H1).await;
        let judged = read(&s).await;
        let writes = car::gate_return(&judged, G1, "failed");
        assert_eq!(writes.len(), 2);
        let h = handler(&s.base);
        assert_eq!(
            h.send_judged(&writes[0], "test").await.unwrap(),
            Judged::Written
        );
        g2_launches(&s).await;
        assert_eq!(
            h.send_judged(&writes[1], "test").await.unwrap(),
            Judged::Moved,
            "the PUT is judged on the holder the merge was"
        );
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Active, "{step:?}");
        assert_eq!(step.metadata[car::GATE_RUN], G2);
    }

    /// The control: nobody moved, so G1's red hands its own gate back.
    #[tokio::test]
    async fn a_red_hands_back_the_gate_it_still_holds() {
        let s = car_held_by(G1, H1).await;
        handler(&s.base)
            .hand_gate_back(G1, "failed", "test")
            .await
            .unwrap();
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Ready, "{step:?}");
        assert_eq!(step.metadata[car::LAST_VERDICT][car::GATE_RUN], G1);
    }

    /// R2 at write time: G1's green judged the binding while G1 held the
    /// step, then G2 launched before the gate writes. They land on
    /// nothing; G2 still holds the step, with no receipt from G1.
    #[tokio::test]
    async fn a_green_judged_before_a_restamp_lands_nothing_on_the_gate() {
        let s = car_held_by(G1, H1).await;
        let judged = read(&s).await;
        let expect = car::green_binding(&judged, G1, None, H1)
            .expect("G1 holds the step when the green reads it");
        let receipt = Receipt {
            raw: format!("{{\"verdict\":\"green\",\"head\":\"{H1}\"}}"),
            head: H1.into(),
            mode: "full".into(),
        };
        let writes =
            car::finish_writes(&judged, "s", "e", "t", "v", &receipt, chrono::Utc::now()).unwrap();
        g2_launches(&s).await;
        let landed = handler(&s.base)
            .finish_steps(&s.car_id, writes, &expect, G1, "test")
            .await
            .unwrap();
        assert_eq!(landed, Judged::Moved);
        held_by_g2_untouched(&gate_step(&s).await);
    }

    /// R2 from UNBOUND (Codex review of car 0d04ee3e, round 2): G1's
    /// green reads a car still at build — no gate-run binds its gate
    /// step — then G2 launches (build completed, gate stamped G2 and
    /// claimed) before either of G1's finishing writes. The binding G1
    /// judged is ABSENCE, and its writes carry it: they land on nothing,
    /// so G1 never completes G2's gate with G1's receipt.
    #[tokio::test]
    async fn a_green_that_read_an_unbound_gate_lands_nothing_once_another_run_binds_it() {
        let s = car_at_build().await;
        let expect = car::green_binding(&read(&s).await, G1, None, H1).unwrap();
        assert_eq!(expect, car::unbound_gate(), "G1 read the gate unbound");
        // G2's launch: build handed in, the gate stamped and claimed.
        set_status(&s, &s.build, boss_core::job::StepStatus::Completed).await;
        g2_launches(&s).await;
        set_status(&s, &s.gate, boss_core::job::StepStatus::Active).await;
        // G1's handler re-reads the full car before deciding its writes,
        // as `invoke` does, and finds G2's ACTIVE gate to finish.
        let receipt = Receipt {
            raw: format!("{{\"verdict\":\"green\",\"head\":\"{H1}\"}}"),
            head: H1.into(),
            mode: "full".into(),
        };
        let writes = car::finish_writes(
            &read(&s).await,
            "s",
            "e",
            "t",
            "v",
            &receipt,
            chrono::Utc::now(),
        )
        .unwrap();
        assert!(writes.iter().any(|w| w.title == car::GATE), "{writes:?}");
        let landed = handler(&s.base)
            .finish_steps(&s.car_id, writes, &expect, G1, "test")
            .await
            .unwrap();
        assert_eq!(landed, Judged::Moved);
        held_by_g2_untouched(&gate_step(&s).await);
    }

    /// R2, both orderings Codex named. G1's green while G2 holds the step
    /// is refused before any write; G2's red then hands G2's gate back.
    /// And once G2's red has handed it back, a late G1 green is still not
    /// G2's verdict and finishes nothing.
    #[tokio::test]
    async fn a_stale_green_never_finishes_a_gate_another_run_holds_in_either_order() {
        let s = car_held_by(G2, H2).await;
        let stale = car::green_binding(&read(&s).await, G1, None, H1).unwrap_err();
        assert!(stale.contains("held by gate-run"), "{stale}");
        held_by_g2_untouched(&gate_step(&s).await);

        handler(&s.base)
            .hand_gate_back(G2, "failed", "test")
            .await
            .unwrap();
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Ready);
        assert_eq!(step.metadata[car::LAST_VERDICT][car::GATE_RUN], G2);

        assert!(car::green_binding(&read(&s).await, G1, None, H1).is_err());
    }

    /// A gate-run filed for `BRANCH` at `H1` with park intent, as the
    /// conductor's lost-car relaunch files one when `relaunch_of` names
    /// the lost run (`lost_gate::relaunch_marks`), else as a hand gate.
    async fn gate_run_filed(s: &Served, id: &str, relaunch_of: Option<&str>) {
        gate_run_filed_with(s, id, relaunch_of, json!({})).await
    }

    /// [`gate_run_filed`], with `extra` merged onto its metadata — the
    /// `fulfils` edge a launch on a car writes (backlog a3a367e4).
    async fn gate_run_filed_with(s: &Served, id: &str, relaunch_of: Option<&str>, extra: Value) {
        let mut md = json!({
            "branch": BRANCH, "sha": H1,
            "park_summary": "s", "park_excludes": "e", "park_test": "t",
            "park_verified": "v",
            "park_probe": "echo ok", "park_expect": "ok",
            "park_partial_item": "b8e75382-0000-0000-0000-000000000000",
        });
        if let Some(lost) = relaunch_of {
            md[boss_jobs::lost_gate::RELAUNCH_OF] = json!(lost);
        }
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            md[k] = v;
        }
        let mut run = boss_core::job::Job::new(
            "gate-run",
            boss_core::job::Subject::new("custom", BRANCH),
            "A gate-run",
            "emp-owner",
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap(),
        )
        .with_id(boss_core::job::JobId::from_uuid(
            uuid::Uuid::parse_str(id).unwrap(),
        ));
        run.status = boss_core::job::JobStatus::Open;
        run.metadata = md;
        s.jobs.create_job(&run).await.unwrap();
    }

    /// `gate_run`'s green verdict at `H1`, through the handler's own invoke.
    async fn green_of(s: &Served, gate_run: &str) {
        let receipt = format!("{{\"verdict\":\"green\",\"head\":\"{H1}\",\"mode\":\"full\"}}");
        let ctx = InvocationContext {
            rule_name: "auto-park-on-gate-green".into(),
            triggering_event_id: format!("evt-{gate_run}-green"),
            triggering_topic: "step.done.gate-verdict".into(),
            event_timestamp: Some(chrono::Utc::now()),
            event_payload: json!({"job_id": gate_run, "step_id": "verdict",
                "kind": "gate-verdict",
                "metadata": {"verdict": "green", "receipt": receipt}}),
        };
        handler(&s.base).invoke(&[], &ctx).await.unwrap();
    }

    /// `gate_run`'s NON-green verdict, through the handler's own invoke.
    async fn verdict_of(s: &Served, gate_run: &str, verdict: &str) {
        let ctx = InvocationContext {
            rule_name: "auto-park-on-gate-green".into(),
            triggering_event_id: format!("evt-{gate_run}-{verdict}"),
            triggering_topic: "step.done.gate-verdict".into(),
            event_timestamp: Some(chrono::Utc::now()),
            event_payload: json!({"job_id": gate_run, "step_id": "verdict",
                "kind": "gate-verdict", "metadata": {"verdict": verdict}}),
        };
        handler(&s.base).invoke(&[], &ctx).await.unwrap();
    }

    /// The R8 edge `boss gate` writes onto a gate-run it launched on
    /// `s`'s car (`car::gate_edge`, backlog a3a367e4).
    fn edge_to(s: &Served) -> Value {
        boss_jobs::fulfils::Fulfils {
            job: s.car_id.clone(),
            step: s.gate.to_string(),
        }
        .patch()
    }

    /// THE ONE-CAR INVARIANT: every car carrying `BRANCH`, in ANY status.
    /// This fixture's car has no review step, so a completed gate closes
    /// it; counting only open cars would read that as "no car" — the
    /// very misreading that filed three twins — and miss a twin filed
    /// beside it.
    async fn cars_on_branch(s: &Served) -> usize {
        let all = get_json(
            &handler(&s.base).client,
            &format!("{}/api/jobs?kind=ship-a-change&limit=200", s.base),
            "test",
        )
        .await
        .unwrap();
        rows_or_refuse::<Value>(&all, "test")
            .unwrap()
            .iter()
            .filter(|c| c.pointer("/metadata/branch").and_then(Value::as_str) == Some(BRANCH))
            .count()
    }

    async fn run_metadata(s: &Served, id: &str) -> Value {
        let id = boss_core::job::JobId::from_uuid(uuid::Uuid::parse_str(id).unwrap());
        s.jobs.get_job(&id).await.unwrap().unwrap().metadata
    }

    /// THE VERDICT COMPLETES THE STEP ITS GATE-RUN FULFILS (design
    /// 689d1f7f car 2, backlog a3a367e4). A park-intent gate launched on
    /// the branch's open car carries the edge to that car's gate step;
    /// its green completes that step with the receipt, leaves the car at
    /// review, and the branch still has exactly one open car. A
    /// redelivery of the same green finds the step finished and writes
    /// nothing more.
    #[tokio::test]
    async fn a_green_follows_its_edge_and_leaves_one_car_with_its_gate_complete() {
        let s = car_held_by(G1, H1).await;
        gate_run_filed_with(&s, G1, None, edge_to(&s)).await;
        assert_eq!(cars_on_branch(&s).await, 1);
        green_of(&s, G1).await;
        let step = gate_step(&s).await;
        assert_eq!(
            step.status,
            boss_core::job::StepStatus::Completed,
            "{:?}",
            step.metadata
        );
        let receipt = step.metadata["receipt"].as_str().unwrap_or_default();
        assert!(receipt.contains(H1), "{:?}", step.metadata);
        assert_eq!(step.metadata[car::GATE_RUN], G1);
        assert_eq!(cars_on_branch(&s).await, 1, "one car, no twin");
        assert_eq!(run_metadata(&s, G1).await.get("park_skipped"), None);

        green_of(&s, G1).await;
        assert_eq!(gate_step(&s).await.metadata, step.metadata, "redelivered");
        assert_eq!(cars_on_branch(&s).await, 1);
    }

    /// A GREEN REPLAYED AFTER ITS CAR CLOSED WRITES NO FALSE RECORD
    /// (review 01fe1c71 of car 3a7ab114, finding 3). This fixture's car
    /// has no review step, so the green that completes its gate closes it
    /// — the shape of a car that has since landed. A redelivery then
    /// finds the step it fulfils COMPLETED on a car no longer open: that
    /// green DID finish the car, so the answer is "already finished", not
    /// an "edge-refused, no car finished" note on the gate-run whose green
    /// finished it. The scan wrote a truthful "landed" skip in the same
    /// case; the edge must not be less truthful.
    #[tokio::test]
    async fn a_green_replayed_after_its_car_closed_records_no_refusal() {
        let s = car_held_by(G1, H1).await;
        gate_run_filed_with(&s, G1, None, edge_to(&s)).await;
        green_of(&s, G1).await;
        assert!(
            !car::is_open(&read(&s).await),
            "the fixture's car closes on its gate: {}",
            read(&s).await["status"]
        );
        let finished = gate_step(&s).await;
        assert_eq!(finished.status, boss_core::job::StepStatus::Completed);

        green_of(&s, G1).await;
        assert_eq!(
            run_metadata(&s, G1).await.get("park_skipped"),
            None,
            "a replay of the green that finished the car is not a refusal: {}",
            run_metadata(&s, G1).await
        );
        assert_eq!(gate_step(&s).await.metadata, finished.metadata);
        assert_eq!(cars_on_branch(&s).await, 1);
    }

    /// THE EDGE-GREEN WRITE IS JUDGED ON THE HOLDER (review 01fe1c71 of
    /// car 3a7ab114, finding 1 — R2 of the Codex review of car 0d04ee3e,
    /// on the edge's own route). G1's green follows its edge and reads the
    /// car held by G1 at H1; G2 is then launched on the same branch at the
    /// SAME head — a re-gate after a suspected flake — and restamps the
    /// step, ACTIVE. Only the holder now tells the two apart, so G1's merge
    /// and PUT must land on nothing: G2's gate is never completed with G1's
    /// receipt, and G1's gate-run says why. Driven through `invoke`, with
    /// G2's launch landing the moment the router has answered the read
    /// `car::edge_green` judged.
    #[tokio::test]
    async fn an_edge_green_judged_before_a_same_head_restamp_lands_nothing() {
        let s = car_held_by(G1, H1).await;
        gate_run_filed_with(&s, G1, None, edge_to(&s)).await;
        arm(
            &s,
            axum::http::Method::GET,
            format!("/api/jobs/{}", s.car_id),
            json!({ car::GATE_RUN: G2, "gated_head": H1 }),
        );
        green_of(&s, G1).await;
        assert!(s.armed.lock().unwrap().is_none(), "G2's launch landed");
        let step = gate_step(&s).await;
        held_by_g2_untouched(&step);
        assert_eq!(step.metadata["gated_head"], H1, "the same head");
        assert_eq!(
            run_metadata(&s, G1).await["park_skipped"],
            "gate-held-elsewhere"
        );
        assert_eq!(cars_on_branch(&s).await, 1);
    }

    /// THE EDGE-RED SEND LOOP IS JUDGED ON THE HOLDER, in both windows R3
    /// named (review 01fe1c71 of car 3a7ab114: `return_gate`'s loop was
    /// pinned only through `hand_gate_back`). G2 restamps at the same
    /// head after G1's red read the car — before the merge, then, on a
    /// fresh car, between the merge and the PUT. Either way G2's gate
    /// stays ACTIVE and G2's: a red never hands back a gate another run
    /// holds.
    #[tokio::test]
    async fn an_edge_red_hands_back_nothing_once_a_same_head_restamp_lands() {
        let s = car_held_by(G1, H1).await;
        gate_run_filed_with(&s, G1, None, edge_to(&s)).await;
        arm(
            &s,
            axum::http::Method::GET,
            format!("/api/jobs/{}", s.car_id),
            json!({ car::GATE_RUN: G2, "gated_head": H1 }),
        );
        verdict_of(&s, G1, "failed").await;
        assert!(s.armed.lock().unwrap().is_none(), "G2's launch landed");
        held_by_g2_untouched(&gate_step(&s).await);

        let s = car_held_by(G1, H1).await;
        gate_run_filed_with(&s, G1, None, edge_to(&s)).await;
        arm(
            &s,
            axum::http::Method::PATCH,
            format!("/api/jobs/{}/steps/{}/metadata", s.car_id, s.gate),
            json!({ car::GATE_RUN: G2, "gated_head": H1 }),
        );
        verdict_of(&s, G1, "failed").await;
        assert!(s.armed.lock().unwrap().is_none(), "G2's launch landed");
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Active, "{step:?}");
        assert_eq!(step.metadata[car::GATE_RUN], G2, "{:?}", step.metadata);
    }

    /// A HALF-WRITTEN EDGE ON A GREEN FINISHES NOTHING BY THE SCAN (review
    /// 01fe1c71 of car 3a7ab114, finding 2). A gate-run whose `fulfils` is
    /// present but cannot be followed is still an edged gate-run: the
    /// handler tells `park_action` so, which answers Skip before it looks
    /// at any car — so the scan neither adopts the branch's car nor
    /// refreshes nor files one. Nothing is written to any car.
    #[tokio::test]
    async fn a_green_with_a_half_written_edge_is_skipped_and_moves_no_car() {
        let s = car_held_by(G1, H1).await;
        gate_run_filed_with(
            &s,
            G1,
            None,
            json!({ boss_jobs::fulfils::KEY: { "job": s.car_id } }),
        )
        .await;
        let before = read(&s).await;
        green_of(&s, G1).await;
        assert_eq!(
            run_metadata(&s, G1).await["park_skipped"],
            "finished-by-its-verdict"
        );
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Active, "{step:?}");
        assert!(
            step.metadata.get("receipt").is_none(),
            "{:?}",
            step.metadata
        );
        assert_eq!(
            read(&s).await["metadata"],
            before["metadata"],
            "no car write"
        );
        assert_eq!(cars_on_branch(&s).await, 1);
    }

    /// AN EDGE TO AN UNBOUND GATE FINISHES NOTHING, through `invoke`
    /// (review 01fe1c71, beside finding 2: `car::edge_green`'s refusal of
    /// an unbound step was pinned only as a unit). The car is still at
    /// build — no launch has stamped its gate step — so no gate-run holds
    /// it, and an edge naming it is a locator with nothing to locate.
    #[tokio::test]
    async fn an_edge_to_an_unbound_gate_moves_nothing() {
        let s = car_at_build().await;
        gate_run_filed_with(&s, G1, None, edge_to(&s)).await;
        green_of(&s, G1).await;
        assert_eq!(run_metadata(&s, G1).await["park_skipped"], "edge-refused");
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Pending, "{step:?}");
        assert!(
            step.metadata.get("receipt").is_none(),
            "{:?}",
            step.metadata
        );
        let build = s.jobs.get_step(&s.build).await.unwrap().unwrap();
        assert_eq!(build.status, boss_core::job::StepStatus::Active);
    }

    /// AN EDGE IS AT LEAST AS STRICT AS THE SCAN ABOUT THE BRANCH (review
    /// 01fe1c71 of car 3a7ab114, finding 4). The scan finds a car only by
    /// the branch its gate-run gated; the edge names the car outright, so
    /// it must check the same thing — a gate-run that gated another branch
    /// finishes nothing here, even with the holder and the head matching,
    /// and its park prose never lands on this car.
    #[tokio::test]
    async fn an_edge_from_a_gate_run_on_another_branch_finishes_nothing() {
        let s = car_held_by(G1, H1).await;
        gate_run_filed_with(
            &s,
            G1,
            None,
            json!({
                "branch": "feat/some-other-branch",
                boss_jobs::fulfils::KEY: { "job": s.car_id, "step": s.gate.to_string() },
            }),
        )
        .await;
        let before = read(&s).await;
        green_of(&s, G1).await;
        assert_eq!(run_metadata(&s, G1).await["park_skipped"], "edge-refused");
        let note = run_metadata(&s, G1).await["park_skipped_note"].clone();
        assert!(
            note.as_str().unwrap_or_default().contains("branch"),
            "{note}"
        );
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Active, "{step:?}");
        assert_eq!(step.metadata[car::GATE_RUN], G1);
        assert!(
            step.metadata.get("receipt").is_none(),
            "{:?}",
            step.metadata
        );
        assert_eq!(
            read(&s).await["metadata"],
            before["metadata"],
            "no car write"
        );
    }

    /// A RED BY EDGE LEAVES THE GATE READY, NEVER COMPLETED — the trust
    /// boundary of backlog a3a367e4: a completed gate step boards. The
    /// verdict sits beside the step, the branch keeps its one car, and
    /// the outcome-blind subjob resolve — which completes a parent step
    /// on ANY close of a child naming `parent_step_id` — finds no parent
    /// to complete when this gate-run closes, because the edge is not
    /// spelled that way.
    #[tokio::test]
    async fn a_red_follows_its_edge_and_leaves_the_gate_ready_not_completed() {
        let s = car_held_by(G1, H1).await;
        gate_run_filed_with(&s, G1, None, edge_to(&s)).await;
        verdict_of(&s, G1, "failed").await;
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Ready, "{step:?}");
        assert_eq!(step.metadata[car::LAST_VERDICT][car::GATE_RUN], G1);
        assert_eq!(step.metadata[car::LAST_VERDICT]["verdict"], "failed");
        assert!(
            step.metadata.get("receipt").is_none(),
            "{:?}",
            step.metadata
        );
        assert_eq!(cars_on_branch(&s).await, 1, "one car, no twin");

        // The gate-run closes on its red; the subjob rule's handler, run
        // on that close, finds no parent edge of its own and writes
        // nothing.
        crate::handlers::jobs_subjob_resolve::JobsSubjobResolve::new(&s.base)
            .invoke(
                &[],
                &InvocationContext {
                    rule_name: "resolve-subjob-on-child-job-closed".into(),
                    triggering_event_id: "evt-g1-closed".into(),
                    triggering_topic: "jobs.job.closed".into(),
                    event_timestamp: Some(chrono::Utc::now()),
                    event_payload: json!({"id": G1, "outcome": "failed",
                        "parent_step_id": null}),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            gate_step(&s).await.status,
            boss_core::job::StepStatus::Ready
        );
    }

    /// AN EDGE NEVER FINISHES A GATE ITS RUN DOES NOT HOLD. G1's edge
    /// names the car, but G2 relaunched the gate since: G1's green moves
    /// nothing, says why on G1's gate-run, and files no second car; G1's
    /// red hands nothing back from under G2.
    #[tokio::test]
    async fn an_edge_to_a_gate_another_run_holds_moves_nothing() {
        let s = car_held_by(G2, H2).await;
        gate_run_filed_with(&s, G1, None, edge_to(&s)).await;
        green_of(&s, G1).await;
        assert_eq!(run_metadata(&s, G1).await["park_skipped"], "edge-refused");
        held_by_g2_untouched(&gate_step(&s).await);
        verdict_of(&s, G1, "failed").await;
        held_by_g2_untouched(&gate_step(&s).await);
        assert_eq!(cars_on_branch(&s).await, 1);
    }

    /// THE CUTOVER, NEW DISPATCHER: a gate-run launched before this car
    /// deployed carries no edge, and its verdict still finds the car by
    /// the scan — a green finishes it, a red hands it back.
    #[tokio::test]
    async fn an_edge_less_gate_run_is_still_finished_or_handed_back_by_the_scan() {
        let s = car_held_by(G1, H1).await;
        gate_run_filed(&s, G1, None).await;
        verdict_of(&s, G1, "failed").await;
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Ready, "{step:?}");

        let s = car_held_by(G1, H1).await;
        gate_run_filed(&s, G1, None).await;
        green_of(&s, G1).await;
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Completed);
        assert_eq!(cars_on_branch(&s).await, 1);
    }

    /// A LOST GATE'S RELAUNCH FINISHES ITS CAR (backlog 64d417ca).
    /// Measured 2026-11-10 on car 88645b17: gate-run cdd2f3d9 was LOST,
    /// its red handed the car's gate step back READY still naming
    /// cdd2f3d9, and the conductor's relaunch fc5d6d2e — carrying
    /// `relaunch_of: cdd2f3d9` — went green on the same head. This
    /// handler recorded `gate-held-elsewhere` and moved nothing, so an
    /// urgent fix sat about 20 hours off the dock with a valid green, and
    /// the hand step that followed (`boss rerail --finish`) made it look
    /// parked without moving it. Through the real router and the
    /// handler's own invoke: a green that is NOT the lost run's relaunch
    /// still moves nothing (the control), and the relaunch's green
    /// completes the gate step with its receipt, with no hand step.
    #[tokio::test]
    async fn a_lost_gates_relaunch_that_goes_green_finishes_its_car() {
        const STRANGER: &str = "3a1d2e3f-4a5b-4c6d-8e7f-000000000003";
        let s = car_with(
            boss_core::job::StepStatus::Completed,
            boss_core::job::StepStatus::Ready,
            json!({
                car::GATE_RUN: G1, "gated_head": H1,
                car::LAST_VERDICT: {car::GATE_RUN: G1, "verdict": "lost"},
            }),
        )
        .await;
        let skipped = |id: &str| {
            let jobs = s.jobs.clone();
            let id = boss_core::job::JobId::from_uuid(uuid::Uuid::parse_str(id).unwrap());
            async move { jobs.get_job(&id).await.unwrap().unwrap().metadata["park_skipped"].clone() }
        };

        // The control: a hand gate on the same head is not the lost run's
        // relaunch, and the binding still refuses it.
        gate_run_filed(&s, STRANGER, None).await;
        green_of(&s, STRANGER).await;
        assert_eq!(skipped(STRANGER).await, "gate-held-elsewhere");
        let step = gate_step(&s).await;
        assert_eq!(step.status, boss_core::job::StepStatus::Ready, "{step:?}");

        // The relaunch: its green finishes the gate the lost run held.
        gate_run_filed(&s, G2, Some(G1)).await;
        green_of(&s, G2).await;
        assert_eq!(
            skipped(G2).await,
            Value::Null,
            "the relaunch's green is not skipped"
        );
        let step = gate_step(&s).await;
        assert_eq!(
            step.status,
            boss_core::job::StepStatus::Completed,
            "{:?}",
            step.metadata
        );
        let receipt = step.metadata["receipt"].as_str().unwrap_or_default();
        assert!(receipt.contains(H1), "{:?}", step.metadata);
        // The step names the run whose receipt it carries, and keeps the
        // run it took over from (backlog 84e46932): the receipt names no
        // gate-run, so a step still naming G1 sent every reader of the
        // car to a LOST run and hid the green one behind a search.
        assert_eq!(step.metadata[car::GATE_RUN], G2, "{:?}", step.metadata);
        assert_eq!(step.metadata[car::LOST_GATE_RUN], G1, "{:?}", step.metadata);
    }

    /// R2 through the handler's own invoke: G1's green, with G2 holding
    /// the car's gate step, writes ONE thing — the skip on G1's gate-run,
    /// naming the holder — and nothing on the car.
    #[tokio::test]
    async fn a_green_whose_gate_another_run_holds_records_a_skip_and_moves_no_car() {
        use axum::{
            Json, Router,
            extract::Request,
            http::{Method, StatusCode},
            response::IntoResponse,
        };
        let car_id = "be025b44-0000-0000-0000-0000000000a2";
        let held = json!({
            "id": car_id, "kind": "ship-a-change", "status": "open",
            "subject": {"subject_kind": "custom", "id": BRANCH},
            "metadata": {"branch": BRANCH, car::ADMITTED_BY: "boss dispatch --started"},
            "steps": [
                {"id": "s-scope", "spec_slug": "scope", "title": car::SCOPE, "status": "completed"},
                {"id": "s-build", "spec_slug": "build", "title": car::BUILD, "status": "completed"},
                {"id": "s-gate", "spec_slug": "gate", "title": car::GATE, "status": "active",
                 "metadata": {car::GATE_RUN: G2, "gated_head": H2}},
                {"id": "s-review", "spec_slug": "review", "title": car::REVIEW, "status": "pending"},
            ],
        });
        let gate_run = json!({
            "id": G1, "kind": "gate-run",
            "metadata": {
                "branch": BRANCH, "sha": H1,
                "park_summary": "s", "park_excludes": "e", "park_test": "t",
                "park_verified": "v",
                "park_probe": "echo ok", "park_expect": "ok",
                "park_partial_item": "b8e75382-0000-0000-0000-000000000000",
            },
        });
        type Writes = Arc<std::sync::Mutex<Vec<(String, String, Value)>>>;
        let writes: Writes = Arc::default();
        let retained = writes.clone();
        let app = Router::new().fallback(move |request: Request| {
            let (held, gate_run, writes) = (held.clone(), gate_run.clone(), retained.clone());
            async move {
                let method = request.method().clone();
                let path = request.uri().path().to_string();
                let query = request.uri().query().unwrap_or_default().to_string();
                if method == Method::GET {
                    return if path == format!("/api/jobs/{G1}") {
                        Json(gate_run).into_response()
                    } else if path == format!("/api/jobs/{car_id}") {
                        Json(held).into_response()
                    } else if query.contains("status=open") {
                        Json(json!({"data": [held], "total": 1})).into_response()
                    } else {
                        Json(json!({"data": [], "total": 0})).into_response()
                    };
                }
                let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
                    .await
                    .unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                writes
                    .lock()
                    .unwrap()
                    .push((method.to_string(), path, body));
                StatusCode::NO_CONTENT.into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let receipt = format!("{{\"verdict\":\"green\",\"head\":\"{H1}\",\"mode\":\"full\"}}");
        let ctx = InvocationContext {
            rule_name: "auto-park-on-gate-green".into(),
            triggering_event_id: "evt-g1-green".into(),
            triggering_topic: "step.done.gate-verdict".into(),
            event_timestamp: None,
            event_payload: json!({"job_id": G1, "step_id": "verdict", "kind": "gate-verdict",
                "metadata": {"verdict": "green", "receipt": receipt}}),
        };
        let result = handler(&base).invoke(&[], &ctx).await;
        server.abort();
        result.unwrap();
        let writes = writes.lock().unwrap().clone();
        assert_eq!(writes.len(), 1, "{writes:#?}");
        let (method, path, body) = &writes[0];
        assert_eq!(
            (method.as_str(), path.as_str()),
            ("PATCH", format!("/api/jobs/{G1}/metadata").as_str())
        );
        assert_eq!(body["park_skipped"], "gate-held-elsewhere");
        let note = body["park_skipped_note"].as_str().unwrap();
        assert!(note.contains("held by gate-run 2a1d2e3f"), "{note}");
    }
}
