//! `jobs.release_on_review` — a held car's reviewer verdict releases it
//! through the release door (design 393bdef6 Q1, David 2026-10-16;
//! backlog 0cfc57e6).
//!
//! WHY. A car held for its adversarial review waited for a person to
//! copy its reviewer's RELEASE verdict into `boss release --review`.
//! Measured 2026-10-09..16: 156 releases by hand (agent-codex 105,
//! emp-david 40, agent-claude 9, other 2), about 106 h of delay in
//! total; verdict to release took agents a median 0.9 min and David a
//! median 34.5 min, p90 8.7 h. No hold class was released only by a
//! person, and every check the release rests on is a read of the record.
//! So the record's own verdict now moves the car.
//!
//! THE DOOR DECIDES, NEVER THIS HANDLER. Fired on `jobs.job.updated` of an
//! agent-run whose `review` names a car (`boss review` writes it through
//! the job merge door), this handler asks `POST /api/jobs/{car}/release`
//! with `{review: <the run>}` and nothing else, signed as the rule. The
//! door runs `boss_jobs::car_release::judge` — the one rule `boss release`
//! and the yard's Release control ask — at the head the hold was placed
//! at (no head is sent, as the yard sends none; the conductor re-reads
//! the live forge head at boarding and holds again a release at another
//! head). A verdict for a head the hold no longer names, a CHANGES
//! verdict, a run with no verdict, and a car reserved for a person's own
//! release (`releaser`, written by `boss hold --releaser`) are all the
//! judge's refusals, and the judge stamps the release `by` the rule's
//! recorded actor, `automation:rule:<name>`, so `completed_by` and `by`
//! name the rule and never a person or an agent.
//!
//! WHAT IT DECIDES ITSELF is only whether there is anything to ask, and
//! each answer leaves the car exactly as it was:
//!
//! - no car named, the car gone, its review step absent or closed, or no
//!   hold on it: nothing is held, so nothing is asked and nothing written.
//!   This is also what makes a later write to the same run (its report,
//!   its age-out) a no-op once the car is released.
//! - the run is not the reviewer the car's review step was dispatched to
//!   (`car_release::dispatched_reviewer`, the run the yard's row names):
//!   the rule speaks only for the independent reviewer the protocol sent,
//!   so any other verdict is left to a person. Recorded, like a refusal.
//! - the verdict was not recorded AFTER the hold now on the car was
//!   placed, or either time cannot be read (review 3af8f7e2 N2). The rule
//!   fires on ANY write to the run — its report, its age-out, a backlog
//!   after an outage — and a person's `boss hold` over a live review hold
//!   keeps `hold_sha`, so the door alone would lift that newer hold on
//!   the older verdict. The verdict's time is its own `review.at`; the
//!   hold's is the car's history (`GET /api/jobs/{car}/events`): the
//!   last write of the review step at which `hold` took its current
//!   value. A hold rewritten with the same text is the same hold to the
//!   record. Recorded, like a refusal.
//!
//! TWO TRIGGERS, ONE ROAD (backlog 60993cda). The verdict's own write
//! fired the rule only while the car was held at the head it read, so a
//! verdict given BEFORE its hold — a reviewer reading a repair's head
//! before the repair's gate finished, the car then re-held there by the
//! green — was never asked again, and a person released the car on that
//! same verdict (measured 2026-10-18: 4 such holds, 3 released by hand
//! on that verdict, cars 048df940, 4a19d6a9 and c1c19d5e, 20 s to 8.5
//! min after the hold). So a write of a held review step
//! (`jobs.step.updated`, rule `release-a-held-car-when-its-review-hold-
//! is-placed`) wakes the handler too: it reads the reviewer the step
//! names and, when that run has a verdict on this car, takes exactly the
//! road above. The hold wake records nothing when there is no verdict to
//! ask about. The one thing it widens is the time check, and only for
//! the write that bound the hold to the head the verdict read (see
//! [`not_after_the_hold`]); the door's judge is unchanged.
//!
//! A REFUSAL IS RECORDED WHERE THE HOLD IS. The door's 409 sentence is
//! written onto the car's review step as `auto_release` (`{review,
//! outcome, why, by, at}`, `boss_jobs::car::AUTO_RELEASE`), beside the
//! `hold` every reader of a held car already reads — a rule that declines
//! in silence is the failure the correctness protocol forbids. The same
//! refusal by the same review is written once: a redelivery, or a later
//! write to the run, finds it already there and writes nothing.
//!
//! THE HOLD JUDGED IS THE HOLD ASKED ABOUT (review 792b871a N1). The
//! placement check above is a judgement of the car as this handler READ
//! it, and the door reads the car again at the POST; a person's `boss
//! hold` landing in between keeps `hold_sha`, so the door judged that
//! new hold good on the old verdict and lifted it. The ask now carries
//! the hold this firing judged (`car_release::JudgedHold`, text and
//! sha), and the door lifts that hold or refuses.
//!
//! A 409 IS RECORDED ONLY WHERE A HOLD STILL IS (review 792b871a N2).
//! Two firings for one verdict both read the car held, and the second's
//! 409 meets the first's release. The car is re-read on a 409: nothing
//! held means the release is done, and nothing is recorded beside it.
//!
//! Any other answer from the door is an error. One that refuses for good
//! (a 4xx but 408/429: 401, 403, 404, 422) is recorded beside the hold
//! once, with its reason, AND returned Permanent, dead-lettered and
//! counted (review 792b871a N3: a door 404 was retried, and no reason
//! reached the car). The rest are retried and write nothing. A history
//! read refused for good is recorded the same way.

use super::common::{
    dispatcher_actor_header, dispatcher_reader_header, rows_or_refuse, sim_origin_value,
};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::car_release;
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};
use std::sync::Arc;

pub struct JobsReleaseOnReview {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsReleaseOnReview {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            jobs_base: jobs_base.into(),
        })
    }
}

fn str_at<'a>(v: &'a Json, pointer: &str) -> &'a str {
    v.pointer(pointer)
        .and_then(Json::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// The most rows `GET /api/jobs/{id}/events` serves (its `limit` is
/// clamped to 1..=1000): a window this full may start after the hold.
pub(crate) const EVENTS_LIMIT: usize = 1000;

/// PURE: the review step to ask about, when `car` holds anything a
/// release could lift — an open review step carrying a hold. `None` is
/// "nothing is held": no ask, no record.
pub(crate) fn held_review(car: &Json) -> Option<&Json> {
    let step = car_release::review_step(car)?;
    let open = matches!(str_at(step, "/status"), "ready" | "active");
    let held = boss_jobs::stranded::hold_reason(step.get("metadata").unwrap_or(&Json::Null));
    (open && held.is_some()).then_some(step)
}

/// PURE: why the rule does not speak for `run` on `car`, when it does not:
/// the run is not the reviewer the car's review step was dispatched to.
pub(crate) fn not_the_dispatched_reviewer(car: &Json, run: &str) -> Option<String> {
    match car_release::dispatched_reviewer(car) {
        Some(r) if r.eq_ignore_ascii_case(run) => None,
        Some(r) => Some(format!(
            "run {} is not the reviewer car {}'s review step was dispatched to ({}); the release \
             rule releases only on the dispatched reviewer's verdict, so this one is left to a \
             person: boss release <car> --review {run}",
            short(run),
            short(str_at(car, "/id")),
            short(r)
        )),
        None => Some(format!(
            "car {}'s review step names no dispatched reviewer, so the release rule speaks for no \
             verdict on it; run {}'s is left to a person: boss release <car> --review {run}",
            short(str_at(car, "/id")),
            short(run)
        )),
    }
}

/// When the hold a review step carries now was placed, and the head that
/// SAME write bound it to, when it bound one ([`hold_placement`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Placement {
    pub at: DateTime<Utc>,
    /// `hold_sha` as the placing write left it, when that write is the one
    /// that gave the step this head (it differs from the write before, or
    /// the write is the first of the whole history) and it is still the
    /// step's head. A review hold's writers put `hold` and `hold_sha` in
    /// one body (the auto-park green, the rerail, the conductor's
    /// re-judge); a hand `boss hold` writes `hold` alone and binds none
    /// (backlog 60993cda).
    pub binds: Option<String>,
}

/// PURE: when the hold the review step carries NOW was placed, read off
/// the car's own history (`GET /api/jobs/{car}/events`, oldest first,
/// each step write carrying the post-write row): the last write of `step`
/// at which its `hold` took the value it holds now, and the head that
/// write bound ([`Placement::binds`]). `Err` says why that cannot be read
/// — no write of the step in the window, a window full enough to start
/// after the placing, or a history whose last write disagrees with the
/// car the handler read.
///
/// The record cannot tell a hold rewritten with the SAME text from the
/// hold it replaced, so "placed" means "took its current reason".
pub(crate) fn hold_placement(events: &Json, step: &Json) -> Result<Placement, String> {
    let step_id = str_at(step, "/id");
    let rows: Vec<Json> = rows_or_refuse(events, "the car's history")?;
    let full = rows.len() >= EVENTS_LIMIT;
    // (hold, hold_sha, the event) for each write of the step, oldest first.
    let writes: Vec<(Option<&Json>, Option<&Json>, &Json)> = rows
        .iter()
        .filter(|e| str_at(e, "/payload/step_id") == step_id)
        .filter_map(|e| {
            e.pointer("/payload/metadata")
                .map(|md| (md.get("hold"), md.get(boss_jobs::car::HOLD_SHA), e))
        })
        .collect();
    let now = step.pointer("/metadata/hold");
    if writes.last().map(|w| w.0) != Some(now) {
        return Err(format!(
            "the car's history carries no write of review step {} holding what it holds now",
            short(step_id)
        ));
    }
    // The first write of the run of writes that hold what it holds now.
    let i = writes.iter().rposition(|w| w.0 != now).map_or(0, |k| k + 1);
    // The window's first write is the placing only when the window is
    // the whole history; a full window may start after it.
    if i == 0 && full {
        return Err(format!(
            "the car's history is a full window of {EVENTS_LIMIT} rows that starts after review \
             step {}'s hold took its reason",
            short(step_id)
        ));
    }
    let (_, sha, e) = writes[i];
    let before = i.checked_sub(1).and_then(|k| writes[k].1);
    let head = str_at(step, &format!("/metadata/{}", boss_jobs::car::HOLD_SHA));
    let binds = sha
        .and_then(Json::as_str)
        .filter(|s| !s.trim().is_empty() && s.trim() == head)
        .filter(|_| sha != before)
        .map(|s| s.trim().to_string());
    let placed = str_at(e, "/at");
    DateTime::parse_from_rfc3339(placed)
        .map(|t| Placement {
            at: t.with_timezone(&Utc),
            binds,
        })
        .map_err(|e| format!("the write that placed the hold is dated {placed:?}: {e}"))
}

/// PURE: why the rule does not ask the door on this verdict, when it does
/// not: the verdict was not recorded AFTER the hold the car carries now
/// was placed (review 3af8f7e2 N2). The rule fires on any write to the
/// reviewer's run — its report, its age-out, a backlog after an outage —
/// and a person's `boss hold` over a live review hold keeps `hold_sha`,
/// so without this the door would lift a NEW hold on an OLD verdict.
/// Either time unreadable is a reason too: nothing is asked on a guess.
///
/// ONE VERDICT BEFORE ITS HOLD IS THAT HOLD'S (backlog 60993cda): when
/// the write that placed the hold also bound it to the very head the
/// verdict read ([`Placement::binds`] == `reviewed_sha`). That is a
/// review hold for exactly that head — a gate's green holding the car
/// for the review of what it gated — and the reviewer had already read
/// it: measured 2026-10-18, 4 of the 21 verdicts that preceded a hold at
/// their head, 3 of them then released by a person on that same verdict
/// (cars 048df940, 4a19d6a9, c1c19d5e). A hand hold and a red-train hold
/// bind no head, and a re-hold at the head already held binds no NEW one
/// — the other 17 — so none of them is ever that.
///
/// `run` is the reviewer's agent-run as the jobs API serves it.
pub(crate) fn not_after_the_hold(run: &Json, events: &Json, step: &Json) -> Option<String> {
    let said = str_at(run, &format!("/metadata/{}/at", car_release::REVIEW_KEY));
    let verdict = match DateTime::parse_from_rfc3339(said) {
        Ok(t) => t.with_timezone(&Utc),
        Err(e) => {
            return Some(format!(
                "the verdict's recorded time cannot be read ({said:?}: {e}), so the release rule \
                 cannot tell it was recorded after the hold now on the car; left to a person: \
                 boss release <car> --review <run>"
            ));
        }
    };
    let placement = match hold_placement(events, step) {
        Ok(p) => p,
        Err(why) => {
            return Some(format!(
                "the time the hold now on the car was placed cannot be read ({why}), so the \
                 release rule cannot tell the verdict came after it; left to a person: boss \
                 release <car> --review <run>"
            ));
        }
    };
    let read = str_at(
        run,
        &format!("/metadata/{}/reviewed_sha", car_release::REVIEW_KEY),
    );
    if !read.is_empty() && placement.binds.as_deref() == Some(read) {
        return None;
    }
    let placed = placement.at;
    (verdict <= placed).then(|| {
        format!(
            "the verdict was recorded at {said}, not after the hold now on the car was placed \
             ({}), so it never judged that hold and the release rule does not lift it; a person \
             releases it: boss release <car> --review <run>",
            placed.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
        )
    })
}

/// PURE: the merge body that records why the rule left the car, or `None`
/// when the review step already records this same refusal by this same
/// review (a redelivery or a later write to the run writes nothing).
pub(crate) fn refusal_record(
    review_md: &Json,
    review: &str,
    why: &str,
    by: &str,
    at: Option<&str>,
) -> Option<Json> {
    let key = boss_jobs::car::AUTO_RELEASE;
    let prior = review_md.get(key);
    if prior.is_some_and(|p| str_at(p, "/review") == review && str_at(p, "/why") == why.trim()) {
        return None;
    }
    Some(json!({ key: {
        "review": review,
        "outcome": "refused",
        "why": why.trim(),
        "by": by,
        "at": at,
    }}))
}

impl JobsReleaseOnReview {
    async fn record_refusal(
        &self,
        ctx: &InvocationContext,
        car: &Json,
        step: &Json,
        review: &str,
        why: &str,
    ) -> Result<(), HandlerError> {
        let md = step.get("metadata").unwrap_or(&Json::Null);
        let by = format!(
            "{}{}",
            boss_dispatcher::rules::authoring::RULE_FAMILY,
            ctx.rule_name
        );
        let at = ctx.firing_instant().ok().map(|t| t.to_rfc3339());
        let Some(body) = refusal_record(md, review, why, &by, at.as_deref()) else {
            return Ok(());
        };
        let url = format!(
            "{}/api/jobs/{}/steps/{}/metadata",
            self.jobs_base.trim_end_matches('/'),
            str_at(car, "/id"),
            str_at(step, "/id")
        );
        let resp = self
            .client
            .patch(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PATCH {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(answered(
                status,
                format!("PATCH {url} returned {status}: {text}"),
            ));
        }
        tracing::info!(rule = %ctx.rule_name, car = %str_at(car, "/id"), review, why,
            "jobs.release_on_review: left the car held, and recorded why on its review step");
        Ok(())
    }

    /// A refusal no redelivery can change (review 792b871a N3): written
    /// beside the hold once, like any refusal, and still an error, so the
    /// firing is dead-lettered and counted where every failed one is.
    /// When the record itself cannot be written, both failures are said.
    async fn record_permanent(
        &self,
        ctx: &InvocationContext,
        car: &Json,
        step: &Json,
        review: &str,
        why: String,
    ) -> HandlerError {
        match self.record_refusal(ctx, car, step, review, &why).await {
            Ok(()) => HandlerError::Permanent(why),
            Err(e) => HandlerError::Permanent(format!(
                "{why}; and the refusal could not be recorded on the car: {e}"
            )),
        }
    }
}

/// PURE: does an HTTP answer refuse for good? A 4xx says the request
/// itself will not be granted however often it is sent — except 408 and
/// 429, which ask for it again later. A 5xx is the server's, and may
/// pass (review 792b871a N3: a door 404 was retried).
pub(crate) fn refuses_for_good(status: reqwest::StatusCode) -> bool {
    status.is_client_error()
        && status != reqwest::StatusCode::REQUEST_TIMEOUT
        && status != reqwest::StatusCode::TOO_MANY_REQUESTS
}

/// The error an unsuccessful answer is: permanent when it refuses for
/// good ([`refuses_for_good`]), retried otherwise.
fn answered(status: reqwest::StatusCode, said: String) -> HandlerError {
    if refuses_for_good(status) {
        HandlerError::Permanent(said)
    } else {
        HandlerError::Downstream(said)
    }
}

#[async_trait]
impl Handler for JobsReleaseOnReview {
    fn name(&self) -> &'static str {
        "jobs.release_on_review"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let p = &ctx.event_payload;
        if ctx.triggering_topic == HOLD_TOPIC {
            return self.on_review_step_write(ctx).await;
        }
        let run = str_at(p, "/id");
        let car_id = str_at(p, &format!("/metadata/{}/car", car_release::REVIEW_KEY));
        if str_at(p, "/kind") != "agent-run" || run.is_empty() || car_id.is_empty() {
            // No run, or a run whose review names no car: no verdict
            // names anything this rule could release.
            return Ok(());
        }
        let Some(car) = self.read(&format!("/api/jobs/{car_id}")).await? else {
            return Ok(());
        };
        self.ask(ctx, &car, p).await
    }
}

/// The topic of the SECOND trigger, `release-a-held-car-when-its-review-
/// hold-is-placed` (backlog 60993cda): a write of a car's review step,
/// whose payload is the step row rather than the reviewer's run.
pub(crate) const HOLD_TOPIC: &str = "jobs.step.updated";

impl JobsReleaseOnReview {
    /// `GET {jobs}{path}` as the dispatcher's reader: the JSON, or `None`
    /// for a 404 (the packet is gone). Anything else not 2xx is the error
    /// [`answered`] makes of it: permanent when it refuses for good,
    /// retried otherwise (review 792b871a N3).
    async fn read(&self, path: &str) -> Result<Option<Json>, HandlerError> {
        let url = format!("{}{path}", self.jobs_base.trim_end_matches('/'));
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(answered(
                status,
                format!("GET {url} returned {status}: {text}"),
            ));
        }
        resp.json()
            .await
            .map(Some)
            .map_err(|e| HandlerError::Downstream(format!("GET {url} is not JSON: {e}")))
    }

    /// THE HOLD WAKE (backlog 60993cda). A verdict the dispatched reviewer
    /// gave BEFORE the hold was placed fired the rule while there was no
    /// hold, or a hold at another head, and nothing asked again: the car
    /// waited for a person, who released it on that same verdict. So a
    /// write of a held review step also wakes the rule, which reads the
    /// reviewer the step names and, when that run has a verdict on THIS
    /// car, asks exactly as the verdict's own write does. No verdict yet,
    /// or one on another car: nothing to ask and nothing recorded — the
    /// verdict's own write will fire the rule when it lands.
    async fn on_review_step_write(&self, ctx: &InvocationContext) -> Result<(), HandlerError> {
        let p = &ctx.event_payload;
        let car_id = str_at(p, "/job_id");
        if car_id.is_empty() {
            return Ok(());
        }
        let Some(car) = self.read(&format!("/api/jobs/{car_id}")).await? else {
            return Ok(());
        };
        let Some(step) = held_review(&car) else {
            return Ok(());
        };
        // Only the car's own review step speaks for it; another step's
        // slug is not this car's hold.
        if str_at(step, "/id") != str_at(p, "/step_id") {
            return Ok(());
        }
        let Some(reviewer) = car_release::dispatched_reviewer(&car) else {
            return Ok(());
        };
        let Some(run) = self.read(&format!("/api/jobs/{reviewer}")).await? else {
            return Ok(());
        };
        let names = str_at(&run, &format!("/metadata/{}/car", car_release::REVIEW_KEY));
        if !names.eq_ignore_ascii_case(str_at(&car, "/id")) {
            return Ok(());
        }
        self.ask(ctx, &car, &run).await
    }

    /// Ask the release door about `car` on `run`'s verdict, or record why
    /// the rule does not — the one road both triggers take.
    async fn ask(
        &self,
        ctx: &InvocationContext,
        car: &Json,
        run: &Json,
    ) -> Result<(), HandlerError> {
        let Some(step) = held_review(car) else {
            return Ok(());
        };
        let car_id = str_at(car, "/id");
        let review = str_at(run, "/id");
        if let Some(why) = not_the_dispatched_reviewer(car, review) {
            return self.record_refusal(ctx, car, step, review, &why).await;
        }
        // The verdict must postdate the hold it would lift (review
        // 3af8f7e2 N2), read off the car's own history.
        let base = self.jobs_base.trim_end_matches('/');
        let events_url = format!("{base}/api/jobs/{car_id}/events?limit={EVENTS_LIMIT}");
        let resp = self
            .client
            .get(&events_url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {events_url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let said = format!(
                "the car's history could not be read (GET {events_url} returned {status}: \
                 {text}), so the release rule cannot tell the verdict came after the hold"
            );
            if refuses_for_good(status) {
                return Err(self.record_permanent(ctx, car, step, review, said).await);
            }
            return Err(HandlerError::Downstream(said));
        }
        let events: Json = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {events_url} is not JSON: {e}")))?;
        if let Some(why) = not_after_the_hold(run, &events, step) {
            return self.record_refusal(ctx, car, step, review, &why).await;
        }
        let run = review;

        // The ask names the hold this firing judged, as it read the car
        // (review 792b871a N1): the door lifts that hold or nothing, so a
        // hold rewritten since this read — a person's `boss hold` keeps
        // `hold_sha` — is refused rather than lifted on this verdict.
        let url = format!("{base}/api/jobs/{car_id}/release");
        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&json!({ "review": run, "hold": car_release::JudgedHold::of(car) }))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.is_success() {
            tracing::info!(rule = %ctx.rule_name, car = car_id, review = run,
                "jobs.release_on_review: released the car on its reviewer's verdict");
            return Ok(());
        }
        if status == reqwest::StatusCode::CONFLICT {
            // A REFUSAL IS RECORDED WHERE THE HOLD IS — so read where it
            // is now (review 792b871a N2). Two firings for one verdict
            // both read the car held — and two wakes for one verdict, the
            // verdict's write and the hold's (backlog 60993cda); the
            // loser's 409 finds the winner's release, and nothing holds
            // the car to record it beside.
            let Some(now) = self.read(&format!("/api/jobs/{car_id}")).await? else {
                return Ok(());
            };
            let Some(step) = held_review(&now) else {
                tracing::info!(rule = %ctx.rule_name, car = car_id, review = run,
                    "jobs.release_on_review: the door refused, and nothing holds the car now \
                     (a racing firing released it); nothing to record");
                return Ok(());
            };
            return self.record_refusal(ctx, &now, step, run, &text).await;
        }
        let said = format!("the release door answered {status}: {text}");
        if refuses_for_good(status) {
            return Err(self.record_permanent(ctx, car, step, run, said).await);
        }
        Err(HandlerError::Downstream(format!("POST {url}: {said}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Bytes;
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use axum::response::IntoResponse;
    use std::sync::Mutex;

    const RULE: &str = "release-a-held-car-on-its-reviewer-verdict";
    const CAR: &str = "c0ffee00-0000-4000-8000-000000000000";
    const BUILDER: &str = "b0b0b0b0-0000-4000-8000-000000000000";
    const REVIEWER: &str = "5eed5eed-0000-4000-8000-000000000000";
    const OTHER: &str = "0ther000-0000-4000-8000-000000000000";
    const STEP: &str = "00000000-0000-4000-8000-000000000003";
    const H1: &str = "1111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222";

    fn held_car(extra: Json) -> Json {
        let mut md = json!({"hold": "trust-boundary car (area policy)", "hold_sha": H1,
                            "agent_run": REVIEWER});
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            md[k] = v;
        }
        json!({"id": CAR, "kind": "ship-a-change", "workflow_version": 7,
               "metadata": {"branch": "feat/x", "agent_run": BUILDER},
               "steps": [{"id": STEP, "spec_slug": "review",
                          "title": boss_jobs::car::REVIEW, "status": "active",
                          "metadata": md}]})
    }

    fn run(id: &str, sha: &str, verdict: &str) -> Json {
        json!({"id": id, "kind": "agent-run", "status": "open",
               "metadata": {"review": {"car": CAR, "branch": "feat/x",
                   "reviewed_sha": sha, "verdict": verdict, "findings": "",
                   "at": "2026-10-16T19:00:00Z"}}})
    }

    /// Executor provenance is advisory, while reviewed source is an
    /// original first record, as the real release door now requires.
    struct Advisory {
        source: Option<boss_jobs::first_record::FirstRecord>,
    }

    #[async_trait::async_trait]
    impl car_release::ExecutorRecordReader for Advisory {
        async fn protocol_version(&self, kind: &str, version: i64) -> anyhow::Result<Option<Json>> {
            Ok(Some(
                json!({"kind": kind, "version": version, "status": "active",
                           "steps": [{"title": "review"}]}),
            ))
        }
        async fn executor_original(
            &self,
            job: uuid::Uuid,
            step: uuid::Uuid,
            key: &str,
        ) -> anyhow::Result<Option<boss_jobs::first_record::FirstRecord>> {
            Ok(self
                .source
                .as_ref()
                .filter(|record| {
                    record.job_id == boss_core::job::JobId::from_uuid(job)
                        && record.step_id == boss_core::job::StepId::from_uuid(step)
                        && key == boss_jobs::release_window::SOURCE_KEY
                })
                .cloned())
        }
    }

    fn attested_source(mut run: Json) -> (Json, Advisory) {
        let Some(id) = run["id"].as_str().and_then(|id| id.parse().ok()) else {
            return (run, Advisory { source: None });
        };
        let building = uuid::Uuid::from_u128(79);
        run["steps"] = json!([{"id":building,"spec_slug":"building"}]);
        let manifest =
            "kind: ConfigMap\nmetadata:\n  name: boss-machine-gate\ndata:\n  mode: report\n";
        let record = boss_jobs::first_record::FirstRecord::new(
            boss_core::job::JobId::from_uuid(id),
            boss_core::job::StepId::from_uuid(building),
            boss_jobs::release_window::SOURCE_KEY,
            &json!({"car":CAR,"head":run["metadata"]["review"]["reviewed_sha"],
                "main":H1,"base":H1,"path":boss_jobs::release_window::MANIFEST_PATH,
                "mode":"100644","base_mode":"100644","base_manifest":manifest,
                "manifest":manifest,"diff":""}),
            &boss_core::publisher::EventStamp::new(
                "test",
                boss_core::actor::ActorId::automation("review"),
            ),
            uuid::Uuid::from_u128(78),
        );
        if run.pointer("/metadata/review").is_some_and(Json::is_object) {
            run["metadata"]["review"]["source_binding"] =
                json!({"digest":record.digest,"event_id":record.event_id});
        }
        (
            run,
            Advisory {
                source: Some(record),
            },
        )
    }

    /// The jobs API's three answers this handler needs, with the release
    /// door played by the REAL judge: `GET /api/jobs/{car}`, `POST
    /// /api/jobs/{car}/release` (judged as `http/release.rs` judges it,
    /// the caller's `by` its ambient actor, and the granted merge body
    /// applied to the car), and the step merge door. Every request is
    /// logged as `METHOD /path` with its body and its `x-boss-user` id.
    struct Api {
        base: String,
        car: Arc<Mutex<Json>>,
        log: Arc<Mutex<Vec<(String, Json, String)>>>,
    }

    impl Api {
        fn requests(&self) -> Vec<String> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.0.clone())
                .collect()
        }
        fn sent(&self, req: &str) -> Vec<(Json, String)> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.0 == req)
                .map(|r| (r.1.clone(), r.2.clone()))
                .collect()
        }
        fn review_md(&self) -> Json {
            self.car.lock().unwrap()["steps"][0]["metadata"].clone()
        }
    }

    fn merge(md: &mut Json, patch: &Json) {
        for (k, v) in patch.as_object().cloned().unwrap_or_default() {
            if v.is_null() {
                md.as_object_mut().unwrap().remove(&k);
            } else {
                md[k] = v;
            }
        }
    }

    /// One row of `GET /api/jobs/{car}/events`: a write of the review
    /// step at `at`, its payload the post-write row (`step_state_payload`).
    fn step_event(at: &str, md: Json) -> Json {
        json!({"kind": "jobs.step.updated", "at": at, "job_id": CAR, "step_id": STEP,
               "payload": {"step_id": STEP, "id": STEP, "metadata": md}})
    }

    /// The history every fixture car has unless a test says otherwise:
    /// its review hold placed at 18:00, an hour before the verdict.
    fn placed_before_the_verdict(car: &Json) -> Vec<Json> {
        vec![step_event(
            "2026-10-16T18:00:00Z",
            car["steps"][0]["metadata"].clone(),
        )]
    }

    async fn api(car: Json, run: Json, door: Option<StatusCode>) -> Api {
        let events = placed_before_the_verdict(&car);
        api_with(car, run, door, events).await
    }

    async fn api_with(car: Json, run: Json, door: Option<StatusCode>, events: Vec<Json>) -> Api {
        api_racing(car, run, door, events, None).await
    }

    /// [`api_with`], with a write that lands on the review step BETWEEN
    /// the handler's reads and its POST: `race` is merged into the step
    /// as the history read is answered, the gap review 792b871a N1 named
    /// (a person's `boss hold`, or a duplicate firing's release).
    async fn api_racing(
        car: Json,
        run: Json,
        door: Option<StatusCode>,
        events: Vec<Json>,
        race: Option<Json>,
    ) -> Api {
        let (run, source) = attested_source(run);
        let source = Arc::new(source);
        let events = Arc::new(json!({ "data": events }));
        let car = Arc::new(Mutex::new(car));
        let log: Arc<Mutex<Vec<(String, Json, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let race = Arc::new(Mutex::new(race));
        let (c, l, r, h) = (car.clone(), log.clone(), Arc::new(run), events);
        let app = Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let (car, log, run, history) = (c.clone(), l.clone(), r.clone(), h.clone());
                let race = race.clone();
                let source = source.clone();
                async move {
                    let who: Json = headers
                        .get("x-boss-user")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|h| serde_json::from_str(h).ok())
                        .unwrap_or(Json::Null);
                    let id = str_at(&who, "/id").to_string();
                    let parsed: Json = serde_json::from_slice(&body).unwrap_or(Json::Null);
                    let req = format!("{method} {}", uri.path());
                    log.lock()
                        .unwrap()
                        .push((req.clone(), parsed.clone(), id.clone()));
                    let car_path = format!("/api/jobs/{CAR}");
                    if method == Method::GET && uri.path() == car_path {
                        return axum::Json(car.lock().unwrap().clone()).into_response();
                    }
                    // The reviewer's run, as the hold wake reads it.
                    if method == Method::GET
                        && uri.path() == format!("/api/jobs/{}", str_at(&run, "/id"))
                    {
                        return axum::Json((*run).clone()).into_response();
                    }
                    if method == Method::GET && uri.path() == format!("{car_path}/events") {
                        if let Some(write) = race.lock().unwrap().take() {
                            merge(&mut car.lock().unwrap()["steps"][0]["metadata"], &write);
                        }
                        return axum::Json((*history).clone()).into_response();
                    }
                    if method == Method::POST && uri.path() == format!("{car_path}/release") {
                        if let Some(status) = door {
                            return (status, "the door answered so").into_response();
                        }
                        let user: boss_policy_client::User =
                            serde_json::from_value(who).expect("a user");
                        let by = user.ambient_actor().map(|a| a.to_string()).unwrap();
                        let ask: car_release::ReleaseAsk =
                            serde_json::from_value(parsed).expect("a release body");
                        let now = car.lock().unwrap().clone();
                        return match car_release::judge(source.as_ref(), &now, &run, &ask, &by, "t")
                            .await
                        {
                            Ok(g) => {
                                // The write's precondition, as the
                                // adapter judges it under its lock.
                                let md = &mut car.lock().unwrap()["steps"][0]["metadata"];
                                let expect = g.expect.as_object().cloned().unwrap_or_default();
                                if !boss_jobs::step_metadata_write::metadata_still_reads(
                                    md, &expect,
                                ) {
                                    return (StatusCode::CONFLICT, "step changed").into_response();
                                }
                                merge(md, &g.patch);
                                axum::Json(json!({"car": CAR, "head": g.head})).into_response()
                            }
                            Err(why) => (
                                StatusCode::CONFLICT,
                                format!("boss release: REFUSED — {why}"),
                            )
                                .into_response(),
                        };
                    }
                    if method == Method::PATCH
                        && uri.path() == format!("{car_path}/steps/{STEP}/metadata")
                    {
                        merge(&mut car.lock().unwrap()["steps"][0]["metadata"], &parsed);
                        return axum::Json(json!({})).into_response();
                    }
                    StatusCode::NOT_FOUND.into_response()
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Api {
            base: format!("http://{addr}"),
            car,
            log,
        }
    }

    fn handler(api: &Api) -> Arc<JobsReleaseOnReview> {
        Arc::new(JobsReleaseOnReview {
            client: crate::handlers::common::api_client(),
            jobs_base: api.base.clone(),
        })
    }

    fn ctx(payload: Json) -> InvocationContext {
        InvocationContext {
            rule_name: RULE.into(),
            triggering_event_id: "evt-review-1".into(),
            triggering_topic: "jobs.job.updated".into(),
            event_payload: payload,
            event_timestamp: Some("2026-10-16T19:05:00Z".parse().unwrap()),
        }
    }

    const POST_RELEASE: &str = "POST /api/jobs/c0ffee00-0000-4000-8000-000000000000/release";
    const PATCH_REVIEW: &str = "PATCH /api/jobs/c0ffee00-0000-4000-8000-000000000000/steps/00000000-0000-4000-8000-000000000003/metadata";

    async fn fire(api: &Api, payload: Json) -> Result<(), HandlerError> {
        handler(api).invoke(&[], &ctx(payload)).await
    }

    /// The SECOND trigger (backlog 60993cda): a write of the car's review
    /// step, its payload the post-write step row as `jobs.step.updated`
    /// carries it (`step_state_payload`), read off the car as it stands.
    async fn fire_on_hold(api: &Api) -> Result<(), HandlerError> {
        let md = api.review_md();
        let row = json!({"id": STEP, "step_id": STEP, "job_id": CAR, "spec_slug": "review",
                         "kind": "task", "status": "active", "metadata": md});
        let mut c = ctx(row);
        c.triggering_topic = HOLD_TOPIC.into();
        handler(api).invoke(&[], &c).await
    }

    const GET_CAR: &str = "GET /api/jobs/c0ffee00-0000-4000-8000-000000000000";
    const GET_REVIEWER: &str = "GET /api/jobs/5eed5eed-0000-4000-8000-000000000000";

    /// The refusal the door gave, as recorded on the review step.
    fn recorded(api: &Api) -> Json {
        api.review_md()[boss_jobs::car::AUTO_RELEASE].clone()
    }

    fn still_held(api: &Api) {
        let md = api.review_md();
        assert_eq!(
            md["hold"],
            json!("trust-boundary car (area policy)"),
            "{md}"
        );
        assert!(
            md.get(boss_jobs::car::RELEASE).is_none(),
            "no release: {md}"
        );
    }

    /// REFUSAL: a run whose review names no car asks nothing at all.
    #[tokio::test]
    async fn a_run_with_no_verdict_asks_nothing() {
        let api = api(held_car(json!({})), run(REVIEWER, H1, "release"), None).await;
        let bare = json!({"id": REVIEWER, "kind": "agent-run", "metadata": {}});
        fire(&api, bare).await.expect("a no-op");
        assert!(api.requests().is_empty(), "{:?}", api.requests());
        still_held(&api);
    }

    /// REFUSAL: a verdict for a head the hold no longer names. The door's
    /// own sentence is recorded beside the hold, and the hold stays.
    #[tokio::test]
    async fn a_verdict_for_another_head_is_refused_and_recorded() {
        let api = api(held_car(json!({})), run(REVIEWER, H2, "release"), None).await;
        fire(&api, run(REVIEWER, H2, "release"))
            .await
            .expect("recorded");
        still_held(&api);
        let rec = recorded(&api);
        assert_eq!(rec["outcome"], json!("refused"), "{rec}");
        assert_eq!(rec["review"], json!(REVIEWER));
        assert_eq!(rec["by"], json!(format!("automation:rule:{RULE}")));
        assert!(
            str_at(&rec, "/why").contains("a review of another head"),
            "{rec}"
        );
    }

    /// REFUSAL: a CHANGES verdict releases nothing; the reason is
    /// recorded.
    #[tokio::test]
    async fn a_changes_verdict_is_refused_and_recorded() {
        let api = api(held_car(json!({})), run(REVIEWER, H1, "changes"), None).await;
        fire(&api, run(REVIEWER, H1, "changes"))
            .await
            .expect("recorded");
        still_held(&api);
        assert!(
            str_at(&recorded(&api), "/why").contains("not \"release\""),
            "{}",
            recorded(&api)
        );
    }

    /// REFUSAL: the run's verdict was withdrawn between the event and the
    /// door's read: the door reads no verdict, and that is recorded.
    #[tokio::test]
    async fn a_verdict_the_door_no_longer_reads_is_refused_and_recorded() {
        let gone = json!({"id": REVIEWER, "kind": "agent-run", "metadata": {}});
        let api = api(held_car(json!({})), gone, None).await;
        fire(&api, run(REVIEWER, H1, "release"))
            .await
            .expect("recorded");
        still_held(&api);
        assert!(
            str_at(&recorded(&api), "/why").contains("records no review verdict"),
            "{}",
            recorded(&api)
        );
    }

    /// REFUSAL: David's own-release override. `releaser` on the review
    /// step makes the judge refuse the rule; the car waits for him.
    #[tokio::test]
    async fn a_car_reserved_for_david_waits_for_him() {
        let api = api(
            held_car(json!({boss_jobs::car::RELEASER: "emp-david"})),
            run(REVIEWER, H1, "release"),
            None,
        )
        .await;
        fire(&api, run(REVIEWER, H1, "release"))
            .await
            .expect("recorded");
        still_held(&api);
        let why = str_at(&recorded(&api), "/why").to_string();
        assert!(why.contains("reserved for emp-david"), "{why}");
    }

    /// REFUSAL: a verdict from a run the review step was not dispatched
    /// to is not asked of the door; it is left to a person, and said.
    #[tokio::test]
    async fn a_verdict_from_another_run_is_left_to_a_person() {
        let api = api(held_car(json!({})), run(OTHER, H1, "release"), None).await;
        fire(&api, run(OTHER, H1, "release"))
            .await
            .expect("recorded");
        assert!(api.sent(POST_RELEASE).is_empty(), "the door is not asked");
        still_held(&api);
        let rec = recorded(&api);
        assert_eq!(rec["review"], json!(OTHER), "{rec}");
        assert!(str_at(&rec, "/why").contains("dispatched"), "{rec}");
    }

    /// REFUSAL: nothing held, nothing asked — an unheld car, and a car
    /// whose review step is closed, are not the rule's.
    #[tokio::test]
    async fn a_car_with_no_open_hold_is_not_asked_about() {
        let mut unheld = held_car(json!({}));
        unheld["steps"][0]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("hold");
        let mut closed = held_car(json!({}));
        closed["steps"][0]["status"] = json!("completed");
        for car in [unheld, closed] {
            let api = api(car, run(REVIEWER, H1, "release"), None).await;
            fire(&api, run(REVIEWER, H1, "release"))
                .await
                .expect("a no-op");
            assert_eq!(
                api.requests(),
                vec![format!("GET /api/jobs/{CAR}")],
                "read, then nothing"
            );
        }
    }

    /// The same refusal by the same review is written ONCE: a redelivery,
    /// or the run's next write, finds it on the step and writes nothing.
    #[tokio::test]
    async fn the_same_refusal_is_recorded_once() {
        let api = api(held_car(json!({})), run(REVIEWER, H2, "release"), None).await;
        fire(&api, run(REVIEWER, H2, "release"))
            .await
            .expect("recorded");
        fire(&api, run(REVIEWER, H2, "release"))
            .await
            .expect("again");
        assert_eq!(api.sent(PATCH_REVIEW).len(), 1, "{:?}", api.requests());
    }

    /// A DOOR THAT WILL NOT JUDGE SAYS SO ON THE CAR (review 792b871a
    /// N3). An answer that cannot change on redelivery — 401, 403, 404,
    /// 422 — is permanent, and its reason is written beside the hold
    /// ONCE, so the car's reader learns why the rule left it without a
    /// dead-letter query; it is still an error, counted where every
    /// failed firing is. A door 404 was retried before. An unavailable
    /// door (5xx) is retried and writes nothing: it may yet answer.
    #[tokio::test]
    async fn a_door_that_does_not_judge_is_an_error_recorded_once_on_the_car() {
        for (status, permanent) in [
            (StatusCode::UNAUTHORIZED, true),
            (StatusCode::FORBIDDEN, true),
            (StatusCode::NOT_FOUND, true),
            (StatusCode::UNPROCESSABLE_ENTITY, true),
            (StatusCode::INTERNAL_SERVER_ERROR, false),
        ] {
            let api = api(
                held_car(json!({})),
                run(REVIEWER, H1, "release"),
                Some(status),
            )
            .await;
            for _ in 0..2 {
                let err = fire(&api, run(REVIEWER, H1, "release"))
                    .await
                    .expect_err("not a pass");
                assert_eq!(err.is_permanent(), permanent, "{status}: {err}");
            }
            still_held(&api);
            if !permanent {
                assert!(api.sent(PATCH_REVIEW).is_empty(), "{status}");
                continue;
            }
            assert_eq!(api.sent(PATCH_REVIEW).len(), 1, "{status}: recorded once");
            let rec = recorded(&api);
            assert_eq!(rec["review"], json!(REVIEWER), "{rec}");
            let why = str_at(&rec, "/why");
            assert!(why.contains(&status.as_u16().to_string()), "{why}");
            assert!(why.contains("the door answered so"), "{why}");
        }
    }

    /// THE HOLD THE RULE JUDGED IS THE ONLY HOLD THE DOOR LIFTS (review
    /// 792b871a N1). The rule judges the hold's placement on the car as
    /// it READ it; a person's `boss hold` landing before the POST keeps
    /// `hold_sha`, so a door asked with the review alone judged the NEW
    /// hold good on the OLD verdict and lifted it. The ask now carries
    /// the hold the rule judged, text and sha, and the door refuses a
    /// car that carries another. The refusal is recorded once, and the
    /// person's hold stays.
    #[tokio::test]
    async fn a_hold_rewritten_between_the_read_and_the_post_is_not_lifted() {
        let car = held_car(json!({}));
        let hand = "David wants to read this one first";
        let api = api_racing(
            car.clone(),
            run(REVIEWER, H1, "release"),
            None,
            placed_before_the_verdict(&car),
            Some(json!({"hold": hand})),
        )
        .await;
        fire(&api, run(REVIEWER, H1, "release"))
            .await
            .expect("recorded");
        let md = api.review_md();
        assert_eq!(md["hold"], json!(hand), "the person's hold stays: {md}");
        assert!(md.get(boss_jobs::car::RELEASE).is_none(), "{md}");
        let asked = api.sent(POST_RELEASE);
        assert_eq!(asked.len(), 1, "{:?}", api.requests());
        assert_eq!(
            asked[0].0["hold"],
            json!({"reason": "trust-boundary car (area policy)", "sha": H1}),
            "the ask names the hold the rule judged"
        );
        let rec = recorded(&api);
        assert_eq!(rec["outcome"], json!("refused"), "{rec}");
        assert!(str_at(&rec, "/why").contains(hand), "{rec}");
    }

    /// A RACING DUPLICATE IS NOT A REFUSAL (review 792b871a N2). Two
    /// firings for one verdict both read the car held; the first is
    /// granted, and the second's 409 — the hold it judged is gone, the
    /// car is released — is the release already done, not a reason the
    /// rule left the car. Nothing is recorded beside the release.
    #[tokio::test]
    async fn a_racing_duplicate_is_not_recorded_as_a_refusal() {
        let car = held_car(json!({}));
        let first = json!({"hold": null, boss_jobs::car::RELEASE: {
            "sha": H1, "review": REVIEWER, "verdict": "release",
            "by": format!("automation:rule:{RULE}"), "at": "2026-10-16T19:05:01Z"}});
        let api = api_racing(
            car.clone(),
            run(REVIEWER, H1, "release"),
            None,
            placed_before_the_verdict(&car),
            Some(first),
        )
        .await;
        fire(&api, run(REVIEWER, H1, "release"))
            .await
            .expect("the release is done; nothing to say");
        assert_eq!(api.sent(POST_RELEASE).len(), 1, "{:?}", api.requests());
        assert!(api.sent(PATCH_REVIEW).is_empty(), "{:?}", api.requests());
        let md = api.review_md();
        assert!(md.get(boss_jobs::car::AUTO_RELEASE).is_none(), "{md}");
        assert_eq!(md[boss_jobs::car::RELEASE]["review"], json!(REVIEWER));
    }

    /// A HOLD PLACED AFTER THE VERDICT IS NOT LIFTED ON IT (review
    /// 3af8f7e2 N2). The rule fires on ANY write to the dispatched
    /// reviewer's run — its report, its building completion, its age-out,
    /// a backlog drained after an outage — not only on the verdict's own.
    /// A plain `boss hold` over a live review hold keeps `hold_sha` (a
    /// hand hold over a review hold is still a review hold), so the door
    /// would judge the OLD verdict good for the NEW hold. The sequence:
    /// review hold at 18:00, RELEASE at the hold head at 19:00 (not acted
    /// on), a person's hold at 19:30, then the run's report fires the
    /// rule. The door is not asked; the hand hold stays; why is recorded
    /// once.
    #[tokio::test]
    async fn a_hold_placed_after_the_verdict_is_not_lifted_on_it() {
        let review_hold = held_car(json!({}))["steps"][0]["metadata"].clone();
        let hand = "David wants to read this one first";
        let car = held_car(json!({"hold": hand}));
        let history = vec![
            step_event("2026-10-16T18:00:00Z", review_hold),
            step_event("2026-10-16T19:30:00Z", car["steps"][0]["metadata"].clone()),
        ];
        let api = api_with(car, run(REVIEWER, H1, "release"), None, history).await;
        let mut report = run(REVIEWER, H1, "release");
        report["metadata"]["report"] = json!("the review's report, written after its verdict");
        fire(&api, report.clone()).await.expect("recorded");
        fire(&api, report).await.expect("again");
        // The hold wake (backlog 60993cda) reads the same verdict and
        // leaves the hand hold alone for the same reason: it bound no
        // head, so it is not the review hold the verdict judged.
        fire_on_hold(&api).await.expect("the hold wake");
        assert!(api.sent(POST_RELEASE).is_empty(), "the door is not asked");
        let md = api.review_md();
        assert_eq!(md["hold"], json!(hand), "{md}");
        assert!(md.get(boss_jobs::car::RELEASE).is_none(), "{md}");
        let rec = recorded(&api);
        assert_eq!(rec["review"], json!(REVIEWER), "{rec}");
        let why = str_at(&rec, "/why");
        assert!(why.contains("2026-10-16T19:00:00Z"), "{why}");
        assert!(why.contains("2026-10-16T19:30:00Z"), "{why}");
        assert_eq!(api.sent(PATCH_REVIEW).len(), 1, "recorded once");
    }

    /// A history read that answered no `data` array is a refusal naming
    /// the read, never zero rows (backlog d4698bc2): an empty window and a
    /// broken one must not read the same.
    #[test]
    fn a_history_with_no_data_array_is_refused_not_read_as_empty() {
        let car = held_car(json!({}));
        let step = &car["steps"][0];
        let why = hold_placement(&json!({"error": "boom"}), step).expect_err("refused");
        assert!(
            why.contains("the car's history answered no `data` array"),
            "{why}"
        );
        let empty = hold_placement(&json!({"data": []}), step).expect_err("no write");
        assert!(empty.contains("carries no write"), "{empty}");
    }

    /// When EITHER time cannot be read the rule does not ask: a verdict
    /// with no readable `at`, a review step whose history carries no
    /// write of the hold it holds now (a window that starts after the
    /// hold was placed, or a history that disagrees with the car). Each
    /// is recorded once, and the car waits for a person.
    #[tokio::test]
    async fn an_unreadable_time_asks_nothing_and_says_so() {
        let car = held_car(json!({}));
        let mut undated = run(REVIEWER, H1, "release");
        undated["metadata"]["review"]["at"] = json!("yesterday-ish");
        let other_hold = step_event("2026-10-16T18:00:00Z", json!({"hold": "another reason"}));
        let full: Vec<Json> = (0..EVENTS_LIMIT)
            .map(|_| step_event("2026-10-16T18:00:00Z", car["steps"][0]["metadata"].clone()))
            .collect();
        for (payload, history, says) in [
            (undated, placed_before_the_verdict(&car), "verdict"),
            (run(REVIEWER, H1, "release"), vec![], "hold"),
            (run(REVIEWER, H1, "release"), vec![other_hold], "hold"),
            (run(REVIEWER, H1, "release"), full, "hold"),
        ] {
            let api = api_with(car.clone(), run(REVIEWER, H1, "release"), None, history).await;
            fire(&api, payload.clone()).await.expect("recorded");
            fire(&api, payload).await.expect("again");
            assert!(api.sent(POST_RELEASE).is_empty(), "{says}: not asked");
            still_held(&api);
            let why = str_at(&recorded(&api), "/why").to_string();
            assert!(
                why.contains("cannot be read") && why.contains(says),
                "{why}"
            );
            assert_eq!(api.sent(PATCH_REVIEW).len(), 1, "{says}: recorded once");
        }
    }

    /// THE HAPPY PATH: the dispatched reviewer's RELEASE at the head the
    /// hold names releases the car through the door, signed as the rule,
    /// asking nothing but the review; the record credits the rule.
    #[tokio::test]
    async fn the_dispatched_reviewers_release_releases_the_car_as_the_rule() {
        let api = api(held_car(json!({})), run(REVIEWER, H1, "release"), None).await;
        fire(&api, run(REVIEWER, H1, "release"))
            .await
            .expect("released");
        let asked = api.sent(POST_RELEASE);
        assert_eq!(asked.len(), 1, "{:?}", api.requests());
        assert_eq!(
            asked[0].0,
            json!({"review": REVIEWER,
                   "hold": {"reason": "trust-boundary car (area policy)", "sha": H1}}),
            "the review and the hold it judged; no head, no run"
        );
        assert_eq!(asked[0].1, format!("rule:{RULE}"));
        let md = api.review_md();
        assert!(md.get("hold").is_none(), "{md}");
        let release = &md[boss_jobs::car::RELEASE];
        assert_eq!(release["sha"], json!(H1), "{md}");
        assert_eq!(release["review"], json!(REVIEWER));
        assert_eq!(release["by"], json!(format!("automation:rule:{RULE}")));
        assert!(
            api.sent(PATCH_REVIEW).is_empty(),
            "a release records no refusal"
        );
        // The run's next write finds nothing held, and asks nothing.
        fire(&api, run(REVIEWER, H1, "release"))
            .await
            .expect("a no-op");
        assert_eq!(api.sent(POST_RELEASE).len(), 1);
    }

    /// The review hold a gate's green places at `head`, with `reason`:
    /// `hold` and `hold_sha` in ONE write (`jobs_auto_park`'s body, the
    /// rerail's, the conductor's re-judge).
    fn review_hold(reason: &str, head: &str) -> Json {
        json!({"hold": reason, "hold_sha": head, "agent_run": REVIEWER})
    }

    /// THE GAP (backlog 60993cda, claim 3), in the shape car 048df940
    /// had on 2026-10-16. The car stood held for review at its first
    /// gate's head (H2, 18:00); its reviewer read the REPAIR's head H1
    /// and gave RELEASE at 19:00, before the repair's gate finished — so
    /// the door refused it then ("a review of another head") and the
    /// refusal was recorded; at 19:30 the repair's green re-held the car
    /// at H1, the head the verdict read. Nothing asked again, and the
    /// car waited for a person, who released it on that same verdict
    /// (measured: emp-david, 20 s after the hold). Now the hold's own
    /// write wakes the rule, which reads the dispatched reviewer's
    /// verdict and asks the door, signed as the rule.
    #[tokio::test]
    async fn a_verdict_given_before_its_review_hold_is_asked_when_the_hold_is_placed() {
        let reason = "trust-boundary car (area policy)";
        let car = held_car(json!({}));
        let history = vec![
            step_event("2026-10-16T18:00:00Z", review_hold(reason, H2)),
            step_event("2026-10-16T19:30:00Z", review_hold(reason, H1)),
        ];
        let api = api_with(car, run(REVIEWER, H1, "release"), None, history).await;
        fire_on_hold(&api).await.expect("released");
        let asked = api.sent(POST_RELEASE);
        assert_eq!(asked.len(), 1, "{:?}", api.requests());
        assert_eq!(
            asked[0].0,
            json!({"review": REVIEWER, "hold": {"reason": reason, "sha": H1}}),
            "the review and the hold it judged (review 792b871a N1), as the verdict's wake asks"
        );
        assert_eq!(asked[0].1, format!("rule:{RULE}"));
        let md = api.review_md();
        assert!(md.get("hold").is_none(), "{md}");
        assert_eq!(md[boss_jobs::car::RELEASE]["sha"], json!(H1), "{md}");
        assert_eq!(
            md[boss_jobs::car::RELEASE]["by"],
            json!(format!("automation:rule:{RULE}"))
        );
        // The release's own write wakes it again: nothing is held.
        fire_on_hold(&api).await.expect("a no-op");
        assert_eq!(api.sent(POST_RELEASE).len(), 1);
    }

    /// The same gap where the repair's green re-held the car with a NEW
    /// reason (car 4a19d6a9, 2026-10-15: the verdict at 12:30, the gate's
    /// `--hold` at 13:52 binding the head the verdict read, agent-codex's
    /// hand release on that verdict at 13:54). The verdict predates the
    /// hold's reason, but that write bound the hold to exactly the head
    /// the verdict read, so it is the review hold the verdict judged:
    /// asked, from either trigger.
    #[tokio::test]
    async fn a_review_hold_bound_to_the_head_a_verdict_read_is_that_verdicts() {
        let fresh = "mutating-verb source car: fresh distinct exact-head adversarial review";
        let earlier = review_hold("trust-boundary car (area policy)", H2);
        let car = held_car(json!({"hold": fresh}));
        let history = vec![
            step_event("2026-10-16T18:00:00Z", earlier),
            step_event("2026-10-16T19:30:00Z", review_hold(fresh, H1)),
        ];
        for wake in ["hold", "run"] {
            let api = api_with(
                car.clone(),
                run(REVIEWER, H1, "release"),
                None,
                history.clone(),
            )
            .await;
            match wake {
                "hold" => fire_on_hold(&api).await,
                _ => fire(&api, run(REVIEWER, H1, "release")).await,
            }
            .expect("released");
            assert_eq!(api.sent(POST_RELEASE).len(), 1, "{wake}");
            let md = api.review_md();
            assert!(md.get("hold").is_none(), "{wake}: {md}");
        }
    }

    /// A hold bound to a head the verdict did NOT read is not that
    /// verdict's, whatever the door would say: the verdict predates it,
    /// so it is left to a person and the reason recorded, once.
    #[tokio::test]
    async fn a_review_hold_bound_to_another_head_is_not_lifted_on_an_older_verdict() {
        let reason = "trust-boundary car (area policy)";
        let car = held_car(json!({"hold": "re-gated", "hold_sha": H2}));
        let history = vec![
            step_event("2026-10-16T18:00:00Z", review_hold(reason, H1)),
            step_event("2026-10-16T19:30:00Z", review_hold("re-gated", H2)),
        ];
        let api = api_with(car, run(REVIEWER, H1, "release"), None, history).await;
        fire_on_hold(&api).await.expect("recorded");
        fire_on_hold(&api).await.expect("again");
        assert!(api.sent(POST_RELEASE).is_empty(), "the door is not asked");
        let why = str_at(&recorded(&api), "/why").to_string();
        assert!(why.contains("not after the hold"), "{why}");
        assert_eq!(api.sent(PATCH_REVIEW).len(), 1, "recorded once");
    }

    /// The hold wake asks only about a verdict the dispatched reviewer
    /// has GIVEN on this car. A reviewer still working (no verdict), or
    /// one whose verdict names another car, leaves nothing to ask and
    /// nothing to record: the car reads exactly as it did.
    #[tokio::test]
    async fn the_hold_wake_asks_nothing_until_the_reviewer_has_a_verdict_on_this_car() {
        let working = json!({"id": REVIEWER, "kind": "agent-run", "status": "open",
                             "metadata": {}});
        let mut elsewhere = run(REVIEWER, H1, "release");
        elsewhere["metadata"]["review"]["car"] = json!(OTHER);
        for reviewer in [working, elsewhere] {
            let api = api(held_car(json!({})), reviewer.clone(), None).await;
            fire_on_hold(&api).await.expect("a no-op");
            assert_eq!(
                api.requests(),
                vec![GET_CAR.to_string(), GET_REVIEWER.to_string()],
                "read the car and its reviewer, then nothing: {reviewer}"
            );
        }
    }

    /// The hold wake speaks for the car's REVIEW step only, and only
    /// while it holds: another step's write, and a released car's, read
    /// the car and stop.
    #[tokio::test]
    async fn the_hold_wake_reads_only_a_held_review_step() {
        let other = api(held_car(json!({})), run(REVIEWER, H1, "release"), None).await;
        let row = json!({"id": "another-step", "step_id": "another-step", "job_id": CAR,
                         "spec_slug": "review", "metadata": other.review_md()});
        let mut c = ctx(row);
        c.triggering_topic = HOLD_TOPIC.into();
        handler(&other).invoke(&[], &c).await.expect("a no-op");
        let mut released = held_car(json!({}));
        released["steps"][0]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("hold");
        let api2 = api(released, run(REVIEWER, H1, "release"), None).await;
        fire_on_hold(&api2).await.expect("a no-op");
        for a in [&other, &api2] {
            assert_eq!(a.requests(), vec![GET_CAR.to_string()]);
        }
    }

    /// PURE: the placing write is dated, and says which head it bound
    /// when it bound one in the same write — and binds none when the
    /// head was already the step's before it (a hand hold over a live
    /// review hold), or when the step never named one.
    #[test]
    fn the_placing_write_says_which_head_it_bound() {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let car = held_car(json!({"hold": "new", "hold_sha": H1}));
        let step = &car["steps"][0];
        let bound = json!({"data": [
            step_event("2026-10-16T18:00:00Z", review_hold("old", H2)),
            step_event("2026-10-16T19:30:00Z", review_hold("new", H1)),
        ]});
        let p = hold_placement(&bound, step).expect("read");
        assert_eq!(p.at, at("2026-10-16T19:30:00Z"));
        assert_eq!(p.binds.as_deref(), Some(H1));
        let kept = json!({"data": [
            step_event("2026-10-16T18:00:00Z", review_hold("old", H1)),
            step_event("2026-10-16T19:30:00Z", review_hold("new", H1)),
        ]});
        assert_eq!(hold_placement(&kept, step).expect("read").binds, None);
        let first = json!({"data": [
            step_event("2026-10-16T19:30:00Z", review_hold("new", H1)),
        ]});
        assert_eq!(
            hold_placement(&first, step).expect("read").binds.as_deref(),
            Some(H1),
            "the whole history's first write binds what it carries"
        );
    }
}
