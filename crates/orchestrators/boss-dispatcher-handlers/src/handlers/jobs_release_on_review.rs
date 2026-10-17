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
//! A REFUSAL IS RECORDED WHERE THE HOLD IS. The door's 409 sentence is
//! written onto the car's review step as `auto_release` (`{review,
//! outcome, why, by, at}`, `boss_jobs::car::AUTO_RELEASE`), beside the
//! `hold` every reader of a held car already reads — a rule that declines
//! in silence is the failure the correctness protocol forbids. The same
//! refusal by the same review is written once: a redelivery, or a later
//! write to the run, finds it already there and writes nothing.
//!
//! Any other answer from the door is an error: 401/403/422 cannot change
//! on redelivery (Permanent, dead-lettered and counted), the rest are
//! retried.

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

/// PURE: when the hold the review step carries NOW was placed, read off
/// the car's own history (`GET /api/jobs/{car}/events`, oldest first,
/// each step write carrying the post-write row): the instant of the last
/// write of `step` at which its `hold` took the value it holds now.
/// `Err` says why that cannot be read — no write of the step in the
/// window, a window full enough to start after the placing, or a history
/// whose last write disagrees with the car the handler read.
///
/// The record cannot tell a hold rewritten with the SAME text from the
/// hold it replaced, so "placed" means "took its current reason".
pub(crate) fn hold_placed_at(events: &Json, step: &Json) -> Result<DateTime<Utc>, String> {
    let step_id = str_at(step, "/id");
    let rows: Vec<Json> = rows_or_refuse(events, "the car's history")?;
    let full = rows.len() >= EVENTS_LIMIT;
    let writes = rows.iter().filter_map(|e| {
        (str_at(e, "/payload/step_id") == step_id)
            .then(|| e.pointer("/payload/metadata").map(|md| (md.get("hold"), e)))
            .flatten()
    });
    // (the hold as of the last write seen, when that hold took its value)
    let (last, placed) = writes.fold((None, None), |(prev, placed), (hold, e)| match prev {
        Some(p) if p == hold => (Some(hold), placed),
        // The window's first write: its hold is placed HERE only when the
        // window is the whole history; a full window may start after it.
        None if full => (Some(hold), None),
        _ => (Some(hold), Some(str_at(e, "/at"))),
    });
    if last != Some(step.pointer("/metadata/hold")) {
        return Err(format!(
            "the car's history carries no write of review step {} holding what it holds now",
            short(step_id)
        ));
    }
    let placed = placed.ok_or_else(|| {
        format!(
            "the car's history is a full window of {EVENTS_LIMIT} rows that starts after review \
             step {}'s hold took its reason",
            short(step_id)
        )
    })?;
    DateTime::parse_from_rfc3339(placed)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| format!("the write that placed the hold is dated {placed:?}: {e}"))
}

/// PURE: why the rule does not ask the door on this verdict, when it does
/// not: the verdict was not recorded AFTER the hold the car carries now
/// was placed (review 3af8f7e2 N2). The rule fires on any write to the
/// reviewer's run — its report, its age-out, a backlog after an outage —
/// and a person's `boss hold` over a live review hold keeps `hold_sha`,
/// so without this the door would lift a NEW hold on an OLD verdict.
/// Either time unreadable is a reason too: nothing is asked on a guess.
pub(crate) fn not_after_the_hold(payload: &Json, events: &Json, step: &Json) -> Option<String> {
    let said = str_at(
        payload,
        &format!("/metadata/{}/at", car_release::REVIEW_KEY),
    );
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
    let placed = match hold_placed_at(events, step) {
        Ok(t) => t,
        Err(why) => {
            return Some(format!(
                "the time the hold now on the car was placed cannot be read ({why}), so the \
                 release rule cannot tell the verdict came after it; left to a person: boss \
                 release <car> --review <run>"
            ));
        }
    };
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
            return Err(HandlerError::Downstream(format!(
                "PATCH {url} returned {status}: {text}"
            )));
        }
        tracing::info!(rule = %ctx.rule_name, car = %str_at(car, "/id"), review, why,
            "jobs.release_on_review: left the car held, and recorded why on its review step");
        Ok(())
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
        let run = str_at(p, "/id");
        let car_id = str_at(p, &format!("/metadata/{}/car", car_release::REVIEW_KEY));
        if str_at(p, "/kind") != "agent-run" || run.is_empty() || car_id.is_empty() {
            // No run, or a run whose review names no car: no verdict
            // names anything this rule could release.
            return Ok(());
        }
        let base = self.jobs_base.trim_end_matches('/');
        let car_url = format!("{base}/api/jobs/{car_id}");
        let resp = self
            .client
            .get(&car_url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {car_url}: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {car_url} returned {status}: {text}"
            )));
        }
        let car: Json = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {car_url} is not JSON: {e}")))?;
        let Some(step) = held_review(&car) else {
            return Ok(());
        };
        if let Some(why) = not_the_dispatched_reviewer(&car, run) {
            return self.record_refusal(ctx, &car, step, run, &why).await;
        }
        // The verdict must postdate the hold it would lift (review
        // 3af8f7e2 N2), read off the car's own history.
        let events_url = format!("{car_url}/events?limit={EVENTS_LIMIT}");
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
            return Err(HandlerError::Downstream(format!(
                "GET {events_url} returned {status}: {text}"
            )));
        }
        let events: Json = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {events_url} is not JSON: {e}")))?;
        if let Some(why) = not_after_the_hold(p, &events, step) {
            return self.record_refusal(ctx, &car, step, run, &why).await;
        }

        let url = format!("{base}/api/jobs/{car_id}/release");
        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&json!({ "review": run }))
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
        match status.as_u16() {
            409 => self.record_refusal(ctx, &car, step, run, &text).await,
            401 | 403 | 422 => Err(HandlerError::Permanent(format!(
                "POST {url} returned {status}: {text}"
            ))),
            _ => Err(HandlerError::Downstream(format!(
                "POST {url} returned {status}: {text}"
            ))),
        }
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

    /// The pinned protocol answers advisory and there are no first
    /// records: the live shape of every car on 2026-10-16.
    struct Advisory;

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
            _: uuid::Uuid,
            _: uuid::Uuid,
            _: &str,
        ) -> anyhow::Result<Option<boss_jobs::first_record::FirstRecord>> {
            Ok(None)
        }
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
        let events = Arc::new(json!({ "data": events }));
        let car = Arc::new(Mutex::new(car));
        let log: Arc<Mutex<Vec<(String, Json, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let (c, l, r, h) = (car.clone(), log.clone(), Arc::new(run), events);
        let app = Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let (car, log, run, history) = (c.clone(), l.clone(), r.clone(), h.clone());
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
                    if method == Method::GET && uri.path() == format!("{car_path}/events") {
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
                        return match car_release::judge(&Advisory, &now, &run, &ask, &by, "t").await
                        {
                            Ok(g) => {
                                merge(&mut car.lock().unwrap()["steps"][0]["metadata"], &g.patch);
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

    /// A door answer that cannot change on redelivery is permanent; an
    /// unavailable door is retried. Neither writes anything.
    #[tokio::test]
    async fn a_door_that_does_not_judge_is_an_error_not_a_record() {
        for (status, permanent) in [
            (StatusCode::FORBIDDEN, true),
            (StatusCode::UNPROCESSABLE_ENTITY, true),
            (StatusCode::INTERNAL_SERVER_ERROR, false),
        ] {
            let api = api(
                held_car(json!({})),
                run(REVIEWER, H1, "release"),
                Some(status),
            )
            .await;
            let err = fire(&api, run(REVIEWER, H1, "release"))
                .await
                .expect_err("not a pass");
            assert_eq!(err.is_permanent(), permanent, "{status}: {err}");
            assert!(api.sent(PATCH_REVIEW).is_empty(), "{status}");
            still_held(&api);
        }
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
        let why = hold_placed_at(&json!({"error": "boom"}), step).expect_err("refused");
        assert!(
            why.contains("the car's history answered no `data` array"),
            "{why}"
        );
        let empty = hold_placed_at(&json!({"data": []}), step).expect_err("no write");
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
        assert_eq!(asked[0].0, json!({"review": REVIEWER}), "no head, no run");
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
}
