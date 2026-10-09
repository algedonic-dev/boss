//! `jobs.age_out_step` — a clock rule completes an open step on every
//! packet of a kind that has gone SILENT past a bound.
//!
//! The gap this closes (design c87fb59b car 2, backlog 39d0b528): an
//! `agent-run` packet's `building` step is open while a builder works.
//! A builder that dies leaves it open forever — nothing completes it,
//! nothing reads its age, and the run it stood for looks exactly like
//! one still building. "A builder that dies is a packet that ages" was
//! the design's whole reason to make the run a packet, and this is the
//! reader of that age.
//!
//! ## Why this is a handler and not agent-run code
//!
//! Every noun is a rule arg: which `kind`, which `step`, how many
//! `hours` of silence, and what to write (`done_metadata`, the step
//! kind's own required vocabulary — `result = died` for a run). The
//! handler knows "on this tick, for every open packet of `kind` whose
//! `step` is open and whose last movement is older than `hours`,
//! complete `step`" — a shape, not a policy. Point it at a different
//! kind and it ages out a different obligation.
//!
//! Rule shape:
//! ```toml
//! [[rule]]
//! schedule = { cadence = "hourly", anchor_date = "2026-09-18" }
//! [[rule.do]]
//! handler = "jobs.age_out_step"
//! args = { kind = "\"agent-run\"", step = "\"building\"", hours = "\"4\"", done_metadata = '"{\"result\": \"died\"}"' }
//! ```
//!
//! ## What "silent" is measured from
//!
//! The newest `completed_at` across the packet's completed steps — the
//! server stamps it at every flip to `completed` and never
//! client-supplied — because that is the last instant the packet
//! provably MOVED. On a packet with no such stamp (one whose steps all
//! predate the column, or whose only completed step is a trigger born
//! completed without one) the fallback is `metadata.opened_at`, the
//! filing instant the create handler writes. A packet with neither is
//! reported and left alone: "I cannot tell how old this is" is a
//! finding, not a licence to guess.
//!
//! A kind whose life is a HEARTBEAT rather than a chain of completions
//! names the stamp as `since_key` (design 511fa7d4 car 2b, backlog
//! da925366): a `work-session` completes nothing between SessionStart
//! and SessionEnd, and its UserPromptSubmit hook writes
//! `metadata.last_active_at` on every prompt. With `since_key =
//! "last_active_at"` that stamp counts as movement alongside the
//! completions, and the newest of them all is what the silence is
//! measured from — without it a session working for seven hours would
//! be ended six hours after it opened.
//!
//! ## What is never silent, whatever its age (b951c00a)
//!
//! `unless_job_holds = "<key>"` spares a packet whose metadata holds a
//! value under that key. The clock's template says a record NEVER
//! ARRIVED — `handback = absent` for an agent-run — and on 2026-09-28
//! it said so of run e737a54f while the builder's handback sat on the
//! packet as `metadata.report`: the report had come before the gate's
//! green, and nothing had moved it onto the step. The rule that lands
//! such a record (`jobs.complete_step_from_record`) reads presence
//! through the same function, `job_holds`, so the two agree on what a
//! record is. A spared packet stays open and is logged, never guessed.
//!
//! `unless_linked_job_proves_progress` declares a related kind, its
//! source-id link, native start step, live step, success step and typed
//! metadata evidence (d4a3e74f). The newest native launch wins, including
//! terminal launches: an old open gate cannot hide a newer failure, but
//! a green gate waiting for linked completion is still progress. Its
//! branch/SHA describe the linked packet's frozen launch, not a match
//! to the author's working tree, which the source does not record.
//! An unreadable or incomplete linked roster refuses before any writes.
//! Report prose alone proves neither an active gate nor success.
//!
//! ## The record an ending posts (backlog b5a3a174)
//!
//! A run aged out as `unreported` shipped its work and cost something,
//! and until 2026-10-08 it ended with no `agent_runs` row — or, where a
//! launch-time placeholder had been posted for it, with one reading
//! zero tokens at $0, which is a cost nobody measured. A rule naming
//! `post_to` posts a record as it ends the step, by the landing
//! handler's own `post_*` vocabulary (`jobs.complete_step_from_record`,
//! whose `Post` this reuses) less `post_record`: a packet aged out for
//! want of a record stored none, so the body is `post_fields` (fixed
//! keys — `total_tokens = null`, the stated absence of a count, and a
//! `detail.unpriced` sentence) plus `post_fill` (keys read off the
//! packet or a named step), with `post_id_key` naming the packet and
//! `post_at_key` the body key stamped with the tick's instant. Posted
//! BEFORE the completion; what the door answered rides the evidence as
//! `posted`.
//!
//! `now` is the tick's own `_at`, which the schedule runner writes onto
//! every sub-day firing. The handler holds no clock: a rule that fires
//! this on a DAILY cadence gets no `_at` and is refused as permanent —
//! a bound of hours judged once a day is the same silence one level up.
//!
//! ## Idempotence
//!
//! A completed step is not open, so the next tick finds nothing on a
//! packet this one aged out. A tick that dies between two packets
//! leaves the second for the next tick. Nothing is ever written twice.

use super::common::{api_client, complete_step, get_json, open_jobs_of_kind, rows_or_refuse};
use super::jobs_complete_linked_step::{is_open, is_unset, step_by_slug, template_arg};
use super::jobs_complete_step_from_record::{Post, job_holds, post_body, post_record};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::sync::Arc;

/// Registry data chooses the linked obligation and the records proving
/// progress (d4a3e74f). No kind, step or outcome is hardcoded here.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkedProgress {
    kind: String,
    link: String,
    started_step: String,
    live_step: String,
    success_step: String,
    metadata_formats: std::collections::BTreeMap<String, ProgressFormat>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ProgressFormat {
    Nonblank,
    GitSha,
    Timestamp,
}

impl LinkedProgress {
    fn from_args(args: &[(String, Value)]) -> Result<Option<Self>, HandlerError> {
        let Some(value) = arg(args, "unless_linked_job_proves_progress") else {
            return Ok(None);
        };
        let Value::String(text) = value else {
            return Err(HandlerError::Permanent(
                "unless_linked_job_proves_progress must be a JSON string".into(),
            ));
        };
        let guard: Self = serde_json::from_str(text).map_err(|e| {
            HandlerError::Permanent(format!("unless_linked_job_proves_progress: {e}"))
        })?;
        if [
            &guard.kind,
            &guard.link,
            &guard.started_step,
            &guard.live_step,
            &guard.success_step,
        ]
        .iter()
        .any(|s| s.trim().is_empty() || s.contains(['&', '?', '=']))
            || guard.metadata_formats.is_empty()
            || guard.metadata_formats.keys().any(|s| s.trim().is_empty())
        {
            return Err(HandlerError::Permanent(
                "linked progress requires named kind, link, steps and metadata formats".into(),
            ));
        }
        Ok(Some(guard))
    }

    fn started_at(&self, linked: &serde_json::Value) -> Option<DateTime<Utc>> {
        let started = step_by_slug(linked, &self.started_step)?;
        if started.get("status")?.as_str()? != "completed" {
            return None;
        }
        DateTime::parse_from_rfc3339(started.get("completed_at")?.as_str()?)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }

    fn latest<'a>(
        &self,
        source: &serde_json::Value,
        linked: &'a [serde_json::Value],
        now: DateTime<Utc>,
    ) -> Result<Option<(&'a serde_json::Value, DateTime<Utc>)>, HandlerError> {
        let Some(id) = source
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        // Include terminal packets: an older open launch must not hide
        // a newer failed launch. A completed success is progress while
        // its linked completion settles, not evidence of a dead actor.
        let launches: Vec<_> = linked
            .iter()
            .filter(|j| {
                j.get("kind").and_then(serde_json::Value::as_str) == Some(self.kind.as_str())
            })
            .filter(|j| {
                j.get("metadata")
                    .and_then(|m| m.get(&self.link))
                    .and_then(serde_json::Value::as_str)
                    == Some(id)
            })
            .map(|j| {
                if step_by_slug(j, &self.started_step)
                    .and_then(|s| s.get("status"))
                    .and_then(serde_json::Value::as_str) == Some("completed")
                    && self.started_at(j).is_none()
                {
                    return Err(HandlerError::Downstream(format!(
                        "linked packet {} for source {id} has an unreadable completed launch stamp — refusing unknown launch ordering",
                        j["id"]
                    )));
                }
                Ok(self.started_at(j).map(|at| (j, at)))
            })
            .collect::<Result<Vec<_>, HandlerError>>()?
            .into_iter()
            .flatten()
            .collect();
        let Some(at) = launches.iter().map(|(_, at)| *at).max() else {
            return Ok(None);
        };
        let latest: Vec<_> = launches.into_iter().filter(|(_, t)| *t == at).collect();
        if at > now || latest.len() != 1 {
            return Err(HandlerError::Downstream(format!(
                "linked progress for source {id} has {} launches at {at}, firing {now} — refusing an ambiguous or stale silence judgment: {}",
                latest.len(),
                serde_json::Value::Array(latest.iter().map(|(j, _)| j["id"].clone()).collect())
            )));
        }
        if let Some((job, _)) = latest.first()
            && let Some(success) = step_by_slug(job, &self.success_step)
            && success.get("status").and_then(serde_json::Value::as_str) == Some("completed")
            && success
                .get("completed_at")
                .and_then(serde_json::Value::as_str)
                .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
                .is_some_and(|stamp| stamp > now)
        {
            return Err(HandlerError::Downstream(format!(
                "linked packet {} succeeded after firing {now} for source {id} — refusing a stale silence judgment",
                job["id"]
            )));
        }
        Ok(latest.into_iter().next())
    }

    fn protects(
        &self,
        source: &serde_json::Value,
        linked: &[serde_json::Value],
        now: DateTime<Utc>,
    ) -> bool {
        let Some((j, at)) = self.latest(source, linked, now).ok().flatten() else {
            return false;
        };
        if last_moved(source, None).is_none_or(|since| at < since) {
            return false;
        }
        let valid_metadata = self.metadata_formats.iter().all(|(key, format)| {
            let Some(text) = j
                .get("metadata")
                .and_then(|m| m.get(key))
                .and_then(serde_json::Value::as_str)
            else {
                return false;
            };
            match format {
                ProgressFormat::Nonblank => !text.trim().is_empty(),
                ProgressFormat::GitSha => {
                    text.len() == 40 && text.bytes().all(|c| c.is_ascii_hexdigit())
                }
                ProgressFormat::Timestamp => {
                    // CLI seconds and the server microsecond stamp can
                    // arrive in either order. Ordering uses the server
                    // stamp alone; this field proves only its format.
                    DateTime::parse_from_rfc3339(text).is_ok_and(|t| t <= now)
                }
            }
        });
        if !valid_metadata {
            return false;
        }
        match j.get("status").and_then(serde_json::Value::as_str) {
            Some("open") => step_by_slug(j, &self.live_step).is_some_and(is_open),
            Some("closed") => step_by_slug(j, &self.success_step).is_some_and(|s| {
                s.get("status").and_then(serde_json::Value::as_str) == Some("completed")
                    && s.get("completed_at")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                        .is_some_and(|t| t >= at && t <= now)
            }),
            _ => false,
        }
    }

    async fn read(
        &self,
        handler: &JobsAgeOutStep,
        rule: &str,
    ) -> Result<Vec<serde_json::Value>, HandlerError> {
        let mut rows = Vec::new();
        let mut expected_total = None;
        let mut ids = std::collections::HashSet::new();
        loop {
            let body = get_json(
                &handler.client,
                &format!(
                    "{}/api/jobs?kind={}&full=true&limit=500&offset={}",
                    handler.base(),
                    self.kind,
                    rows.len()
                ),
                rule,
            )
            .await?;
            let total = body
                .get("total")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    HandlerError::Downstream(
                        "linked progress GET /api/jobs has no readable total".into(),
                    )
                })? as usize;
            let page: Vec<serde_json::Value> =
                rows_or_refuse(&body, "linked progress GET /api/jobs")
                    .map_err(HandlerError::Downstream)?;
            if expected_total.is_some_and(|expected| expected != total)
                || rows.len() + page.len() > total
            {
                return Err(HandlerError::Downstream("linked progress GET /api/jobs changed its total or returned surplus rows — refusing a silence verdict".into()));
            }
            expected_total = Some(total);
            for row in &page {
                let id = row
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| {
                        HandlerError::Downstream(
                            "linked progress GET /api/jobs row has no readable id".into(),
                        )
                    })?;
                if !ids.insert(id.to_string()) {
                    return Err(HandlerError::Downstream(format!(
                        "linked progress GET /api/jobs repeated packet {id} — refusing a silence verdict"
                    )));
                }
            }
            if page.is_empty() && rows.len() < total {
                return Err(HandlerError::Downstream("linked progress GET /api/jobs ended before its total — refusing a silence verdict".into()));
            }
            rows.extend(page);
            if rows.len() >= total {
                return Ok(rows);
            }
        }
    }
}

/// Default metadata key the measurement lands under on the completed
/// step. Overridable per rule via the `evidence_key` arg.
const DEFAULT_EVIDENCE_KEY: &str = "aged_out";

/// The key the schedule runner writes the firing instant under, on
/// every sub-day tick (`schedule_runner::tick`).
const TICK_AT: &str = "_at";

pub struct JobsAgeOutStep {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsAgeOutStep {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// local stand-in for jobs-api).
    pub fn with_client(
        client: boss_core::machine_token::Client,
        jobs_base: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

/// The last instant this packet provably moved: the newest
/// `completed_at` on any completed step — and, when the rule names a
/// `since_key`, the newest of those and `metadata.<since_key>` (a
/// heartbeat) — else `metadata.opened_at`. `None` when none is
/// readable.
pub(crate) fn last_moved(
    job: &serde_json::Value,
    since_key: Option<&str>,
) -> Option<DateTime<Utc>> {
    let parse = |v: &serde_json::Value| {
        v.as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
    };
    let metadata = |k: &str| job.get("metadata").and_then(|m| m.get(k)).and_then(parse);
    let newest_completion = job
        .get("steps")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("completed"))
        .filter_map(|s| s.get("completed_at").and_then(parse))
        .max();
    let heartbeat = since_key.and_then(metadata);
    newest_completion
        .max(heartbeat)
        .or_else(|| metadata("opened_at"))
}

/// Hours of silence, or `None` when the packet's age cannot be read.
pub(crate) fn silent_hours(
    job: &serde_json::Value,
    since_key: Option<&str>,
    now: DateTime<Utc>,
) -> Option<f64> {
    last_moved(job, since_key).map(|since| (now - since).num_seconds() as f64 / 3600.0)
}

/// The `hours` arg as a positive bound. A rule authored with a bound
/// that is not a positive number is permanent — no redelivery fixes it.
fn bound_hours(args: &[(String, Value)]) -> Result<f64, HandlerError> {
    let raw = arg_string(args, "hours")?;
    match raw.trim().parse::<f64>() {
        Ok(h) if h.is_finite() && h > 0.0 => Ok(h),
        _ => Err(HandlerError::Permanent(format!(
            "hours must be a positive number of hours, got {raw:?}"
        ))),
    }
}

#[async_trait]
impl Handler for JobsAgeOutStep {
    fn name(&self) -> &'static str {
        "jobs.age_out_step"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let kind = arg_string(args, "kind")?;
        let step_slug = arg_string(args, "step")?;
        let bound = bound_hours(args)?;
        let linked_progress = LinkedProgress::from_args(args)?;
        let evidence_key = match arg(args, "evidence_key") {
            Some(Value::String(s)) if !s.is_empty() => s.as_str(),
            _ => DEFAULT_EVIDENCE_KEY,
        };
        let template = template_arg(args, "done_metadata", &ctx.rule_name);
        let since_key = match arg(args, "since_key") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        };
        let unless_job_holds = match arg(args, "unless_job_holds") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        };
        let text = |k: &str| match arg(args, k) {
            Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        };
        // THE RECORD THE ENDING POSTS (b5a3a174 — see the module doc):
        // built from the rule's own args, because a packet aged out for
        // want of a record stored none.
        let post = text("post_to").map(|to| Post {
            to,
            record: None,
            id_key: text("post_id_key"),
            fields: template_arg(args, "post_fields", &ctx.rule_name).unwrap_or_default(),
            fill: template_arg(args, "post_fill", &ctx.rule_name).unwrap_or_default(),
        });
        let post_at_key = text("post_at_key");

        // The tick's own instant. Absent on a daily firing, which is a
        // rule-authoring error this handler cannot make good by
        // guessing a clock.
        let now = ctx
            .event_payload
            .get(TICK_AT)
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "the firing carries no `{TICK_AT}` — jobs.age_out_step judges hours and \
                     needs a sub-day cadence (hourly, every-<n>-minutes)"
                ))
            })?;

        let candidates = open_jobs_of_kind(&self.client, self.base(), kind, &ctx.rule_name).await?;
        // A failed/partial read is not absence. Read before any writes
        // so an unavailable linked roster cannot produce a false death.
        let linked = match &linked_progress {
            Some(guard) => guard.read(self, &ctx.rule_name).await?,
            None => Vec::new(),
        };
        if let Some(guard) = &linked_progress {
            for source in candidates.iter().filter(|source| {
                step_by_slug(source, step_slug).is_some_and(is_open)
                    && silent_hours(source, since_key, now).is_some_and(|hours| hours >= bound)
            }) {
                // A future server stamp or tied latest launches are
                // unknown ordering, not licence to choose a verdict.
                guard.latest(source, &linked, now)?;
            }
        }
        for job in &candidates {
            let job_id = job.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let Some(step) = step_by_slug(job, step_slug).filter(|s| is_open(s)) else {
                continue;
            };
            // A packet already holding the record this step stands for
            // is not silent, whatever its age: writing the template
            // onto it would say the record never came while it sits on
            // the packet (run e737a54f, 2026-09-28, b951c00a). It is
            // left open and said so — the shop floor keeps counting it,
            // and a hand, or the rule that lands the record, closes it.
            if let Some(key) = unless_job_holds
                && job_holds(job, key)
            {
                tracing::warn!(
                    rule = %ctx.rule_name,
                    packet = %job_id,
                    "`{step_slug}` is open and the packet holds `{key}` — not aged out: the \
                     record is there to land, not absent"
                );
                continue;
            }
            let Some(silent) = silent_hours(job, since_key, now) else {
                tracing::warn!(
                    rule = %ctx.rule_name,
                    packet = %job_id,
                    "`{step_slug}` is open but the packet carries no completed_at and no \
                     opened_at — its age cannot be read, leaving it alone"
                );
                continue;
            };
            if silent < bound {
                continue;
            }
            if linked_progress
                .as_ref()
                .is_some_and(|guard| guard.protects(job, &linked, now))
            {
                tracing::info!(rule = %ctx.rule_name, packet = %job_id,
                    "linked obligation proves progress — silence verdict deferred");
                continue;
            }
            let Some(step_id) = step.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let existing = step.get("metadata").and_then(|m| m.as_object());
            // The template's vocabulary, ABSENT keys only: a value a
            // person wrote on the step is their record.
            let mut fields: serde_json::Map<String, serde_json::Value> = template
                .iter()
                .flatten()
                .filter(|(k, _)| is_unset(existing.and_then(|m| m.get(*k))))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let mut evidence = json!({
                "silent_hours": (silent * 100.0).round() / 100.0,
                "bound_hours": bound,
                "at": now.to_rfc3339(),
                "rule": ctx.rule_name,
            });
            // THE RECORD, BEFORE THE COMPLETION — the landing handler's
            // order and its reason: the door is insert-once, so a tick
            // that dies after the post re-posts harmlessly, while a
            // completion made first would close the step and no later
            // tick would reach this line. A refusal no retry repairs
            // does not hold the ending hostage; the evidence carries it.
            if let Some(p) = &post {
                let posted = match post_body(job, p) {
                    None => "absent: nothing to post".to_string(),
                    Some(Err(e)) => format!("refused: {e}"),
                    Some(Ok(mut body)) => {
                        if let Some(key) = post_at_key {
                            body[key] = json!(now.to_rfc3339());
                        }
                        let url = format!("{}{}", self.base(), p.to);
                        match post_record(&self.client, &url, &body, &ctx.rule_name).await? {
                            Ok(()) => format!("recorded: posted to {}", p.to),
                            Err(e) => {
                                tracing::warn!(rule = %ctx.rule_name, packet = %job_id, "{e}");
                                format!("refused: {e}")
                            }
                        }
                    }
                };
                evidence["posted"] = json!(posted);
            }
            fields.insert(evidence_key.to_string(), evidence);
            // Those keys through the step merge door, then the status
            // alone (backlog e39a9d2a): this PUT the step's metadata AS
            // READ plus them, which the step PUT refuses once anything
            // wrote the step in between, and refuses outright under the
            // decided end state. The merge door keeps what it is not
            // sent.
            complete_step(
                &self.client,
                self.base(),
                job_id,
                step_id,
                fields,
                &ctx.rule_name,
            )
            .await?;
            tracing::info!(
                rule = %ctx.rule_name,
                packet = %job_id,
                "`{step_slug}` silent {silent:.1}h past the {bound}h bound — aged out"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const SILENT: &str = "11111111-1111-1111-1111-111111111111";
    const FRESH: &str = "22222222-2222-2222-2222-222222222222";
    const AGELESS: &str = "33333333-3333-3333-3333-333333333333";

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "agent-run-dies-when-building-is-silent".into(),
            triggering_event_id: "tick-1".into(),
            triggering_topic: "schedule".into(),
            event_payload: payload,
        }
    }

    fn args() -> Vec<(String, Value)> {
        vec![
            ("kind".to_string(), Value::String("agent-run".into())),
            ("step".to_string(), Value::String("building".into())),
            ("hours".to_string(), Value::String("4".into())),
            (
                "done_metadata".to_string(),
                Value::String(r#"{"result": "died"}"#.into()),
            ),
        ]
    }

    /// The tick the schedule runner emits for a sub-day cadence.
    fn tick(at: &str) -> serde_json::Value {
        json!({ "_day": &at[..10], "_at": at })
    }

    /// An open run whose `briefed` completed at `briefed_at` (the
    /// moment the build began) and whose `building` is open.
    fn run(id: &str, briefed_at: Option<&str>, opened_at: Option<&str>) -> serde_json::Value {
        let mut metadata = json!({ "packet": "p", "step": "build" });
        if let Some(o) = opened_at {
            metadata["opened_at"] = json!(o);
        }
        json!({
            "id": id,
            "kind": "agent-run",
            "title": format!("run {id}"),
            "status": "open",
            "metadata": metadata,
            "steps": [
                { "id": format!("{id}-claimed"), "spec_slug": "claimed", "status": "completed",
                  "completed_at": null, "metadata": {} },
                { "id": format!("{id}-briefed"), "spec_slug": "briefed", "status": "completed",
                  "completed_at": briefed_at, "metadata": { "prompt_bytes": "1" } },
                { "id": format!("{id}-building"), "spec_slug": "building", "status": "ready",
                  "metadata": { "authority_role": "platform-admin" } },
                { "id": format!("{id}-reported"), "spec_slug": "reported", "status": "ready",
                  "metadata": {} },
            ],
        })
    }

    type Puts = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

    async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Puts) {
        let puts: Puts = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<Mutex<HashMap<String, serde_json::Value>>> = Arc::new(Mutex::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                .collect(),
        ));
        let list_jobs = by_id.clone();
        let put_jobs = by_id.clone();
        let merge_jobs = by_id.clone();
        let put_log = puts.clone();
        let merge_log = puts.clone();
        let post_log = puts.clone();
        let app = Router::new()
            // The agent-runs door, recorded in order with the step writes.
            .route(
                "/api/agent-runs",
                axum::routing::post(move |Json(body): Json<serde_json::Value>| {
                    let puts = post_log.clone();
                    async move {
                        puts.lock()
                            .unwrap()
                            .push(("POST".into(), "/api/agent-runs".into(), body));
                        Json(json!({ "recorded": true }))
                    }
                }),
            )
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let by_id = list_jobs.clone();
                    async move {
                        let rows: Vec<serde_json::Value> = by_id
                            .lock()
                            .unwrap()
                            .values()
                            .filter(|j| {
                                q.get("kind").is_none_or(|k| j["kind"] == json!(k))
                                    && q.get("status").is_none_or(|s| j["status"] == json!(s))
                            })
                            .cloned()
                            .collect();
                        Json(json!({ "data": rows, "total": rows.len() }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}",
                axum::routing::put(
                    move |Path((id, step_id)): Path<(String, String)>,
                          Json(body): Json<serde_json::Value>| {
                        let puts = put_log.clone();
                        let by_id = put_jobs.clone();
                        async move {
                            // The decided end state (e39a9d2a).
                            if let Some(refused) =
                                super::super::listing_stub::end_state_step_put(&id, &step_id, &body)
                            {
                                return refused;
                            }
                            puts.lock()
                                .unwrap()
                                .push((id.clone(), step_id.clone(), body.clone()));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id)
                                && let Some(steps) =
                                    job.get_mut("steps").and_then(|s| s.as_array_mut())
                            {
                                for step in steps.iter_mut() {
                                    if step["id"] == json!(step_id) {
                                        step["status"] = body["status"].clone();
                                    }
                                }
                            }
                            Json(json!({ "ok": true })).into_response()
                        }
                    },
                ),
            )
            // The step merge door: merges into the stored step, and is
            // recorded in order with the PUTs as `<step>/metadata`.
            .route(
                "/api/jobs/{id}/steps/{step_id}/metadata",
                axum::routing::patch(
                    move |Path((id, step_id)): Path<(String, String)>,
                          Json(body): Json<serde_json::Value>| {
                        let puts = merge_log.clone();
                        let by_id = merge_jobs.clone();
                        async move {
                            puts.lock().unwrap().push((
                                id.clone(),
                                format!("{step_id}/metadata"),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id)
                                && let Some(steps) =
                                    job.get_mut("steps").and_then(|s| s.as_array_mut())
                            {
                                for step in steps.iter_mut() {
                                    if step["id"] == json!(step_id)
                                        && let (Some(stored), Some(sent)) =
                                            (step["metadata"].as_object_mut(), body.as_object())
                                    {
                                        for (k, v) in sent {
                                            stored.insert(k.clone(), v.clone());
                                        }
                                    }
                                }
                            }
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

    /// The obligation: a run briefed five hours ago with `building`
    /// still open is aged out — `result = died`, the measurement under
    /// `aged_out`, the step's own keys kept — and a run briefed an hour
    /// ago is not. A second tick finds the first run's step completed
    /// and writes nothing more.
    #[tokio::test]
    async fn a_silent_run_is_aged_out_once_and_a_fresh_one_is_left_alone() {
        let (base, puts) = mock_jobs(vec![
            run(SILENT, Some("2026-09-18T10:00:00Z"), None),
            run(FRESH, Some("2026-09-18T14:00:00Z"), None),
        ])
        .await;
        let h = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-18T15:00:00Z")))
            .await
            .expect("the tick runs");
        let written = puts.lock().unwrap().clone();
        assert_eq!(
            written.len(),
            2,
            "exactly one step completed — the merge, then the flip: {written:?}"
        );
        let (job, step, body) = &written[0];
        assert_eq!(job, SILENT);
        assert_eq!(step, &format!("{SILENT}-building/metadata"));
        assert_eq!(body["result"], "died");
        assert!(
            body.get("authority_role").is_none(),
            "the step's own keys are not re-sent: the merge door keeps them (e39a9d2a)"
        );
        assert_eq!(body["aged_out"]["silent_hours"], 5.0);
        assert_eq!(body["aged_out"]["bound_hours"], 4.0);
        assert_eq!(
            body["aged_out"]["rule"],
            "agent-run-dies-when-building-is-silent"
        );
        assert_eq!(written[1].1, format!("{SILENT}-building"));
        assert_eq!(
            written[1].2,
            json!({ "status": "completed" }),
            "the flip alone"
        );

        h.invoke(&args(), &ctx(tick("2026-09-18T16:00:00Z")))
            .await
            .expect("the next tick runs");
        assert_eq!(puts.lock().unwrap().len(), 2, "nothing is written twice");
    }

    /// A packet with no completion stamp is measured from its filing
    /// instant; one with neither is reported and left open.
    #[tokio::test]
    async fn the_age_falls_back_to_opened_at_and_an_unreadable_age_is_left_alone() {
        let (base, puts) = mock_jobs(vec![
            run(SILENT, None, Some("2026-09-18T09:00:00+00:00")),
            run(AGELESS, None, None),
        ])
        .await;
        let h = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-18T15:00:00Z")))
            .await
            .expect("the tick runs");
        let written = puts.lock().unwrap().clone();
        assert_eq!(written.len(), 2, "the merge, then the flip: {written:?}");
        assert_eq!(written[0].0, SILENT);
        assert_eq!(written[0].2["aged_out"]["silent_hours"], 6.0);
    }

    /// A value a person wrote on the step is their record: the template
    /// fills absent keys only.
    #[tokio::test]
    async fn the_template_never_overwrites_a_recorded_value() {
        let mut silent = run(SILENT, Some("2026-09-18T10:00:00Z"), None);
        silent["steps"][2]["metadata"]["result"] = json!("refused");
        let (base, puts) = mock_jobs(vec![silent]).await;
        let h = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-18T15:00:00Z")))
            .await
            .expect("the tick runs");
        let written = puts.lock().unwrap().clone();
        assert_eq!(written.len(), 2, "the merge, then the flip: {written:?}");
        assert!(
            written[0].2.get("result").is_none(),
            "a recorded value is neither overwritten nor re-sent"
        );
    }

    /// THE FALSE RECORD (b951c00a): run e737a54f was aged out as
    /// `handback = absent` at 19:00Z on 2026-09-28 while its builder's
    /// report sat on the packet as `metadata.report`. A packet holding
    /// the key the rule names is never aged out, however long its step
    /// has been open; one holding nothing — or an empty string — still
    /// is, which is the silence the clock exists to end.
    #[tokio::test]
    async fn a_packet_holding_its_report_is_never_aged_out_as_absent() {
        let reported = |id: &str, report: Option<&str>| {
            let mut r = run(id, Some("2026-09-28T16:00:00Z"), None);
            r["steps"][2]["status"] = json!("completed");
            r["steps"][2]["metadata"]["result"] = json!("gated");
            if let Some(t) = report {
                r["metadata"]["report"] = json!(t);
            }
            r
        };
        let (base, puts) = mock_jobs(vec![
            reported(SILENT, Some("packet x, branch y, sha z, gate-run g")),
            reported(FRESH, None),
            reported(AGELESS, Some("")),
        ])
        .await;
        let args = vec![
            ("kind".to_string(), Value::String("agent-run".into())),
            ("step".to_string(), Value::String("reported".into())),
            ("hours".to_string(), Value::String("2".into())),
            (
                "unless_job_holds".to_string(),
                Value::String("report".into()),
            ),
            (
                "done_metadata".to_string(),
                Value::String(r#"{"handback": "absent", "summary": "No handback"}"#.into()),
            ),
        ];
        let h = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        h.invoke(&args, &ctx(tick("2026-09-28T19:00:00Z")))
            .await
            .expect("the tick runs");
        let written = puts.lock().unwrap().clone();
        let mut aged: Vec<&str> = written
            .iter()
            .filter(|(_, s, _)| s.ends_with("/metadata"))
            .map(|(j, _, _)| j.as_str())
            .collect();
        aged.sort();
        assert_eq!(
            aged,
            vec![FRESH, AGELESS],
            "the run holding its report is spared; the silent ones are not: {written:?}"
        );
        assert!(
            !written.iter().any(|(j, _, _)| j == SILENT),
            "nothing is written onto the run whose report is on the packet: {written:?}"
        );
    }

    /// The unreported rule as `infra/dispatcher/rules/` authors it,
    /// read from the file so this judges the rule that runs (§9a).
    fn unreported_args() -> Vec<(String, Value)> {
        let path = boss_testing::repo_root().join(
            "infra/dispatcher/rules/agent-run-ends-unreported-when-the-handback-never-arrives.toml",
        );
        let text = std::fs::read_to_string(&path).expect("the rule is authored");
        let rule: toml::Value = toml::from_str(&text).expect("the rule parses");
        rule["rule"][0]["do"][0]["args"]
            .as_table()
            .expect("the rule has args")
            .iter()
            .map(|(k, v)| {
                // An arg is an expression: a quoted string literal.
                let lit: String = serde_json::from_str(v.as_str().expect("an expression string"))
                    .expect("a string literal");
                (k.clone(), Value::String(lit))
            })
            .collect()
    }

    /// A RUN THAT NEVER REPORTS STILL ENDS WITH A ROW, AND THE ROW SAYS
    /// WHY IT HAS NO PRICE (backlog b5a3a174; the died and refused
    /// endings stay on 4f45f5ec). The clock that ends the run
    /// `unreported` posts its `agent_runs` record first: the run's own
    /// id, the agent `building` was placed with, the model the packet
    /// declared, finished at the tick — and NO count, stated as a null,
    /// with `detail.unpriced` naming the absence. Never a zero, which
    /// reads as a cost of $0. A report that arrives later replaces it,
    /// once (`boss_jobs::agent_runs::port::replaces`).
    #[tokio::test]
    async fn a_run_that_never_reported_ends_with_a_row_that_says_so() {
        let mut silent = run(SILENT, Some("2026-10-08T14:00:00Z"), None);
        silent["metadata"] = json!({
            "packet": "77777777-7777-4777-8777-777777777777", "step": "build",
            "agent": "claude@algedonic.dev", "model": "opus-5[1m]",
            "opened_at": "2026-10-08T13:59:00Z",
        });
        silent["steps"][2] = json!({
            "id": format!("{SILENT}-building"), "spec_slug": "building", "status": "completed",
            "assignee_id": "agent-claude", "completed_at": "2026-10-08T15:00:00Z",
            "metadata": { "result": "gated", "gate_run": { "branch": "fix/the-car" } },
        });
        let (base, puts) = mock_jobs(vec![silent]).await;
        let h = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        let mut c = ctx(tick("2026-10-08T18:00:00Z"));
        c.rule_name = "agent-run-ends-unreported-when-the-handback-never-arrives".into();
        h.invoke(&unreported_args(), &c)
            .await
            .expect("the tick runs");
        let written = puts.lock().unwrap().clone();
        assert_eq!(
            written.len(),
            3,
            "the record, the merge, the flip: {written:?}"
        );
        let (_, path, row) = &written[0];
        assert_eq!(path, "/api/agent-runs", "the record goes first");
        assert_eq!(row["run_id"], SILENT);
        assert_eq!(row["actor_id"], "agent-claude");
        assert_eq!(row["model"], "opus-5[1m]");
        assert_eq!(row["started_at"], "2026-10-08T13:59:00Z");
        assert_eq!(row["finished_at"], "2026-10-08T18:00:00+00:00");
        assert_eq!(row["outcome"], "success", "the work shipped");
        assert_eq!(row["job_id"], "77777777-7777-4777-8777-777777777777");
        assert_eq!(row["branch"], "fix/the-car");
        assert!(
            row.as_object().unwrap().get("total_tokens") == Some(&serde_json::Value::Null),
            "no count, said as a null — never a zero: {row}"
        );
        assert!(
            row["detail"]["unpriced"]
                .as_str()
                .is_some_and(|w| w.starts_with("never reported")),
            "{row}"
        );
        // The door reads this body as a run that reported no count.
        let parsed: boss_jobs::agent_runs::NewAgentRun =
            serde_json::from_value(row.clone()).expect("the door's own shape");
        assert_eq!(parsed.tokens, boss_jobs::agent_runs::TokenUsage::Unreported);

        let merge = &written[1].2;
        assert_eq!(merge["handback"], "absent");
        assert!(
            merge["aged_out"]["posted"]
                .as_str()
                .is_some_and(|p| p.starts_with("recorded")),
            "the step says the row was posted: {merge}"
        );
        assert_eq!(written[2].2, json!({ "status": "completed" }));

        h.invoke(&unreported_args(), &c)
            .await
            .expect("the next tick runs");
        assert_eq!(puts.lock().unwrap().len(), 3, "nothing is written twice");
    }

    /// A daily firing carries no `_at`; the handler refuses rather than
    /// reading a clock of its own, and says which cadence to use.
    #[tokio::test]
    async fn a_firing_without_an_instant_is_a_permanent_refusal() {
        let (base, puts) = mock_jobs(vec![run(SILENT, Some("2026-09-18T10:00:00Z"), None)]).await;
        let h = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        let err = h
            .invoke(&args(), &ctx(json!({ "_day": "2026-09-18" })))
            .await
            .expect_err("no _at, no judgement");
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("_at") && m.contains("hourly")),
            "{err}"
        );
        assert!(puts.lock().unwrap().is_empty());
    }

    #[test]
    fn a_bound_that_is_not_a_positive_number_is_permanent() {
        for bad in ["0", "-2", "soon", ""] {
            let mut a = args();
            a[2].1 = Value::String(bad.into());
            assert!(
                matches!(bound_hours(&a), Err(HandlerError::Permanent(_))),
                "{bad:?}"
            );
        }
        assert_eq!(bound_hours(&args()).unwrap(), 4.0);
    }

    #[test]
    fn last_moved_prefers_the_newest_completion_over_the_filing_instant() {
        let mut job = run(
            SILENT,
            Some("2026-09-18T10:00:00Z"),
            Some("2026-09-18T09:00:00Z"),
        );
        assert_eq!(
            last_moved(&job, None).unwrap().to_rfc3339(),
            "2026-09-18T10:00:00+00:00"
        );
        job["steps"][1]["completed_at"] = json!(null);
        assert_eq!(
            last_moved(&job, None).unwrap().to_rfc3339(),
            "2026-09-18T09:00:00+00:00"
        );
        job["metadata"]["opened_at"] = json!(null);
        assert_eq!(last_moved(&job, None), None);
    }

    /// A kind whose life is a heartbeat, not a chain of completions: a
    /// work-session prompts for hours and completes nothing between
    /// SessionStart and SessionEnd (design 511fa7d4 car 2b, backlog
    /// da925366). With `since_key = last_active_at` the heartbeat is
    /// movement — a session that prompted an hour ago is left alone
    /// however long ago it opened; one whose last prompt is past the
    /// bound is ended, `ended = silent`, measured from that prompt and
    /// not from its opening.
    #[tokio::test]
    async fn the_since_key_reads_a_heartbeat_as_movement() {
        let session = |id: &str, last_active_at: Option<&str>| {
            let mut metadata = json!({
                "actor": "emp-david", "host": "boss-dev-0",
                "started_at": "2026-09-19T00:00:00Z", "opened_at": "2026-09-19T00:00:00Z",
            });
            if let Some(t) = last_active_at {
                metadata["last_active_at"] = json!(t);
            }
            json!({
                "id": id, "kind": "work-session", "title": "session", "status": "open",
                "metadata": metadata,
                "steps": [
                    { "id": format!("{id}-opened"), "spec_slug": "opened", "status": "completed",
                      "completed_at": null, "metadata": {} },
                    { "id": format!("{id}-active"), "spec_slug": "active", "status": "ready",
                      "metadata": { "authority_role": "platform-admin" } },
                ],
            })
        };
        let (base, puts) = mock_jobs(vec![
            // Opened 9h ago, prompted 1h ago: alive.
            session(FRESH, Some("2026-09-19T08:00:00Z")),
            // Opened 9h ago, last prompt 7h ago: silent past six.
            session(SILENT, Some("2026-09-19T02:00:00Z")),
            // Opened 9h ago, never prompted: measured from its opening.
            session(AGELESS, None),
        ])
        .await;
        let args = vec![
            ("kind".to_string(), Value::String("work-session".into())),
            ("step".to_string(), Value::String("active".into())),
            ("hours".to_string(), Value::String("6".into())),
            (
                "since_key".to_string(),
                Value::String("last_active_at".into()),
            ),
            (
                "done_metadata".to_string(),
                Value::String(r#"{"ended": "silent"}"#.into()),
            ),
        ];
        let h = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        h.invoke(&args, &ctx(tick("2026-09-19T09:00:00Z")))
            .await
            .expect("the tick runs");
        let written = puts.lock().unwrap().clone();
        // The merges: one per ended session, onto its `active` step.
        let merges: Vec<_> = written
            .iter()
            .filter(|(_, s, _)| s.ends_with("/metadata"))
            .collect();
        let mut ended: Vec<&str> = merges.iter().map(|(j, _, _)| j.as_str()).collect();
        ended.sort();
        assert_eq!(ended, vec![SILENT, AGELESS], "{written:?}");
        assert_eq!(written.len(), 4, "a merge and a flip for each: {written:?}");
        let silent = merges.iter().find(|(j, _, _)| j == SILENT).unwrap();
        assert_eq!(silent.2["ended"], "silent");
        assert_eq!(
            silent.2["aged_out"]["silent_hours"], 7.0,
            "measured from the last prompt, not the opening"
        );
        let never = merges.iter().find(|(j, _, _)| j == AGELESS).unwrap();
        assert_eq!(never.2["aged_out"]["silent_hours"], 9.0);
    }

    fn linked_guard_args() -> Vec<(String, Value)> {
        let mut a = args();
        a.push((
            "unless_linked_job_proves_progress".into(),
            Value::String(json!({
                "kind": "gate-run", "link": "agent_run", "live_step": "record-verdict",
                "started_step": "launched", "success_step": "green",
                "metadata_formats": {"branch": "nonblank", "sha": "git-sha", "launched_at": "timestamp"}
            }).to_string()),
        ));
        a
    }

    fn linked_gate(id: &str, run_id: &str, at: &str, status: &str) -> serde_json::Value {
        json!({
            "id": id, "kind": "gate-run", "status": status,
            "metadata": { "agent_run": run_id, "branch": "fix/example", "sha": "a".repeat(40),
                "launched_at": at },
            "steps": [
                { "id": "launched", "spec_slug": "launched", "status": "completed", "completed_at": at },
                { "id": "verdict", "spec_slug": "record-verdict", "status": if status == "open" { "ready" } else { "completed" } }
            ]
        })
    }

    #[tokio::test]
    async fn a_live_linked_gate_spares_its_run_but_a_report_alone_does_not() {
        let mut unprotected = run(FRESH, Some("2026-09-18T00:00:00Z"), None);
        unprotected["metadata"]["report"] = json!("a builder said it was still running");
        let (base, puts) = mock_jobs(vec![
            run(SILENT, Some("2026-09-18T00:00:00Z"), None),
            unprotected,
            linked_gate("g", SILENT, "2026-09-18T05:00:00Z", "open"),
        ])
        .await;
        let h = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        h.invoke(&linked_guard_args(), &ctx(tick("2026-09-18T06:00:00Z")))
            .await
            .unwrap();
        let written = puts.lock().unwrap().clone();
        assert_eq!(
            written.len(),
            2,
            "only the unprotected run gets merge + status: {written:?}"
        );
        assert!(written.iter().all(|(id, _, _)| id == FRESH), "{written:?}");
    }

    #[test]
    fn linked_progress_requires_native_launch_and_uses_the_latest_verdict() {
        let guard = LinkedProgress::from_args(&linked_guard_args())
            .unwrap()
            .unwrap();
        let source = run(SILENT, Some("2026-09-18T00:00:00Z"), None);
        let now = DateTime::parse_from_rfc3339("2026-09-18T06:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let live = linked_gate("g", SILENT, "2026-09-18T04:00:00Z", "open");
        assert!(guard.protects(&source, std::slice::from_ref(&live), now));
        let mut green = linked_gate("newer", SILENT, "2026-09-18T05:00:00Z", "closed");
        green["steps"].as_array_mut().unwrap().push(json!({
            "spec_slug":"green", "status":"completed", "completed_at":"2026-09-18T05:30:00Z"
        }));
        assert!(
            guard.protects(&source, &[live.clone(), green.clone()], now),
            "success pending settlement is not death"
        );
        green["steps"][2]["status"] = json!("skipped");
        assert!(
            !guard.protects(&source, &[live.clone(), green], now),
            "newer failed launch beats older open one"
        );
        assert!(!guard.protects(&source, &[], now));
        for (pointer, value) in [
            ("/metadata/agent_run", json!(FRESH)),
            ("/kind", json!("unrelated-kind")),
            ("/metadata/branch", json!(" ")),
            ("/metadata/sha", json!("not-a-frozen-sha")),
            ("/metadata/launched_at", json!("not-a-time")),
            ("/metadata/launched_at", json!("2026-09-18T07:00:00Z")),
            ("/steps/0/status", json!("pending")),
            ("/steps/0/completed_at", json!(null)),
            ("/steps/0/completed_at", json!("not-server-time")),
            ("/steps/0/completed_at", json!("2026-09-18T07:00:00Z")),
            ("/steps/1/status", json!("completed")),
            ("/status", json!("closed")),
        ] {
            let mut bad = live.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                !guard.protects(&source, &[bad], now),
                "invalid evidence at {pointer}"
            );
        }
    }

    #[tokio::test]
    async fn an_unreadable_linked_roster_refuses_without_writing_a_death() {
        let source = run(SILENT, Some("2026-09-18T00:00:00Z"), None);
        let stub = crate::handlers::listing_stub::serve(vec![
            (
                "/api/jobs?kind=agent-run",
                json!({"data":[source],"total":1}),
            ),
            ("/api/jobs?kind=gate-run", json!({"total":1})),
        ])
        .await;
        let handler =
            JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &stub.base);
        assert!(matches!(
            handler
                .invoke(&linked_guard_args(), &ctx(tick("2026-09-18T06:00:00Z")))
                .await,
            Err(HandlerError::Downstream(_))
        ));
        assert!(stub.writes().is_empty());
    }

    #[tokio::test]
    async fn linked_progress_reads_the_whole_roster_and_refuses_a_short_walk() {
        let source = run(SILENT, Some("2026-09-18T00:00:00Z"), None);
        let unrelated = (0..500)
            .map(|i| linked_gate(&format!("other-{i}"), FRESH, "2026-09-18T04:00:00Z", "open"))
            .collect::<Vec<_>>();
        for tail in [
            json!({"data":[linked_gate("last",SILENT,"2026-09-18T05:00:00Z","open")],"total":501}),
            json!({"data":[],"total":501}),
        ] {
            let complete = !tail["data"].as_array().unwrap().is_empty();
            let stub = crate::handlers::listing_stub::serve(vec![
                (
                    "/api/jobs?kind=agent-run",
                    json!({"data":[source.clone()],"total":1}),
                ),
                (
                    "/api/jobs?kind=gate-run&offset=0",
                    json!({"data":unrelated,"total":501}),
                ),
                ("/api/jobs?kind=gate-run&offset=500", tail),
            ])
            .await;
            let handler =
                JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &stub.base);
            let result = handler
                .invoke(&linked_guard_args(), &ctx(tick("2026-09-18T06:00:00Z")))
                .await;
            if complete {
                result.unwrap();
            } else {
                assert!(matches!(result, Err(HandlerError::Downstream(_))));
            }
            assert!(stub.writes().is_empty());
        }
    }

    #[tokio::test]
    async fn a_launch_after_the_firing_refuses_the_stale_silence_judgment() {
        let (base, puts) = mock_jobs(vec![
            run(SILENT, Some("2026-09-18T00:00:00Z"), None),
            linked_gate("g", SILENT, "2026-09-18T06:01:00Z", "open"),
        ])
        .await;
        let handler = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        assert!(matches!(
            handler
                .invoke(&linked_guard_args(), &ctx(tick("2026-09-18T06:00:00Z")))
                .await,
            Err(HandlerError::Downstream(_))
        ));
        assert!(
            puts.lock().unwrap().is_empty(),
            "a stale firing cannot declare observed later work dead"
        );
    }

    #[tokio::test]
    async fn success_after_the_firing_refuses_the_stale_silence_judgment() {
        let mut green = linked_gate("g", SILENT, "2026-09-18T05:00:00Z", "closed");
        green["steps"].as_array_mut().unwrap().push(json!({
            "spec_slug":"green", "status":"completed", "completed_at":"2026-09-18T06:01:00Z"
        }));
        let (base, puts) =
            mock_jobs(vec![run(SILENT, Some("2026-09-18T00:00:00Z"), None), green]).await;
        let handler = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        assert!(matches!(
            handler
                .invoke(&linked_guard_args(), &ctx(tick("2026-09-18T06:00:00Z")))
                .await,
            Err(HandlerError::Downstream(_))
        ));
        assert!(puts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unreadable_completed_launch_refuses_but_pending_launch_keeps_live_proof() {
        for stamp in [json!(null), json!("unreadable")] {
            let mut unknown = linked_gate("unknown", SILENT, "2026-09-18T05:30:00Z", "closed");
            unknown["steps"][0]["completed_at"] = stamp;
            let (base, puts) = mock_jobs(vec![
                run(SILENT, Some("2026-09-18T00:00:00Z"), None),
                linked_gate("older", SILENT, "2026-09-18T05:00:00Z", "open"),
                unknown,
            ])
            .await;
            let handler = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
            assert!(matches!(
                handler
                    .invoke(&linked_guard_args(), &ctx(tick("2026-09-18T06:00:00Z")))
                    .await,
                Err(HandlerError::Downstream(_))
            ));
            assert!(puts.lock().unwrap().is_empty());
        }
        let mut pending = linked_gate("pending", SILENT, "2026-09-18T05:30:00Z", "open");
        pending["steps"][0]["status"] = json!("pending");
        pending["steps"][0]["completed_at"] = json!(null);
        let (base, puts) = mock_jobs(vec![
            run(SILENT, Some("2026-09-18T00:00:00Z"), None),
            linked_gate("older", SILENT, "2026-09-18T05:00:00Z", "open"),
            pending,
        ])
        .await;
        let handler = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &base);
        handler
            .invoke(&linked_guard_args(), &ctx(tick("2026-09-18T06:00:00Z")))
            .await
            .unwrap();
        assert!(puts.lock().unwrap().is_empty());
    }

    #[test]
    fn valid_launch_timestamp_rounding_is_progress_regression() {
        let guard = LinkedProgress::from_args(&linked_guard_args())
            .unwrap()
            .unwrap();
        let source = run(SILENT, Some("2026-09-18T00:00:00Z"), None);
        let now = DateTime::parse_from_rfc3339("2026-09-18T06:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut gate = linked_gate("g", SILENT, "2026-09-18T05:00:00.880636Z", "open");
        gate["metadata"]["launched_at"] = json!("2026-09-18T05:00:00Z");
        assert!(
            guard.protects(&source, &[gate], now),
            "CLI seconds may precede the server microsecond stamp"
        );
    }

    #[tokio::test]
    async fn equal_launch_timestamps_refuse_in_either_order_regression() {
        let source = run(SILENT, Some("2026-09-18T00:00:00Z"), None);
        let live = linked_gate("a", SILENT, "2026-09-18T05:00:00Z", "open");
        let failed = linked_gate("b", SILENT, "2026-09-18T05:00:00Z", "closed");
        for gates in [vec![live.clone(), failed.clone()], vec![failed, live]] {
            let stub = crate::handlers::listing_stub::serve(vec![
                (
                    "/api/jobs?kind=agent-run",
                    json!({"data":[source.clone()],"total":1}),
                ),
                ("/api/jobs?kind=gate-run", json!({"data":gates,"total":2})),
            ])
            .await;
            let handler =
                JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &stub.base);
            assert!(matches!(
                handler
                    .invoke(&linked_guard_args(), &ctx(tick("2026-09-18T06:00:00Z")))
                    .await,
                Err(HandlerError::Downstream(_))
            ));
            assert!(stub.writes().is_empty());
        }
    }

    #[tokio::test]
    async fn inconsistent_linked_pages_refuse_before_a_verdict_regression() {
        let source = run(SILENT, Some("2026-09-18T00:00:00Z"), None);
        let first = (0..500)
            .map(|i| linked_gate(&format!("other-{i}"), FRESH, "2026-09-18T04:00:00Z", "open"))
            .collect::<Vec<_>>();
        for tail in [
            json!({"data":[first[0]],"total":501}),
            json!({"data":[linked_gate("tail",FRESH,"2026-09-18T04:00:00Z","open")],"total":502}),
            json!({"data":[linked_gate("tail",FRESH,"2026-09-18T04:00:00Z","open"),linked_gate("surplus",FRESH,"2026-09-18T04:00:00Z","open")],"total":501}),
        ] {
            let stub = crate::handlers::listing_stub::serve(vec![
                (
                    "/api/jobs?kind=agent-run",
                    json!({"data":[source.clone()],"total":1}),
                ),
                (
                    "/api/jobs?kind=gate-run&offset=0",
                    json!({"data":first,"total":501}),
                ),
                ("/api/jobs?kind=gate-run&offset=500", tail),
            ])
            .await;
            let handler =
                JobsAgeOutStep::with_client(crate::handlers::common::api_client(), &stub.base);
            assert!(matches!(
                handler
                    .invoke(&linked_guard_args(), &ctx(tick("2026-09-18T06:00:00Z")))
                    .await,
                Err(HandlerError::Downstream(_))
            ));
            assert!(stub.writes().is_empty());
        }
    }

    #[test]
    fn the_authored_silence_rule_declares_the_same_progress_guard() {
        let path = boss_testing::repo_root()
            .join("infra/dispatcher/rules/agent-run-dies-when-building-is-silent.toml");
        let rule: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let raw = rule["rule"][0]["do"][0]["args"]["unless_linked_job_proves_progress"]
            .as_str()
            .unwrap();
        let value: String = serde_json::from_str(raw).unwrap();
        let expected = linked_guard_args().pop().unwrap().1;
        let Value::String(expected) = expected else {
            panic!("guard JSON string");
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&value).unwrap(),
            serde_json::from_str::<serde_json::Value>(&expected).unwrap()
        );
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsAgeOutStep::with_client(crate::handlers::common::api_client(), "http://unused");
        assert_eq!(h.name(), "jobs.age_out_step");
        assert_eq!(
            crate::cascade::handler_emits()
                .get("jobs.age_out_step")
                .cloned(),
            Some(vec!["jobs.step.completed", "agents.run.recorded"]),
            "the cascade table knows what this handler emits"
        );
    }
}
