//! `jobs.return_stranded_review` — a review step NOMINATED to its own
//! car's builder, or nominated to anyone and left unclaimed past a
//! bound, goes back to its station's queue (backlog eb86a174, the first
//! rule of "the factory routes stranded work itself").
//!
//! ## The case
//!
//! David, 2026-11-15: "The factory is supposed to handle this
//! automatically so we aren't relying on executor attention to keep work
//! moving." Measured the same evening, 18:0xZ, over the 136 open
//! `ship-a-change` cars: 47 READY review steps carried a named holder
//! nobody had claimed for 7.6 to 13.9 hours — 41 `landed-review`s on
//! agent-claude-b and 6 held `review`s on agent-codex — every one put
//! there by a hand's `boss step nominate`, each carrying its
//! `nomination_requested` record. Forty of the 41 landed reviews were
//! named to the agent that BUILT the car (its builder run's `building`
//! step was agent-claude-b's); the nomination's own `why` read "builder
//! recorded only as the auto-park automation", because those cars were
//! opened at green and carry no `built_by`. Since the matchmaker was
//! retired (backlog e1eaf72b) work stands at its station and an executor
//! PULLS it, and the pull walks past a step held by someone else — so a
//! nomination nobody acts on is a step no other reviewer can reach.
//!
//! ## What it returns, and why each
//!
//! Only a READY step with a holder — a nomination. A claimed (`active`)
//! step is somebody's work in flight and is never taken here; the dead-
//! run reclaim and the urgent watch own that case. Only a step whose
//! agent block declares the rule's `profile` (`reviewer`), the
//! predicate the station pull's own-review exclusion reads
//! (`boss_jobs::station_claim`), so no step slug is matched. And a step
//! whose `agent_run` edge names a run still OPEN is left alone: a
//! dispatched reviewer between its filing and its claim is not stranded.
//!
//! - **Its builder.** A review by the car's builder is no review
//!   (`boss dispatch` refuses the builder's run by name). The builder is
//!   the car's `built_by`, else the holder of its builder run's
//!   `building` step, each read through the agents registry's id-or-
//!   alias so `claude-b@algedonic.dev` and `agent-claude-b` are one
//!   actor. Returned on its own assignment marker, or on the first tick
//!   that sees it, whatever its age.
//! - **Unclaimed past the bound.** A nomination is an intent; the
//!   station is the address. When the `nomination_requested` record
//!   naming this holder is older than `unclaimed_minutes`, the step goes
//!   back. A nomination with no such record cannot be dated, and is left
//!   alone and said so — no evidence is not a pass.
//!
//! ## The builder the queue must keep out
//!
//! The station pull refuses a reviewer profile to the actor `built_by`
//! names, and nothing else. A car opened at green carries none, so a
//! step returned to the queue would be pullable by its builder — the
//! very defect it was returned for. When the builder was read off the
//! builder run and the car carries no `built_by`, the return stamps it
//! (the agents row's id) through the job merge door before the step
//! moves, and the step's note names the run it was read from.
//!
//! ## The writes
//!
//! The step merge door first, JUDGED against the `nomination_requested`
//! it read (`x-boss-expect-metadata`, cd1f68c6), so a re-nomination
//! between the read and the write is left as it stands: it retracts that
//! record and lands the note under [`NOTE_KEY`] — not the reclaim's
//! `reclaimed`, the hand verb's `released`, the urgent watch's
//! `released_unrun` or the executor arm's `executor_refused`, so a
//! reader can tell which door moved it. Then the step PUT returns it to
//! `ready` and nobody's, carrying no metadata (e39a9d2a). The step's
//! `agent_run` edge is NOT touched: on a held car's review it names the
//! reviewer whose verdict the release door reads.
//!
//! ## Two triggers, one handler (backlog c9cbf382)
//!
//! The fifteen-minute sweep answered a nomination of the builder up to
//! fifteen minutes after it was made, and for that window the step stood
//! where no other reviewer's pull could reach it. `boss step nominate`
//! merges its `nomination_requested` record and THEN puts the assignee,
//! and the put emits `step.assigned.<kind>` with the step and packet ids
//! — so the twin rule on that marker answers the builder half the moment
//! the nomination lands. A payload carrying a `job_id` and a `step_id`
//! is the marker; anything else is the tick. The marker judges only the
//! builder half: a nomination is never past the unclaimed bound at the
//! instant it is made, and a stale record beside a fresh assignment is
//! the sweep's to judge, not the marker's. So the sweep must declare
//! `unclaimed_minutes` and the marker must not, and each refuses the
//! other's declaration by name.
//!
//! ## Idempotent, and no loop
//!
//! A returned step is unassigned, so the next tick passes over it. The
//! return's writes emit `jobs.job.updated` and `jobs.step.updated`, and
//! its put assigns nobody, so it never announces the `step.assigned`
//! marker the twin listens on (the jobs API emits it only for a new,
//! non-empty assignee). A hand that names the builder again is answered
//! again on its own marker; one that names another reviewer starts a
//! fresh bound from its own record.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value as ArgValue;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use boss_jobs::car_release::{BUILDING_SLUG, builder_run};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use super::common::{
    Judged, api_client, empty_roster_refusal, get_json, jobs_where_slim, row_or_refuse,
    rows_or_refuse, write_json, write_json_expecting,
};
use super::jobs_reclaim_abandoned_step::release_put;

/// The handler's registered name.
pub const HANDLER: &str = "jobs.return_stranded_review";

/// Where the return's evidence lands on the step.
pub const NOTE_KEY: &str = "returned_to_station";

/// The intent `boss step nominate` records beside the assignee it PUTs
/// (`crates/orchestrators/boss-cli/src/steps.rs`, `nominate_step`).
const NOMINATION_KEY: &str = "nomination_requested";

/// The car key naming who built it (`boss_jobs::car::build_start`), the
/// one the station pull's own-review exclusion reads.
const BUILT_BY: &str = "built_by";

/// The run edge `boss dispatch` writes on the step it dispatches.
const EDGE_KEY: &str = "agent_run";

/// The rule's declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    /// The packet kind whose open board is read.
    pub kind: String,
    /// The agent profile a step must declare to be judged here.
    pub profile: String,
    /// How long a nomination may stand unclaimed: the sweep's bound,
    /// required on the clock and refused on the marker (module doc).
    pub unclaimed_minutes: Option<i64>,
}

/// Parse the rule's args. `kind` and `profile` are required, and a
/// malformed arg refuses by name rather than vanishing. Whether the bound
/// must be declared is the trigger's to say, so [`Handler::invoke`]
/// judges that: the sweep without one is half a rule that is not
/// running, and the marker with one would run the half it must not.
pub fn declaration(args: &[(String, ArgValue)]) -> Result<Declaration, String> {
    let text = |name: &str| match arg(args, name) {
        Some(ArgValue::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        Some(other) => Err(format!(
            "`{name}` must be a non-empty string, got a {} ({other:?})",
            other.kind()
        )),
        None => Err(format!("`{name}` is not declared on the rule")),
    };
    let unclaimed_minutes = match arg(args, "unclaimed_minutes") {
        Some(ArgValue::Int(n)) if *n > 0 => Some(*n),
        Some(other) => {
            return Err(format!(
                "`unclaimed_minutes` must be a positive whole number, got a {} ({other:?})",
                other.kind()
            ));
        }
        None => None,
    };
    Ok(Declaration {
        kind: text("kind")?,
        profile: text("profile")?,
        unclaimed_minutes,
    })
}

/// The agents row's id for `who` — matched by id or alias — else `who`
/// as spelled. One actor, however it signed.
pub(crate) fn canonical(agents: &[Value], who: &str) -> String {
    let who = who.trim();
    agents
        .iter()
        .find(|a| {
            a.get("id").and_then(Value::as_str) == Some(who)
                || a.get("aliases")
                    .and_then(Value::as_array)
                    .is_some_and(|xs| xs.iter().any(|x| x.as_str() == Some(who)))
        })
        .and_then(|a| a.get("id").and_then(Value::as_str))
        .unwrap_or(who)
        .to_string()
}

fn text<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The car's builder as the car itself records it (`built_by`).
fn built_by(car: &Value) -> Option<&str> {
    text(car, "/metadata/built_by")
}

/// The actor that held a builder run's `building` step.
pub(crate) fn run_builder(run: &Value) -> Option<&str> {
    run.get("steps")
        .and_then(Value::as_array)?
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(BUILDING_SLUG))
        .and_then(|s| text(s, "/assignee_id").or_else(|| text(s, "/completed_by")))
}

/// Why a step goes back to its station.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Reason {
    /// Its holder built the car.
    Builder,
    /// Its holder's nomination stood unclaimed past the bound.
    Unclaimed { minutes: i64 },
}

impl Reason {
    fn word(&self) -> &'static str {
        match self {
            Reason::Builder => "builder",
            Reason::Unclaimed { .. } => "unclaimed",
        }
    }
}

/// What the rule makes of one step.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Verdict {
    /// Not this rule's to move, with why.
    Leave(String),
    Return(Reason),
}

/// PURE: judge one step. `holder` and `builder` are canonical; `run_open`
/// says whether the step's `agent_run` edge names a run still open.
pub(crate) fn judge(
    step: &Value,
    holder: &str,
    builder: Option<&str>,
    agents: &[Value],
    decl: &Declaration,
    run_open: bool,
    now: DateTime<Utc>,
) -> Verdict {
    if step.get("status").and_then(Value::as_str) != Some("ready") {
        return Verdict::Leave("not a nomination: the step is not ready".into());
    }
    if text(step, "/metadata/agent_profile") != Some(decl.profile.as_str()) {
        return Verdict::Leave(format!("its agent block is not a `{}`", decl.profile));
    }
    if run_open {
        return Verdict::Leave("a dispatched run is open on it".into());
    }
    if builder == Some(holder) {
        return Verdict::Return(Reason::Builder);
    }
    let Some(bound) = decl.unclaimed_minutes else {
        return Verdict::Leave(
            "its holder did not build the car, and this firing judges only the builder half".into(),
        );
    };
    let nomination = step.pointer(&format!("/metadata/{NOMINATION_KEY}"));
    let named = nomination
        .and_then(|n| text(n, "/assignee_id"))
        .map(|who| canonical(agents, who));
    if named.as_deref() != Some(holder) {
        return Verdict::Leave(
            "no nomination record names its holder, so how long it has stood cannot be read".into(),
        );
    }
    let Some(at) = nomination
        .and_then(|n| text(n, "/at"))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
    else {
        return Verdict::Leave("its nomination record carries no readable `at`".into());
    };
    let minutes = (now - at).num_minutes();
    if minutes >= bound {
        Verdict::Return(Reason::Unclaimed { minutes })
    } else {
        Verdict::Leave(format!(
            "nominated {minutes} min ago, inside the {bound} min bound"
        ))
    }
}

/// PURE: the step merge that returns it — the note, and the holder's
/// nomination retracted.
#[allow(clippy::too_many_arguments)]
pub(crate) fn return_body(
    step: &Value,
    holder: &str,
    reason: &Reason,
    builder: Option<&str>,
    builder_from_run: Option<&str>,
    decl: &Declaration,
    at: &str,
    rule: &str,
) -> Value {
    let metadata = step.get("metadata").cloned().unwrap_or(Value::Null);
    let nomination = metadata.get(NOMINATION_KEY).cloned();
    let minutes = match reason {
        Reason::Unclaimed { minutes } => json!(minutes),
        Reason::Builder => Value::Null,
    };
    let note = json!({
        "holder": step.get("assignee_id"),
        "actor": holder,
        "reason": reason.word(),
        "builder": builder,
        "builder_from_run": builder_from_run,
        "nomination": nomination,
        "unclaimed_minutes": minutes,
        "bound_minutes": decl.unclaimed_minutes,
        "station": metadata.get("at"),
        "at": at,
        "rule": rule,
    });
    let mut body = json!({ NOTE_KEY: note });
    if nomination.is_some() {
        body[NOMINATION_KEY] = Value::Null;
    }
    body
}

pub struct JobsReturnStrandedReview {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsReturnStrandedReview {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Self::with_client(api_client(), jobs_base)
    }

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

    async fn packet(&self, id: &str, rule: &str) -> Result<Value, HandlerError> {
        let body = get_json(
            &self.client,
            &format!("{}/api/jobs/{id}", self.base()),
            rule,
        )
        .await?;
        row_or_refuse(body, &format!("GET /api/jobs/{id}")).map_err(HandlerError::Downstream)
    }

    /// The car's builder, canonical, and the run it was read from when
    /// the car itself does not record one. A builder run that cannot be
    /// read leaves the builder unknown and says so — the unclaimed half
    /// still judges the step, and one missing run never stops the sweep.
    async fn builder(
        &self,
        car: &Value,
        agents: &[Value],
        runs: &mut BTreeMap<String, Option<Value>>,
        rule: &str,
    ) -> (Option<String>, Option<String>) {
        if let Some(b) = built_by(car) {
            return (Some(canonical(agents, b)), None);
        }
        let Some(run_id) = builder_run(car) else {
            return (None, None);
        };
        let run = self.run(run_id, runs, rule).await;
        match run.as_ref().and_then(run_builder) {
            Some(b) => (Some(canonical(agents, b)), Some(run_id.to_string())),
            None => (None, None),
        }
    }

    /// A run row, read once per firing; `None` when it cannot be read.
    async fn run(
        &self,
        id: &str,
        runs: &mut BTreeMap<String, Option<Value>>,
        rule: &str,
    ) -> Option<Value> {
        if !runs.contains_key(id) {
            let read = match self.packet(id, rule).await {
                Ok(row) => Some(row),
                Err(e) => {
                    tracing::warn!(rule, run = %id, "the run could not be read: {e}");
                    None
                }
            };
            runs.insert(id.to_string(), read);
        }
        runs.get(id).cloned().flatten()
    }

    /// Return one step. `Ok(true)` when it moved.
    #[allow(clippy::too_many_arguments)]
    async fn give_back(
        &self,
        car: &Value,
        step: &Value,
        holder: &str,
        reason: &Reason,
        builder: Option<&str>,
        builder_from_run: Option<&str>,
        decl: &Declaration,
        at: &str,
        rule: &str,
    ) -> Result<bool, HandlerError> {
        let (Some(car_id), Some(step_id)) = (text(car, "/id"), text(step, "/id")) else {
            return Ok(false);
        };
        // The builder the queue must keep out (module doc): stamped
        // before the step moves, so the pull never sees it unguarded.
        if let (Some(b), Some(_)) = (builder, builder_from_run)
            && built_by(car).is_none()
        {
            write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!("{}/api/jobs/{car_id}/metadata", self.base()),
                &json!({ BUILT_BY: b }),
                rule,
            )
            .await?;
        }
        let url = format!("{}/api/jobs/{car_id}/steps/{step_id}", self.base());
        let mut expect = Map::new();
        expect.insert(
            NOMINATION_KEY.into(),
            step.pointer(&format!("/metadata/{NOMINATION_KEY}"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        let body = return_body(
            step,
            holder,
            reason,
            builder,
            builder_from_run,
            decl,
            at,
            rule,
        );
        let judged = write_json_expecting(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{url}/metadata"),
            &body,
            &expect,
            rule,
        )
        .await?;
        if judged == Judged::Moved {
            tracing::info!(
                rule, packet = %car_id, step = %step_id,
                "left: its nomination changed between the read and the return"
            );
            return Ok(false);
        }
        write_json(
            &self.client,
            reqwest::Method::PUT,
            &url,
            &release_put(),
            rule,
        )
        .await?;
        tracing::info!(
            rule, packet = %car_id, step = %step_id, holder, reason = reason.word(),
            "returned to its station's queue"
        );
        Ok(true)
    }

    /// The agents registry. An empty read is refused, never "no agents".
    async fn agents(&self, rule: &str) -> Result<Vec<Value>, HandlerError> {
        let body = get_json(&self.client, &format!("{}/api/agents", self.base()), rule).await?;
        let agents: Vec<Value> =
            rows_or_refuse(&body, "GET /api/agents").map_err(HandlerError::Downstream)?;
        if agents.is_empty() {
            return Err(empty_roster_refusal(
                HANDLER,
                "agents",
                "every instance runs at least one registered agent, and an empty read is a dark \
                 or narrowed registry — read so, a builder spelled by its login would not be \
                 its agent, and its own review would stay on it",
            ));
        }
        Ok(agents)
    }

    /// Judge one step of a car read whole, and return it when the verdict
    /// says so. `Ok(true)` when it moved. Both triggers come through here.
    async fn consider(
        &self,
        car: &Value,
        step: &Value,
        firing: &Firing<'_>,
        runs: &mut BTreeMap<String, Option<Value>>,
    ) -> Result<bool, HandlerError> {
        let Firing {
            agents,
            decl,
            now,
            at,
            rule,
        } = *firing;
        let Some(raw_holder) = nominee(step) else {
            return Ok(false);
        };
        if text(step, "/metadata/agent_profile") != Some(decl.profile.as_str()) {
            return Ok(false);
        }
        let holder = canonical(agents, raw_holder);
        let run_open = match text(step, &format!("/metadata/{EDGE_KEY}")) {
            Some(run_id) => self.run(run_id, runs, rule).await.is_none_or(|run| {
                // Unreadable is read as open: the step is left.
                run.get("status").and_then(Value::as_str) == Some("open")
            }),
            None => false,
        };
        let (builder, from_run) = self.builder(car, agents, runs, rule).await;
        match judge(
            step,
            &holder,
            builder.as_deref(),
            agents,
            decl,
            run_open,
            now,
        ) {
            Verdict::Leave(why) => {
                tracing::debug!(rule, packet = ?text(car, "/id"), holder, "left: {why}");
                Ok(false)
            }
            Verdict::Return(reason) => {
                self.give_back(
                    car,
                    step,
                    &holder,
                    &reason,
                    builder.as_deref(),
                    from_run.as_deref(),
                    decl,
                    at,
                    rule,
                )
                .await
            }
        }
    }

    /// THE MARKER (backlog c9cbf382): a `step.assigned.<kind>` names one
    /// step, and only that step is judged, on a fresh read of its packet —
    /// the payload's own metadata filtered the firing (the rule's `when`)
    /// but never decides. A claim, a packet of another kind, or one no
    /// longer open is passed over before the agents registry is read.
    async fn answer_assignment(
        &self,
        job_id: &str,
        step_id: &str,
        decl: &Declaration,
        now: DateTime<Utc>,
        at: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let car = self.packet(job_id, rule).await?;
        if text(&car, "/kind") != Some(decl.kind.as_str()) || text(&car, "/status") != Some("open")
        {
            tracing::debug!(rule, packet = %job_id, "left: not an open `{}`", decl.kind);
            return Ok(());
        }
        let Some(step) = car
            .get("steps")
            .and_then(Value::as_array)
            .and_then(|steps| {
                steps
                    .iter()
                    .find(|s| s.get("id").and_then(Value::as_str) == Some(step_id))
            })
            .cloned()
        else {
            return Ok(());
        };
        if nominee(&step).is_none()
            || text(&step, "/metadata/agent_profile") != Some(decl.profile.as_str())
        {
            tracing::debug!(rule, packet = %job_id, step = %step_id, "left: not a nominated review");
            return Ok(());
        }
        let agents = self.agents(rule).await?;
        let firing = Firing {
            agents: &agents,
            decl,
            now,
            at,
            rule,
        };
        let returned = self
            .consider(&car, &step, &firing, &mut BTreeMap::new())
            .await?;
        tracing::info!(
            rule, packet = %job_id, step = %step_id, returned,
            "answered a review nomination on its marker"
        );
        Ok(())
    }
}

/// What one firing judges with: the registry read once, the rule's
/// declaration, and the instant it stamps.
#[derive(Clone, Copy)]
struct Firing<'a> {
    agents: &'a [Value],
    decl: &'a Declaration,
    now: DateTime<Utc>,
    at: &'a str,
    rule: &'a str,
}

/// A READY step's holder, trimmed.
fn nominee(step: &Value) -> Option<&str> {
    (step.get("status").and_then(Value::as_str) == Some("ready"))
        .then(|| text(step, "/assignee_id"))
        .flatten()
}

#[async_trait]
impl Handler for JobsReturnStrandedReview {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, ArgValue)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let decl = declaration(args).map_err(HandlerError::Permanent)?;
        let rule = ctx.rule_name.as_str();
        let now = ctx.firing_instant()?;
        let at = now.to_rfc3339();
        let payload = &ctx.event_payload;
        // THE MARKER: one step, named by the assignment it announces.
        if let (Some(job_id), Some(step_id)) = (
            payload.get("job_id").and_then(Value::as_str),
            payload.get("step_id").and_then(Value::as_str),
        ) {
            if decl.unclaimed_minutes.is_some() {
                return Err(HandlerError::Permanent(format!(
                    "rule {rule:?} on {}: `unclaimed_minutes` is the sweep's bound — a \
                     nomination is never past it at the moment it is made, and the marker \
                     judges only the builder half; drop the arg from this rule",
                    ctx.triggering_topic
                )));
            }
            return self
                .answer_assignment(job_id, step_id, &decl, now, &at, rule)
                .await;
        }
        if decl.unclaimed_minutes.is_none() {
            return Err(HandlerError::Permanent(format!(
                "rule {rule:?} on {}: `unclaimed_minutes` is not declared on the rule — the \
                 sweep's bound is the rule's to state, and without it the unclaimed half is \
                 not running",
                ctx.triggering_topic
            )));
        }
        // THE SWEEP: the board slim; only a packet holding a nomination
        // is read whole.
        let board = jobs_where_slim(
            &self.client,
            self.base(),
            &format!("kind={}&status=open", decl.kind),
            rule,
        )
        .await?;
        let held: Vec<&str> = board
            .iter()
            .filter(|job| {
                job.get("steps")
                    .and_then(Value::as_array)
                    .is_some_and(|steps| steps.iter().any(|s| nominee(s).is_some()))
            })
            .filter_map(|job| text(job, "/id"))
            .collect();
        if held.is_empty() {
            tracing::info!(
                rule,
                packets = board.len(),
                returned = 0,
                "no nomination stands"
            );
            return Ok(());
        }
        let agents = self.agents(rule).await?;
        let firing = Firing {
            agents: &agents,
            decl: &decl,
            now,
            at: &at,
            rule,
        };
        let mut runs: BTreeMap<String, Option<Value>> = BTreeMap::new();
        let mut returned = 0usize;
        for id in held {
            // The record as it stands now; the slim row carries no step
            // metadata, and only this read decides.
            let car = self.packet(id, rule).await?;
            let steps: Vec<Value> = car
                .get("steps")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for step in steps.iter() {
                if self.consider(&car, step, &firing, &mut runs).await? {
                    returned += 1;
                }
            }
        }
        tracing::info!(
            rule,
            packets = board.len(),
            returned,
            "returned stranded review nominations to their station's queue"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    const NOW: &str = "2026-11-15T18:00:00Z";
    const RULE: &str = "a-stranded-review-nomination-returns-to-its-station";
    const BOARD: &str = "/api/jobs?kind=ship-a-change&status=open";
    const CAR: &str = "/api/jobs/car-1";
    const RUN: &str = "/api/jobs/run-b";
    const STATION: &str = "f675e19a-7c36-453f-bbac-e206aced63ab";

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(NOW)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn decl() -> Declaration {
        Declaration {
            kind: "ship-a-change".into(),
            profile: "reviewer".into(),
            unclaimed_minutes: Some(120),
        }
    }

    fn args() -> Vec<(String, ArgValue)> {
        vec![
            ("kind".into(), ArgValue::String("ship-a-change".into())),
            ("profile".into(), ArgValue::String("reviewer".into())),
            ("unclaimed_minutes".into(), ArgValue::Int(120)),
        ]
    }

    fn agents() -> Vec<Value> {
        vec![
            json!({"id": "agent-claude", "aliases": ["claude@algedonic.dev"]}),
            json!({"id": "agent-claude-b", "aliases": ["claude-b@algedonic.dev"]}),
            json!({"id": "agent-codex", "aliases": ["codex@algedonic.dev"]}),
        ]
    }

    /// A ready reviewer step nominated to `holder` at `nominated_at`.
    fn review(holder: &str, nominated_at: Option<&str>) -> Value {
        let mut md = json!({
            "agent_profile": "reviewer", "agent_model": "opus-5-5",
            "at": STATION, "authority_role": "platform-admin",
        });
        if let Some(t) = nominated_at {
            md[NOMINATION_KEY] = json!({"assignee_id": holder, "at": t,
                "by": "claude@algedonic.dev", "why": "rebalance"});
        }
        json!({"id": "step-lr", "spec_slug": "landed-review", "status": "ready",
               "assignee_id": holder, "metadata": md})
    }

    fn car(built_by: Option<&str>, step: Value) -> Value {
        let mut md = json!({"agent_run": "run-b", "merged": "true"});
        if let Some(b) = built_by {
            md[BUILT_BY] = json!(b);
        }
        json!({"id": "car-1", "kind": "ship-a-change", "status": "open",
               "metadata": md, "steps": [step]})
    }

    fn builder_run_row(holder: &str) -> Value {
        json!({"id": "run-b", "kind": "agent-run", "status": "closed",
               "steps": [{"id": "b1", "spec_slug": "building", "assignee_id": holder,
                          "status": "completed"}]})
    }

    fn listing(rows: Vec<Value>) -> Value {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    fn slim(car: &Value) -> Value {
        let mut s = car.clone();
        for step in s["steps"].as_array_mut().unwrap() {
            step.as_object_mut().unwrap().remove("metadata");
        }
        s
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: RULE.into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_at": NOW}),
        }
    }

    // -- the verdict ------------------------------------------------------

    /// THE MEASURED CASE: a landed review named to the agent that built
    /// the car goes back at once, whatever the nomination's age.
    #[test]
    fn a_review_named_to_its_builder_goes_back_at_once() {
        let step = review("agent-claude-b", Some("2026-11-15T17:59:00Z"));
        assert_eq!(
            judge(
                &step,
                "agent-claude-b",
                Some("agent-claude-b"),
                &agents(),
                &decl(),
                false,
                now()
            ),
            Verdict::Return(Reason::Builder)
        );
    }

    #[test]
    fn a_nomination_unclaimed_past_the_bound_goes_back() {
        let step = review("agent-codex", Some("2026-11-15T10:24:00Z"));
        assert_eq!(
            judge(
                &step,
                "agent-codex",
                Some("agent-claude-b"),
                &agents(),
                &decl(),
                false,
                now()
            ),
            Verdict::Return(Reason::Unclaimed { minutes: 456 })
        );
    }

    #[test]
    fn a_fresh_nomination_of_another_reviewer_stands() {
        let step = review("agent-codex", Some("2026-11-15T17:00:00Z"));
        assert!(matches!(
            judge(
                &step,
                "agent-codex",
                Some("agent-claude-b"),
                &agents(),
                &decl(),
                false,
                now()
            ),
            Verdict::Leave(_)
        ));
    }

    /// The nomination's own record spells the holder by login: one actor.
    #[test]
    fn a_nomination_spelled_by_login_is_its_agents_nomination() {
        let mut step = review("agent-codex", Some("2026-11-15T10:00:00Z"));
        step["metadata"][NOMINATION_KEY]["assignee_id"] = json!("codex@algedonic.dev");
        assert!(matches!(
            judge(&step, "agent-codex", None, &agents(), &decl(), false, now()),
            Verdict::Return(Reason::Unclaimed { .. })
        ));
    }

    /// No evidence is not a pass: an undated nomination is left.
    #[test]
    fn an_undated_nomination_is_left_and_said_so() {
        let step = review("agent-codex", None);
        let Verdict::Leave(why) =
            judge(&step, "agent-codex", None, &agents(), &decl(), false, now())
        else {
            panic!("an undated nomination must be left");
        };
        assert!(why.contains("cannot be read"), "{why}");
    }

    #[test]
    fn a_claim_another_profile_or_an_open_run_is_never_taken() {
        let mut active = review("agent-claude-b", Some("2026-11-15T01:00:00Z"));
        active["status"] = json!("active");
        let mut build = review("agent-claude-b", Some("2026-11-15T01:00:00Z"));
        build["metadata"]["agent_profile"] = json!("builder");
        let open = review("agent-claude-b", Some("2026-11-15T01:00:00Z"));
        let b = Some("agent-claude-b");
        for (step, run_open) in [(&active, false), (&build, false), (&open, true)] {
            assert!(
                matches!(
                    judge(
                        step,
                        "agent-claude-b",
                        b,
                        &agents(),
                        &decl(),
                        run_open,
                        now()
                    ),
                    Verdict::Leave(_)
                ),
                "{step}"
            );
        }
    }

    #[test]
    fn the_actor_is_one_however_it_signed() {
        assert_eq!(
            canonical(&agents(), "claude-b@algedonic.dev"),
            "agent-claude-b"
        );
        assert_eq!(canonical(&agents(), "agent-claude-b"), "agent-claude-b");
        assert_eq!(canonical(&agents(), "emp-david"), "emp-david");
        assert_eq!(
            run_builder(&builder_run_row("agent-claude-b")),
            Some("agent-claude-b")
        );
    }

    #[test]
    fn every_arg_is_the_rules_to_state() {
        assert_eq!(declaration(&args()), Ok(decl()));
        for name in ["kind", "profile"] {
            let missing: Vec<_> = args().into_iter().filter(|(k, _)| k != name).collect();
            assert!(declaration(&missing).unwrap_err().contains(name), "{name}");
        }
        // The bound is the sweep's to state; which trigger needs it is
        // the invocation's to judge (`each_trigger_refuses_...`).
        let unbound: Vec<_> = args()
            .into_iter()
            .filter(|(k, _)| k != "unclaimed_minutes")
            .collect();
        assert_eq!(declaration(&unbound).map(|d| d.unclaimed_minutes), Ok(None));
        let mut zero = args();
        zero[2].1 = ArgValue::Int(0);
        assert!(declaration(&zero).is_err());
    }

    // -- end to end, against the stub jobs API ---------------------------

    async fn serve(full: Value, run: Value) -> listing_stub::Stub {
        listing_stub::serve(vec![
            (BOARD, listing(vec![slim(&full)])),
            ("/api/agents", listing(agents())),
            (RUN, run),
            (CAR, full),
        ])
        .await
    }

    /// PINNED: a car opened at green carries no `built_by`; its builder
    /// is read off the builder run, stamped on the car so the station
    /// pull keeps it out, and only then does the step go back — the
    /// merge (judged, nomination retracted, note) before the status.
    #[tokio::test]
    async fn a_landed_review_named_to_its_builder_returns_to_the_station() {
        let full = car(None, review("agent-claude-b", Some("2026-11-15T17:59:00Z")));
        let stub = serve(full, builder_run_row("agent-claude-b")).await;
        let h = JobsReturnStrandedReview::with_client(api_client(), stub.base.clone());
        h.invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        let paths: Vec<&str> = sent.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            [
                "PATCH /api/jobs/car-1/metadata",
                "PATCH /api/jobs/car-1/steps/step-lr/metadata",
                "PUT /api/jobs/car-1/steps/step-lr",
            ],
            "{sent:?}"
        );
        assert_eq!(sent[0].1, json!({"built_by": "agent-claude-b"}));
        let merge = &sent[1].1;
        assert_eq!(
            merge[NOMINATION_KEY],
            Value::Null,
            "the nomination is retracted"
        );
        assert!(
            merge.get(EDGE_KEY).is_none(),
            "the run edge is never touched"
        );
        let note = &merge[NOTE_KEY];
        assert_eq!(note["reason"], "builder");
        assert_eq!(note["builder"], "agent-claude-b");
        assert_eq!(note["builder_from_run"], "run-b");
        assert_eq!(note["station"], STATION);
        assert_eq!(note["at"], "2026-11-15T18:00:00+00:00");
        assert_eq!(
            sent[2].1,
            json!({"status": "ready", "assignee_id": null}),
            "the PUT carries no metadata"
        );
    }

    /// A car that records its builder is not written; the step goes back
    /// on its age alone.
    #[tokio::test]
    async fn an_old_nomination_returns_without_touching_the_car() {
        let full = car(
            Some("claude-b@algedonic.dev"),
            review("agent-codex", Some("2026-11-15T10:24:00Z")),
        );
        let stub = serve(full, builder_run_row("agent-claude-b")).await;
        let h = JobsReturnStrandedReview::with_client(api_client(), stub.base.clone());
        h.invoke(&args(), &ctx()).await.unwrap();
        let writes = stub.writes();
        assert_eq!(
            writes,
            [
                "PATCH /api/jobs/car-1/steps/step-lr/metadata",
                "PUT /api/jobs/car-1/steps/step-lr",
            ]
        );
        assert_eq!(stub.read_count(RUN), 0, "built_by answers; no run read");
        assert_eq!(stub.sent()[0].1[NOTE_KEY]["reason"], "unclaimed");
    }

    /// Idempotent: a step already back at its station holds no nominee,
    /// and nothing is read whole or written.
    #[tokio::test]
    async fn a_returned_step_is_passed_over() {
        let mut step = review("agent-claude-b", None);
        step["assignee_id"] = Value::Null;
        let stub = serve(car(None, step), builder_run_row("agent-claude-b")).await;
        let h = JobsReturnStrandedReview::with_client(api_client(), stub.base.clone());
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
        assert_eq!(stub.read_count(CAR), 0);
    }

    /// An empty agents registry is a dark read, never "no agents".
    #[tokio::test]
    async fn an_empty_agents_registry_refuses() {
        let full = car(None, review("agent-claude-b", Some("2026-11-15T17:59:00Z")));
        let stub = listing_stub::serve(vec![
            (BOARD, listing(vec![slim(&full)])),
            ("/api/agents", listing(vec![])),
            (CAR, full),
        ])
        .await;
        let h = JobsReturnStrandedReview::with_client(api_client(), stub.base.clone());
        let err = h.invoke(&args(), &ctx()).await.unwrap_err();
        assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
        assert!(stub.writes().is_empty());
    }

    // -- the assignment marker: the builder half, at once (c9cbf382) ------

    const EVENT_RULE: &str = "a-review-nominated-to-its-builder-returns-to-its-station";

    /// The event rule declares no bound: it judges only the builder half.
    fn event_args() -> Vec<(String, ArgValue)> {
        args()
            .into_iter()
            .filter(|(k, _)| k != "unclaimed_minutes")
            .collect()
    }

    /// The `step.assigned.<kind>` marker a nomination emits
    /// (`boss_jobs::http::steps`): the step and packet ids, the new
    /// assignee and the step's metadata.
    fn assigned(step: &Value) -> InvocationContext {
        InvocationContext {
            event_timestamp: Some(now()),
            rule_name: EVENT_RULE.into(),
            triggering_event_id: "evt-assigned-1".into(),
            triggering_topic: "step.assigned.task".into(),
            event_payload: json!({
                "job_id": "car-1", "step_id": "step-lr", "kind": "task",
                "assignee_id": step["assignee_id"], "metadata": step["metadata"],
            }),
        }
    }

    /// Only the car and its run are routed: the board is never read.
    async fn serve_one(full: Value, run: Value) -> listing_stub::Stub {
        listing_stub::serve(vec![
            ("/api/agents", listing(agents())),
            (RUN, run),
            (CAR, full),
        ])
        .await
    }

    #[test]
    fn without_a_bound_only_the_builder_half_is_judged() {
        let no_bound = Declaration {
            unclaimed_minutes: None,
            ..decl()
        };
        let mine = review("agent-claude-b", Some("2026-11-15T17:59:00Z"));
        assert_eq!(
            judge(
                &mine,
                "agent-claude-b",
                Some("agent-claude-b"),
                &agents(),
                &no_bound,
                false,
                now()
            ),
            Verdict::Return(Reason::Builder)
        );
        // Nominated eleven hours ago to another reviewer: the sweep's
        // bound would return it; the marker never does.
        let old = review("agent-codex", Some("2026-11-15T07:00:00Z"));
        assert!(matches!(
            judge(
                &old,
                "agent-codex",
                Some("agent-claude-b"),
                &agents(),
                &no_bound,
                false,
                now()
            ),
            Verdict::Leave(_)
        ));
    }

    /// THE CLAIM (c9cbf382): a nomination of the car's own builder is
    /// answered on the marker it emits — no tick, no board read — with
    /// the same three writes the sweep makes.
    #[tokio::test]
    async fn a_nomination_of_the_builder_is_answered_on_its_marker() {
        let step = review("agent-claude-b", Some("2026-11-15T17:59:00Z"));
        let stub = serve_one(car(None, step.clone()), builder_run_row("agent-claude-b")).await;
        let h = JobsReturnStrandedReview::with_client(api_client(), stub.base.clone());
        h.invoke(&event_args(), &assigned(&step)).await.unwrap();
        assert_eq!(
            stub.writes(),
            [
                "PATCH /api/jobs/car-1/metadata",
                "PATCH /api/jobs/car-1/steps/step-lr/metadata",
                "PUT /api/jobs/car-1/steps/step-lr",
            ]
        );
        assert_eq!(stub.read_count(BOARD), 0, "the marker reads no board");
        let note = &stub.sent()[1].1[NOTE_KEY];
        assert_eq!(note["reason"], "builder");
        assert_eq!(note["rule"], EVENT_RULE);
        assert_eq!(note["bound_minutes"], Value::Null);
    }

    /// The marker never applies the unclaimed half, however old the
    /// nomination record it finds.
    #[tokio::test]
    async fn a_marker_for_another_reviewer_returns_nothing() {
        let step = review("agent-codex", Some("2026-11-15T07:00:00Z"));
        let stub = serve_one(
            car(Some("agent-claude-b"), step.clone()),
            builder_run_row("agent-claude-b"),
        )
        .await;
        let h = JobsReturnStrandedReview::with_client(api_client(), stub.base.clone());
        h.invoke(&event_args(), &assigned(&step)).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// A claim (`active`), a packet of another kind, and a closed car are
    /// not this rule's, and the agents registry is not even read.
    #[tokio::test]
    async fn a_claim_or_another_kind_on_the_marker_is_left() {
        let mut claimed = review("agent-claude-b", Some("2026-11-15T17:59:00Z"));
        claimed["status"] = json!("active");
        let mine = review("agent-claude-b", Some("2026-11-15T17:59:00Z"));
        let mut other_kind = car(None, mine.clone());
        other_kind["kind"] = json!("backlog-item");
        let mut closed = car(None, mine.clone());
        closed["status"] = json!("closed");
        for (full, step) in [
            (car(None, claimed.clone()), claimed.clone()),
            (other_kind, mine.clone()),
            (closed, mine.clone()),
        ] {
            let stub = serve_one(full.clone(), builder_run_row("agent-claude-b")).await;
            let h = JobsReturnStrandedReview::with_client(api_client(), stub.base.clone());
            h.invoke(&event_args(), &assigned(&step)).await.unwrap();
            assert!(stub.writes().is_empty(), "{full}");
            assert_eq!(stub.read_count("/api/agents"), 0, "{full}");
        }
    }

    /// Each trigger refuses the other's declaration by name: a marker
    /// rule that declares a bound would run the half the marker must
    /// never run, and a sweep without one is half a rule.
    #[tokio::test]
    async fn each_trigger_refuses_the_others_declaration() {
        let step = review("agent-claude-b", Some("2026-11-15T17:59:00Z"));
        let stub = serve_one(car(None, step.clone()), builder_run_row("agent-claude-b")).await;
        let h = JobsReturnStrandedReview::with_client(api_client(), stub.base.clone());
        let err = h.invoke(&args(), &assigned(&step)).await.unwrap_err();
        assert!(
            matches!(&err, HandlerError::Permanent(m) if m.contains("unclaimed_minutes")),
            "{err:?}"
        );
        let err = h.invoke(&event_args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(&err, HandlerError::Permanent(m) if m.contains("unclaimed_minutes")),
            "{err:?}"
        );
        assert!(stub.writes().is_empty());
    }

    /// The shipped rules: the fifteen-minute sweep declares the reviewer
    /// profile and the step's own two-hour expectation; its twin on the
    /// assignment marker declares the same profile and no bound, and its
    /// `when` names that profile so a non-review assignment never reads
    /// a packet.
    #[test]
    fn the_shipped_rules_declare_the_sweep_and_its_marker_twin() {
        let raw =
            boss_dispatcher::rules::registry::parse_raw_path(boss_testing::dispatcher_rules_dir())
                .expect("parse the shipped rule registry");
        let mine: Vec<_> = raw
            .rules
            .iter()
            .filter(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
            .collect();
        let parsed = |rule: &boss_dispatcher::rules::registry::RawRule| -> Vec<(String, ArgValue)> {
            rule.do_steps
                .iter()
                .flat_map(|d| d.args.iter())
                .map(|(k, v)| {
                    let v = v.trim();
                    let value = match v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                        Some(s) => ArgValue::String(s.to_string()),
                        None => ArgValue::Int(v.parse().unwrap_or(-1)),
                    };
                    (k.clone(), value)
                })
                .collect()
        };
        let sweep = mine
            .iter()
            .find(|r| r.name == RULE)
            .expect("the sweep is shipped");
        assert!(sweep.on_event.is_none() && sweep.schedule.is_some());
        assert_eq!(declaration(&parsed(sweep)), Ok(decl()), "{}", sweep.name);
        let event = mine
            .iter()
            .find(|r| r.name == EVENT_RULE)
            .expect("the marker twin is shipped");
        assert_eq!(event.on_event.as_deref(), Some("step.assigned.*"));
        let event_decl = declaration(&parsed(event)).expect("the twin's args parse");
        assert_eq!(
            event_decl,
            Declaration {
                unclaimed_minutes: None,
                ..decl()
            }
        );
        let when = event
            .when
            .as_deref()
            .expect("the twin filters on its profile");
        assert!(
            when.contains(&format!(
                "metadata.agent_profile = \"{}\"",
                event_decl.profile
            )),
            "{when}"
        );
        assert_eq!(mine.len(), 2, "the sweep and its twin, and no third");
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsReturnStrandedReview::new("http://unused");
        assert_eq!(h.name(), HANDLER);
        assert!(
            crate::cascade::handler_emits().contains_key(HANDLER),
            "the cascade table knows what this handler emits"
        );
    }
}
