//! `jobs.release_executor_refused` — a nomination or claim on a step
//! whose PINNED executor declaration refuses its holder is released to
//! the step's role audience, with an `executor_refused` note naming the
//! holder, its kind and class, and what the step demanded (backlog
//! 231cb747, conductor order 20261105_1714Z).
//!
//! ## The case
//!
//! On 2026-11-05 agent-codex held 37 of the ready/active `build` steps
//! among the 300 newest backlog items, and Codex does not build from
//! here. Design ccfe87ff's first pass makes `build` declare
//! `executor = "model:opus"`, and the claim door then refuses a robot
//! whose agents row records another class (T2c 4839d6a1). That stops
//! the NEXT claim. It does nothing for a hold already standing:
//!
//! - a NOMINATION is a step PUT naming an assignee, which the claim door
//!   never judges — a hand's `boss step nominate`, or any automatic lane
//!   that matched on something other than the class;
//! - a CLAIM was judged against the facts at the instant it was made,
//!   and a packet moved onto a declaring version afterwards (`boss job
//!   convert`, design 7cf202a9) or a holder whose row changed class
//!   since keeps a hold the door would refuse today.
//!
//! Either stands until a hand releases it, one by one. This arm is the
//! factory doing that instead.
//!
//! ## One predicate, the claim door's
//!
//! The demand is read exactly as the claim door reads it
//! (`crates/core/boss-jobs/src/http/steps.rs`, "THE STEP NAMES ITS
//! EXECUTOR'S KIND"): the packet's kind and the version it was ADMITTED
//! under, that version's row, and [`demanded_by`] on the step's
//! `spec_slug`. Never the active row — a packet pinned to a version
//! that declares no executor demands nothing, so residue on packets
//! still pinned to backlog-item v17 is left exactly where it stands.
//! A version the registry answers it does not hold declares nothing,
//! as at the claim. The verdict is [`judge_holder`], the one
//! eligibility predicate the claim door and the matchmaker ask too
//! (backlog c17dec0f), over the holder's CANONICAL id — the agents row
//! by id or alias (43ca3b3a). Nothing is matched on a step kind: every
//! kind of step on every protocol is judged the one way, off its pinned
//! declaration.
//!
//! ## What it judges, and what it leaves alone
//!
//! It releases a holder for WHO IT IS: the wrong executor kind; a
//! person the people roster does not list among the demanded role's
//! active holders; a robot whose agents row records no model class, or
//! another one; an actor that is not the automaton operating the named
//! equipment station. It never releases for what the registry or the
//! floor says TODAY — a `robots:<class>` pool retired, a station
//! retired, a full bay — because those are capacity and registry
//! facts, and releasing work on them is a routing decision about the
//! factory, not a verdict on the holder. So a class the row records is
//! taken as known, a station is judged only on its automaton (bays are
//! not counted), and a station this arm cannot read is left alone and
//! said so.
//!
//! WHO HOLDS A ROLE is read through the claim door's own roster port
//! ([`RosterLookup`], the people roster), so a person is judged here as
//! the door judges them (c17dec0f: this arm used to judge a person on
//! a `role:` step on kind alone, and kept every hold the door refuses).
//! A role the roster lists NO holder of is left alone and said so, as
//! the empty agents registry below is refused: a dark or narrowed
//! people read answers empty, and judged, it would release every
//! person on every step demanding that role.
//!
//! An EMPTY agents registry is refused, never read as "no agents": a
//! dark or narrowed read answers `total: 0`, and judged, it would make
//! every robot spelled by its login a person and release every model
//! step on the board.
//!
//! ## Two triggers, one handler
//!
//! `step.assigned.<kind>` fires on both a nomination and a claim, with
//! the step and packet ids, so a hold the declaration refuses is
//! released the moment it is made. An hourly sweep of the open board
//! finds the residue no event announces — a hold made before this arm
//! existed, or carried onto a declaring version by a convert, which
//! re-pins a packet without assigning anything. A payload carrying a
//! `job_id` and a `step_id` is the event; anything else is the tick.
//!
//! ## The verdict is the fresh read's
//!
//! The sweep judges the slim listing, and that judgement is only a
//! filter: a step it refuses is read again whole, and judged again on
//! THAT read — its kind, the version it is pinned to now, its slug and
//! its holder — before anything is written (backlog efc60d6d). The
//! fresh read used to be checked for the holder alone, so a packet
//! `boss job convert` moved between the two reads was released under
//! the version it had left, the note naming that version.
//!
//! ## The release is the reclaim's two writes
//!
//! [`release_patch`] then [`release_put`], the same pair the dead-run
//! reclaim and the urgent watch use: the metadata merge first, while
//! the step is still held, clearing the `agent_run` edge with an
//! explicit null (ce3a4b16) and landing the note under [`NOTE_KEY`] —
//! not the reclaim's `reclaimed` and not the hand verb's `released`, so
//! a reader can tell which door freed it — then the PUT that returns the
//! step to `ready` and nobody's, its `authority_role` untouched, which
//! is its role audience. A `nomination_requested` naming the refused
//! holder is retracted in the same merge and copied into the note: an
//! intent the declaration refuses can never be honoured, and left
//! standing it makes the jobs API refuse every automatic assignment to
//! anyone else (`automatic_assignment_superseded`), so the step would
//! sit in its queue unnominated.
//!
//! ## Once per holder per step
//!
//! A step whose note already names its current holder is left alone and
//! warned about. The release emits a `jobs.step.updated`, the core
//! matchmaker reacts to a ready unassigned step, and a lane that does
//! not read the declaration could nominate the same holder straight
//! back; releasing again would be a loop at event speed. The second
//! nomination is a matchmaking defect worth reading, so it is named in
//! the journal rather than fought.

use super::common::{
    api_client, dispatcher_actor_header, empty_roster_refusal, get_json, jobs_where_slim,
    row_or_refuse, rows_or_refuse, sim_origin_value, write_json,
};
use super::jobs_reclaim_abandoned_step::{release_patch, release_put};
use async_trait::async_trait;
use boss_core::actor::{ActorId, ExecutorKind};
use boss_dispatcher::rules::expr::Value as ArgValue;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::agent_runs::EDGE_KEY;
use boss_jobs::owner_resolution::{ReqwestRosterLookup, RosterLookup};
use boss_jobs::registry::WorkflowSpec;
use boss_jobs::step_executor::{
    Bays, Capability, ExecutorDemand, Half, Ineligible, demanded_by, judge_holder,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The handler's registered name.
pub const HANDLER: &str = "jobs.release_executor_refused";

/// Where the release's evidence lands on the freed step.
pub const NOTE_KEY: &str = "executor_refused";

/// The intent `boss step nominate` records beside the assignee it PUTs
/// (`crates/orchestrators/boss-cli/src/steps.rs`, `nominate_step`).
const NOMINATION_KEY: &str = "nomination_requested";

/// The statuses a step is HELD in: nominated (`ready`, assigned) or
/// claimed (`active`).
const HELD: [&str; 2] = ["ready", "active"];

pub struct JobsReleaseExecutorRefused {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    /// Who holds a role: the claim door's own port and adapter
    /// (`JobsApiState::roster`), so a person is judged on the roster the
    /// door judges them on.
    roster: Arc<dyn RosterLookup>,
}

impl JobsReleaseExecutorRefused {
    pub fn new(jobs_base: impl Into<String>, people_base: impl Into<String>) -> Arc<Self> {
        Self::with_client(
            api_client(),
            jobs_base,
            Arc::new(ReqwestRosterLookup::new(people_base)),
        )
    }

    /// Construct with a custom client and roster (tests point them at
    /// stand-ins for jobs-api and the people roster).
    pub fn with_client(
        client: boss_core::machine_token::Client,
        jobs_base: impl Into<String>,
        roster: Arc<dyn RosterLookup>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
            roster,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

/// A holder the pinned declaration refuses, as the note records it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Refusal {
    /// The id the step records as its holder.
    pub holder: String,
    /// Its canonical id — the agents row's, when one speaks for it.
    pub actor: String,
    /// The executor kind that id is.
    pub kind: ExecutorKind,
    /// The model class its agents row records, if any.
    pub model_class: Option<String>,
    /// The declaration, as the step spells it (`model:opus`).
    pub executor: String,
    /// Which half of the claim door's predicate failed.
    pub half: Half,
    pub demanded: String,
    pub found: String,
}

/// What the arm makes of one held step whose pinned spec names a kind.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Verdict {
    /// The declaration admits the holder.
    Admitted,
    /// Not judgeable from what this arm reads — left alone, with why.
    Unjudged(String),
    /// The declaration refuses the holder for who it is.
    Refused(Refusal),
}

/// The agents row speaking for `holder`: by id, or by an alias — the
/// row the claim door reads (43ca3b3a).
fn agent_row<'a>(agents: &'a [Value], holder: &str) -> Option<&'a Value> {
    agents.iter().find(|a| {
        a.get("id").and_then(Value::as_str) == Some(holder)
            || a.get("aliases")
                .and_then(Value::as_array)
                .is_some_and(|aliases| aliases.iter().any(|x| x.as_str() == Some(holder)))
    })
}

/// The ACTIVE equipment station row named `name`, out of `GET
/// /api/stations`.
fn equipment_station<'a>(stations: &'a [Value], name: &str) -> Option<&'a Value> {
    stations.iter().find(|s| {
        s.get("name").and_then(Value::as_str) == Some(name)
            && s.get("status").and_then(Value::as_str) == Some("active")
            && s.get("kind").and_then(Value::as_str) == Some("equipment")
    })
}

/// Judge `holder` against `demand` through [`judge_holder`], the claim
/// door's predicate, over the evidence this arm reads. `stations` is
/// read by the caller only for an `equipment:` demand; `roster` is
/// asked only for a person on a `role:` step — a holder of the wrong
/// kind is refused before any capability is read.
pub(crate) async fn judge(
    demand: &ExecutorDemand,
    holder: &str,
    agents: &[Value],
    stations: &[Value],
    roster: &dyn RosterLookup,
) -> Result<Verdict, HandlerError> {
    let row = agent_row(agents, holder);
    let actor = row
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .unwrap_or(holder)
        .to_string();
    let kind = match actor.parse::<ActorId>() {
        Ok(id) => id.executor_kind(),
        Err(never) => match never {},
    };
    let model_class = row
        .and_then(|r| r.get("model_class"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    let refused = |i: Ineligible| {
        Verdict::Refused(Refusal {
            holder: holder.to_string(),
            actor: actor.clone(),
            kind,
            model_class: model_class.clone(),
            executor: demand.declared(),
            half: i.half,
            demanded: i.demanded,
            found: i.found,
        })
    };
    let admitted = match judge_holder(demand, &actor) {
        Ok(admitted) => admitted,
        Err(i) => return Ok(refused(i)),
    };
    let found = match demand {
        // A class the row records is taken as KNOWN: whether its pool is
        // active today is the registry's fact, not the holder's.
        ExecutorDemand::Model(_) => Capability::Model {
            class: model_class.clone(),
            known: model_class.is_some(),
        },
        // Judged on the automaton alone — one bay, none occupied, so
        // capacity can never be the reason a standing hold is released.
        ExecutorDemand::Equipment(name) => {
            match equipment_station(stations, name)
                .and_then(|s| s.get("automaton"))
                .and_then(Value::as_str)
            {
                Some(automaton) => Capability::Equipment(Some(Bays {
                    automaton: automaton.to_string(),
                    bays: 1,
                    occupied: 0,
                })),
                None => {
                    return Ok(Verdict::Unjudged(format!(
                        "no active equipment station `{name}` was read, so who operates it \
                         cannot be compared — a retired station is the registry's change, not \
                         the holder's"
                    )));
                }
            }
        }
        // The claim door's roster read, through its own port: a person
        // is held to the role exactly as the door holds them.
        ExecutorDemand::Role(role) => {
            let holders = roster.active_holders(role).await.map_err(|e| {
                HandlerError::Downstream(format!("the roster could not list {role}'s holders: {e}"))
            })?;
            if holders.is_empty() {
                return Ok(Verdict::Unjudged(format!(
                    "the roster lists no holder of {role} at all — a dark or narrowed people \
                     read answers empty, and judged, it would release every person on every \
                     role:{role} step"
                )));
            }
            Capability::Role {
                holds: holders.contains(&actor),
            }
        }
    };
    Ok(match admitted.judge(&found) {
        Ok(()) => Verdict::Admitted,
        Err(i) => refused(i),
    })
}

/// Has this arm already released `holder` from this step? Then a second
/// release would be the first half of a loop (module doc, "Once per
/// holder per step").
pub(crate) fn already_released(metadata: &Value, holder: &str) -> bool {
    metadata
        .get(NOTE_KEY)
        .and_then(|n| n.get("holder"))
        .and_then(Value::as_str)
        == Some(holder)
}

/// The metadata merge that releases `step`: the run edge cleared, a
/// nomination naming the refused holder retracted, and the note — one
/// server-side write, before the status PUT.
pub(crate) fn release_body(
    step: &Value,
    refusal: &Refusal,
    pinned: &str,
    at: &str,
    rule: &str,
) -> Value {
    let metadata = step.get("metadata").cloned().unwrap_or(Value::Null);
    let nomination = metadata
        .get(NOMINATION_KEY)
        .filter(|n| {
            n.get("assignee_id")
                .and_then(Value::as_str)
                .is_some_and(|who| who == refusal.holder || who == refusal.actor)
        })
        .cloned();
    let note = json!({
        "holder": refusal.holder,
        "actor": refusal.actor,
        "executor_kind": refusal.kind,
        "model_class": refusal.model_class,
        "executor": refusal.executor,
        "half": refusal.half,
        "demanded": refusal.demanded,
        "found": refusal.found,
        "status_was": step.get("status"),
        "from_run": metadata.get(EDGE_KEY),
        "nomination": nomination,
        "pinned": pinned,
        "at": at,
        "rule": rule,
    });
    let mut body = release_patch(EDGE_KEY, NOTE_KEY, note);
    if nomination.is_some() {
        body[NOMINATION_KEY] = Value::Null;
    }
    body
}

/// The reads one firing shares across every step it judges.
#[derive(Default)]
struct Reads {
    specs: BTreeMap<(String, i64), Option<WorkflowSpec>>,
    agents: Option<Vec<Value>>,
    stations: Option<Vec<Value>>,
}

/// The step's holder, trimmed, when the step is HELD.
fn held_by(step: &Value) -> Option<&str> {
    let status = step.get("status").and_then(Value::as_str)?;
    if !HELD.contains(&status) {
        return None;
    }
    step.get("assignee_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

impl JobsReleaseExecutorRefused {
    /// One packet, whole.
    async fn packet(&self, job_id: &str, rule: &str) -> Result<Value, HandlerError> {
        let body = get_json(
            &self.client,
            &format!("{}/api/jobs/{job_id}", self.base()),
            rule,
        )
        .await?;
        row_or_refuse(body, &format!("GET /api/jobs/{job_id}")).map_err(HandlerError::Downstream)
    }

    /// The version a packet was admitted under, as the claim door reads
    /// it: `None` when the registry answers it does not hold that
    /// version (it then declares nothing); any other unread answer is an
    /// error — no evidence is not a pass.
    async fn pinned(
        &self,
        kind: &str,
        version: i64,
        rule: &str,
    ) -> Result<Option<WorkflowSpec>, HandlerError> {
        let url = format!("{}/api/workflows/{kind}/versions/{version}", self.base());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_actor_header(rule))
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
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}: {text}"
            )));
        }
        resp.json::<WorkflowSpec>()
            .await
            .map(Some)
            .map_err(|e| HandlerError::Downstream(format!("decode {url}: {e}")))
    }

    async fn listing(&self, path: &str, rule: &str) -> Result<Vec<Value>, HandlerError> {
        let body = get_json(&self.client, &format!("{}{path}", self.base()), rule).await?;
        rows_or_refuse(&body, &format!("GET {path}")).map_err(HandlerError::Downstream)
    }

    /// Judge one held step of `job` (whole or slim) and release it when
    /// its pinned declaration refuses its holder. `Ok(true)` when it
    /// released.
    async fn consider(
        &self,
        job: &Value,
        step: &Value,
        reads: &mut Reads,
        at: &str,
        rule: &str,
    ) -> Result<bool, HandlerError> {
        let (Some(job_id), Some(step_id)) = (
            job.get("id").and_then(Value::as_str),
            step.get("id").and_then(Value::as_str),
        ) else {
            return Ok(false);
        };
        if self.refusal(job, step, reads, rule).await?.is_none() {
            return Ok(false);
        }
        // THE RECORD AS IT STANDS NOW, not as the event or the slim
        // listing carried it: the step's metadata (the edge, the
        // nomination, an earlier note) rides only the whole row — and
        // the verdict is taken again on this read, because the packet
        // may have been repinned, or the hold changed hands, since the
        // first was judged (efc60d6d). Only this one decides.
        let fresh = self.packet(job_id, rule).await?;
        let Some(now) = fresh
            .get("steps")
            .and_then(Value::as_array)
            .and_then(|steps| {
                steps
                    .iter()
                    .find(|s| s.get("id").and_then(Value::as_str) == Some(step_id))
            })
        else {
            return Ok(false);
        };
        let Some((refusal, pinned)) = self.refusal(&fresh, now, reads, rule).await? else {
            tracing::debug!(
                rule, packet = %job_id, step = %step_id,
                "left: the packet as it stands now admits its holder, or names none"
            );
            return Ok(false);
        };
        let holder = refusal.holder.as_str();
        let metadata = now.get("metadata").cloned().unwrap_or(Value::Null);
        if already_released(&metadata, holder) {
            tracing::warn!(
                rule, packet = %job_id, step = %step_id, holder,
                "{holder} was released from this step once already, under {}, and holds it \
                 again — a lane nominated it back without reading the declaration; left \
                 standing rather than looped",
                refusal.executor
            );
            return Ok(false);
        }
        let url = format!("{}/api/jobs/{job_id}/steps/{step_id}", self.base());
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{url}/metadata"),
            &release_body(now, &refusal, &pinned, at, rule),
            rule,
        )
        .await?;
        write_json(
            &self.client,
            reqwest::Method::PUT,
            &url,
            &release_put(),
            rule,
        )
        .await?;
        tracing::info!(
            rule, packet = %job_id, step = %step_id, holder,
            "released to its role audience: {} refuses {} ({}): {}",
            refusal.executor, refusal.actor, refusal.kind, refusal.found
        );
        Ok(true)
    }

    /// The refusal the step's PINNED declaration makes of its holder, on
    /// this read of `job`, with the `kind@version` it was judged under —
    /// or `None` when the step is not held, its packet's pin declares no
    /// kind for it, or the declaration admits (or cannot judge) the
    /// holder.
    async fn refusal(
        &self,
        job: &Value,
        step: &Value,
        reads: &mut Reads,
        rule: &str,
    ) -> Result<Option<(Refusal, String)>, HandlerError> {
        let Some(holder) = held_by(step) else {
            return Ok(None);
        };
        let job_id = job.get("id").and_then(Value::as_str).unwrap_or_default();
        let step_id = step.get("id").and_then(Value::as_str).unwrap_or_default();
        let (Some(kind), Some(version)) = (
            job.get("kind").and_then(Value::as_str),
            job.get("workflow_version").and_then(Value::as_i64),
        ) else {
            return Ok(None);
        };
        let key = (kind.to_string(), version);
        if !reads.specs.contains_key(&key) {
            let spec = self.pinned(kind, version, rule).await?;
            reads.specs.insert(key.clone(), spec);
        }
        let Some(spec) = reads.specs.get(&key).and_then(Option::as_ref) else {
            return Ok(None);
        };
        let slug = step.get("spec_slug").and_then(Value::as_str);
        let demand = match demanded_by(spec, slug) {
            Ok(Some(demand)) => demand,
            Ok(None) => return Ok(None),
            Err(why) => {
                tracing::warn!(
                    rule, packet = %job_id, step = %step_id,
                    "{kind}@{version} declares a malformed executor, which the claim door \
                     refuses everyone on; not this arm's to release: {why}"
                );
                return Ok(None);
            }
        };
        if reads.agents.is_none() {
            let agents = self.listing("/api/agents", rule).await?;
            if agents.is_empty() {
                return Err(empty_roster_refusal(
                    HANDLER,
                    "agents",
                    "every instance runs at least one registered agent, and an empty read is a \
                     dark or narrowed registry — judged, every robot spelled by its login would \
                     read as a person and every model step would be released",
                ));
            }
            reads.agents = Some(agents);
        }
        if matches!(demand, ExecutorDemand::Equipment(_)) && reads.stations.is_none() {
            reads.stations = Some(self.listing("/api/stations", rule).await?);
        }
        let verdict = judge(
            &demand,
            holder,
            reads.agents.as_deref().unwrap_or_default(),
            reads.stations.as_deref().unwrap_or_default(),
            self.roster.as_ref(),
        )
        .await?;
        match verdict {
            Verdict::Admitted => Ok(None),
            Verdict::Unjudged(why) => {
                tracing::debug!(rule, packet = %job_id, step = %step_id, holder, "left: {why}");
                Ok(None)
            }
            Verdict::Refused(refusal) => Ok(Some((refusal, format!("{kind}@{version}")))),
        }
    }
}

#[async_trait]
impl Handler for JobsReleaseExecutorRefused {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        _args: &[(String, ArgValue)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let at = ctx.firing_instant()?.to_rfc3339();
        let mut reads = Reads::default();
        let payload = &ctx.event_payload;
        // THE EVENT: one step, named by the assignment marker.
        if let (Some(job_id), Some(step_id)) = (
            payload.get("job_id").and_then(Value::as_str),
            payload.get("step_id").and_then(Value::as_str),
        ) {
            let job = self.packet(job_id, rule).await?;
            if let Some(step) = job
                .get("steps")
                .and_then(Value::as_array)
                .and_then(|steps| {
                    steps
                        .iter()
                        .find(|s| s.get("id").and_then(Value::as_str) == Some(step_id))
                })
                .cloned()
            {
                self.consider(&job, &step, &mut reads, &at, rule).await?;
            }
            return Ok(());
        }
        // THE SWEEP: every held step on the open board, read slim — the
        // pinned row decides which need their metadata, and only a
        // refused one is read whole.
        let board = jobs_where_slim(&self.client, self.base(), "status=open", rule).await?;
        let mut released = 0usize;
        for job in &board {
            for step in job
                .get("steps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if self.consider(job, step, &mut reads, &at, rule).await? {
                    released += 1;
                }
            }
        }
        tracing::info!(
            rule,
            packets = board.len(),
            released,
            "swept the open board for holds an executor declaration refuses"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use axum::{Json, Router, extract::Path, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const KIND: &str = "fixture";
    const RULE: &str = "a-hold-its-step-executor-refuses-is-released";
    const SWEEP: &str = "a-standing-hold-its-step-executor-refuses-is-released-hourly";
    const CODEX: &str = "agent-codex";
    const CLAUDE_B: &str = "agent-claude-b";
    const CLAUDE_B_LOGIN: &str = "claude-b@algedonic.dev";
    const RUNNER: &str = "automation:gate-runner";
    /// The claim door's own two people
    /// (`a_claim_clears_only_for_the_executor_a_step_names.rs`): the
    /// roster lists the first among `auditor`'s holders, not the second.
    const AUDITOR: &str = "emp-auditor";
    const CLERK: &str = "emp-clerk";

    fn spec_step(title: &str, kind: &str, executor: Option<&str>) -> boss_jobs::registry::StepSpec {
        boss_jobs::registry::StepSpec {
            title: title.into(),
            kind: kind.into(),
            ready_when: "true".into(),
            executor: executor.map(str::to_string),
            ..Default::default()
        }
    }

    /// The fixture protocol at two versions: v1 declares NO executor on
    /// any step — the shape backlog-item v17 has — and v2 declares a
    /// `model:` demand on a `task` step and an `equipment:` demand on a
    /// `sign-off` step, beside a step naming none and one naming a
    /// credential principal. Two step kinds, two demand kinds: nothing
    /// in the handler reads the step kind.
    fn workflows() -> HashMap<(String, i64), Value> {
        let v1 = boss_jobs::registry::WorkflowSpec::platform_seed(
            KIND,
            "Fixture",
            "test",
            vec!["custom".into()],
            vec![
                spec_step("build", "task", None),
                spec_step("gate", "sign-off", None),
            ],
        );
        let mut v2 = boss_jobs::registry::WorkflowSpec::platform_seed(
            KIND,
            "Fixture",
            "test",
            vec!["custom".into()],
            vec![
                spec_step("build", "task", Some("model:opus")),
                spec_step("gate", "sign-off", Some("equipment:gates")),
                spec_step("free", "task", None),
                spec_step("ops", "task", Some("runner:ops")),
                spec_step("audit", "task", Some("role:auditor")),
            ],
        );
        v2.version = 2;
        HashMap::from([
            ((KIND.to_string(), 1), serde_json::to_value(v1).unwrap()),
            ((KIND.to_string(), 2), serde_json::to_value(v2).unwrap()),
        ])
    }

    /// The agents registry as `GET /api/agents` serves it: Codex
    /// recording another class, Claude B recording `opus` under the
    /// login it signs with.
    fn agents() -> Vec<Value> {
        vec![
            json!({"id": CODEX, "display_name": "Codex", "default_model": "gpt-5",
                   "model_class": "gpt", "role": "platform-admin", "aliases": []}),
            json!({"id": CLAUDE_B, "display_name": "Claude B", "default_model": "opus-5-5",
                   "model_class": "opus", "role": "platform-admin",
                   "aliases": [CLAUDE_B_LOGIN]}),
        ]
    }

    fn stations() -> Vec<Value> {
        vec![
            json!({"name": "gates", "version": 1, "status": "active", "kind": "equipment",
                    "automaton": RUNNER, "bays": 2}),
        ]
    }

    /// One held step, `slug`, on a packet pinned to `version`.
    fn held(id: &str, version: i64, slug: &str, status: &str, holder: &str) -> Value {
        json!({
            "id": id,
            "kind": KIND,
            "workflow_version": version,
            "status": "open",
            "metadata": {},
            "steps": [
                { "id": format!("{id}-triage"), "spec_slug": "triage", "kind": "task",
                  "status": "completed", "assignee_id": "agent-claude", "metadata": {} },
                { "id": format!("{id}-{slug}"), "spec_slug": slug, "kind": "task",
                  "status": status, "assignee_id": holder,
                  "metadata": { "authority_role": "platform-admin", "agent_model": "opus-5-5" } },
            ],
        })
    }

    fn with_step_metadata(mut job: Value, key: &str, value: Value) -> Value {
        job["steps"][1]["metadata"][key] = value;
        job
    }

    type Writes = Arc<Mutex<Vec<(String, String, Value)>>>;

    /// A jobs API answering the reads this handler makes and the two
    /// doors it writes through, the way the real one does: the PUT is an
    /// overlay that refuses a `metadata` body (e39a9d2a, the server's own
    /// rule called), and the merge door deletes a key sent as `null`.
    async fn mock(jobs: Vec<Value>, agents: Vec<Value>) -> (String, Writes) {
        mock_listed(jobs, None, agents).await
    }

    /// [`mock`], with the open board's slim listing answering `listed`
    /// rather than the packets as they stand — the listing a sweep
    /// judged, read before a move the whole packet's read then shows.
    async fn mock_listed(
        jobs: Vec<Value>,
        listed: Option<Vec<Value>>,
        agents: Vec<Value>,
    ) -> (String, Writes) {
        let writes: Writes = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<Mutex<HashMap<String, Value>>> = Arc::new(Mutex::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                .collect(),
        ));
        let specs = Arc::new(workflows());
        let (list, one, put_jobs, patch_jobs) =
            (by_id.clone(), by_id.clone(), by_id.clone(), by_id.clone());
        let (put_log, patch_log) = (writes.clone(), writes.clone());
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move || {
                    let by_id = list.clone();
                    let listed = listed.clone();
                    async move {
                        let rows: Vec<Value> = listed.unwrap_or_else(|| {
                            by_id
                                .lock()
                                .unwrap()
                                .values()
                                .filter(|j| j["status"] == "open")
                                .cloned()
                                .collect()
                        });
                        Json(json!({ "data": rows, "total": rows.len() }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = one.clone();
                    async move {
                        match by_id.lock().unwrap().get(&id).cloned() {
                            Some(j) => Json(j).into_response(),
                            None => {
                                (axum::http::StatusCode::NOT_FOUND, "no such job").into_response()
                            }
                        }
                    }
                }),
            )
            .route(
                "/api/workflows/{kind}/versions/{version}",
                get(move |Path((kind, version)): Path<(String, i64)>| {
                    let specs = specs.clone();
                    async move {
                        match specs.get(&(kind, version)).cloned() {
                            Some(s) => Json(s).into_response(),
                            None => (axum::http::StatusCode::NOT_FOUND, "no such version")
                                .into_response(),
                        }
                    }
                }),
            )
            .route(
                "/api/agents",
                get(move || {
                    let rows = agents.clone();
                    async move { Json(json!({ "data": rows, "total": rows.len() })) }
                }),
            )
            .route(
                "/api/stations",
                get(|| async {
                    let rows = stations();
                    Json(json!({ "data": rows, "total": rows.len() }))
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}",
                axum::routing::put(
                    move |Path((id, step_id)): Path<(String, String)>, Json(body): Json<Value>| {
                        let writes = put_log.clone();
                        let by_id = put_jobs.clone();
                        async move {
                            if boss_jobs::step_metadata_write::put_carries_metadata(&body) {
                                return (
                                    axum::http::StatusCode::CONFLICT,
                                    boss_jobs::step_metadata_write::METADATA_BODY_HINT,
                                )
                                    .into_response();
                            }
                            writes.lock().unwrap().push((
                                "PUT".into(),
                                step_id.clone(),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id) {
                                for step in job["steps"].as_array_mut().into_iter().flatten() {
                                    if step["id"] == json!(step_id) {
                                        for field in ["status", "assignee_id"] {
                                            if let Some(v) = body.get(field) {
                                                step[field] = v.clone();
                                            }
                                        }
                                    }
                                }
                            }
                            axum::http::StatusCode::NO_CONTENT.into_response()
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}/metadata",
                axum::routing::patch(
                    move |Path((id, step_id)): Path<(String, String)>, Json(body): Json<Value>| {
                        let writes = patch_log.clone();
                        let by_id = patch_jobs.clone();
                        async move {
                            writes.lock().unwrap().push((
                                "PATCH".into(),
                                step_id.clone(),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id) {
                                for step in job["steps"].as_array_mut().into_iter().flatten() {
                                    if step["id"] != json!(step_id) {
                                        continue;
                                    }
                                    let Some(md) = step["metadata"].as_object_mut() else {
                                        continue;
                                    };
                                    for (k, v) in body.as_object().into_iter().flatten() {
                                        if v.is_null() {
                                            md.remove(k);
                                        } else {
                                            md.insert(k.clone(), v.clone());
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
        (format!("http://{addr}"), writes)
    }

    /// The `step.assigned.<kind>` marker a nomination or a claim emits.
    fn assigned(job: &str, step: &str, holder: &str) -> InvocationContext {
        InvocationContext {
            rule_name: RULE.into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.assigned.task".into(),
            event_payload: json!({ "job_id": job, "step_id": step, "kind": "task",
                                   "assignee_id": holder, "metadata": {} }),
            event_timestamp: Some(
                chrono::DateTime::parse_from_rfc3339("2026-11-05T18:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
        }
    }

    /// The hourly tick.
    fn tick(at: &str) -> InvocationContext {
        InvocationContext {
            rule_name: SWEEP.into(),
            triggering_event_id: "tick-1".into(),
            triggering_topic: "schedule".into(),
            event_payload: json!({ "_day": &at[..10], "_at": at }),
            event_timestamp: None,
        }
    }

    async fn stored(base: &str, job: &str, slug: &str) -> Value {
        let job: Value = reqwest::get(format!("{base}/api/jobs/{job}"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        job["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["spec_slug"] == slug)
            .cloned()
            .unwrap()
    }

    /// The people roster, in memory, as the claim door's fixture has it:
    /// [`AUDITOR`] among `auditor`'s active holders, and nobody else
    /// holding anything a step here demands.
    struct Roster(Vec<(&'static str, &'static str)>);

    impl Roster {
        fn door() -> Arc<Self> {
            Arc::new(Self(vec![("auditor", AUDITOR)]))
        }
    }

    #[async_trait]
    impl RosterLookup for Roster {
        async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
            Ok(self
                .0
                .iter()
                .filter(|(r, _)| *r == role)
                .map(|(_, id)| id.to_string())
                .collect())
        }
        async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
            Ok(self.0.iter().any(|(_, who)| *who == id))
        }
    }

    fn handler(base: &str) -> Arc<JobsReleaseExecutorRefused> {
        JobsReleaseExecutorRefused::with_client(
            crate::handlers::common::api_client(),
            base,
            Roster::door(),
        )
    }

    /// THE OBLIGATION. A nomination of Codex onto a step whose PINNED
    /// spec demands `model:opus` is released on the assignment marker:
    /// `ready`, nobody's, its role audience kept, the refused intent
    /// retracted and copied into the note, and the note naming the
    /// holder, its kind and class, and the demanded executor.
    #[tokio::test]
    async fn a_hold_the_pinned_executor_refuses_is_released_with_the_note() {
        let nominated = with_step_metadata(
            held("p1", 2, "build", "ready", CODEX),
            NOMINATION_KEY,
            json!({"assignee_id": CODEX, "at": "2026-11-05T17:00:00Z", "by": "agent-claude",
                   "why": "queue"}),
        );
        let (base, writes) = mock(vec![nominated], agents()).await;
        handler(&base)
            .invoke(&[], &assigned("p1", "p1-build", CODEX))
            .await
            .expect("the event is judged");

        let step = stored(&base, "p1", "build").await;
        assert_eq!(step["status"], "ready", "{step}");
        assert_eq!(step["assignee_id"], Value::Null, "{step}");
        assert_eq!(
            step["metadata"]["authority_role"], "platform-admin",
            "the role audience it returns to is kept"
        );
        assert!(
            step["metadata"].get(NOMINATION_KEY).is_none(),
            "an intent the declaration refuses is retracted: {step}"
        );
        let note = &step["metadata"][NOTE_KEY];
        assert_eq!(note["holder"], CODEX);
        assert_eq!(note["actor"], CODEX);
        assert_eq!(note["executor_kind"], "robot");
        assert_eq!(note["model_class"], "gpt");
        assert_eq!(note["executor"], "model:opus");
        assert_eq!(note["half"], "capability");
        assert!(
            note["found"]
                .as_str()
                .unwrap()
                .contains("runs model class gpt"),
            "{note}"
        );
        assert_eq!(note["pinned"], "fixture@2");
        assert_eq!(note["status_was"], "ready");
        assert_eq!(note["nomination"]["assignee_id"], CODEX);
        assert_eq!(note["rule"], RULE);
        assert_eq!(note["at"], "2026-11-05T18:00:00+00:00");
        let methods: Vec<String> = writes.lock().unwrap().iter().map(|w| w.0.clone()).collect();
        assert_eq!(
            methods,
            ["PATCH", "PUT"],
            "the merge first, then the status"
        );
    }

    /// An admitted holder is untouched — by its registered id, and by
    /// the login the agents row lists as its alias, which the claim door
    /// resolves the same way (43ca3b3a).
    #[tokio::test]
    async fn an_admitted_holder_is_untouched() {
        let (base, writes) = mock(
            vec![
                held("p1", 2, "build", "active", CLAUDE_B),
                held("p2", 2, "build", "ready", CLAUDE_B_LOGIN),
            ],
            agents(),
        )
        .await;
        let h = handler(&base);
        h.invoke(&[], &assigned("p1", "p1-build", CLAUDE_B))
            .await
            .unwrap();
        h.invoke(&[], &assigned("p2", "p2-build", CLAUDE_B_LOGIN))
            .await
            .unwrap();
        h.invoke(&[], &tick("2026-11-05T19:00:00Z")).await.unwrap();
        assert!(
            writes.lock().unwrap().is_empty(),
            "{:?}",
            writes.lock().unwrap()
        );
    }

    /// A step whose pinned spec declares no executor kind is untouched:
    /// no `executor`, a credential principal, and — the load-bearing
    /// one — a packet PINNED to v1, which declares nothing, although the
    /// v2 row beside it demands `model:opus` of the same slug. Residue
    /// on a packet still pinned to backlog-item v17 stays where it is.
    #[tokio::test]
    async fn a_step_its_pinned_spec_declares_no_executor_for_is_untouched() {
        let (base, writes) = mock(
            vec![
                held("p1", 1, "build", "active", CODEX),
                held("p2", 2, "free", "ready", CODEX),
                held("p3", 2, "ops", "active", CODEX),
                held("p4", 7, "build", "ready", CODEX),
            ],
            agents(),
        )
        .await;
        let h = handler(&base);
        h.invoke(&[], &assigned("p1", "p1-build", CODEX))
            .await
            .unwrap();
        h.invoke(&[], &tick("2026-11-05T19:00:00Z")).await.unwrap();
        assert!(
            writes.lock().unwrap().is_empty(),
            "pinned v1, no executor, a principal, and a version the registry does not hold \
             (p4) all demand nothing: {:?}",
            writes.lock().unwrap()
        );
    }

    /// GENERIC OVER DEMANDS AND STEP KINDS. The `sign-off` step demands
    /// `equipment:gates`: a robot is refused on kind, an automaton that
    /// is not the station's on capability, and the station's own
    /// automaton is admitted — one tick, beside the `task` step's model
    /// demand, through the one predicate.
    #[tokio::test]
    async fn an_equipment_demand_releases_another_executor_and_keeps_its_automaton() {
        let (base, writes) = mock(
            vec![
                held("p1", 2, "gate", "active", CODEX),
                held("p2", 2, "gate", "active", "automation:other"),
                held("p3", 2, "gate", "active", RUNNER),
                held("p4", 2, "build", "active", CODEX),
            ],
            agents(),
        )
        .await;
        handler(&base)
            .invoke(&[], &tick("2026-11-05T19:00:00Z"))
            .await
            .unwrap();
        let robot = stored(&base, "p1", "gate").await;
        assert_eq!(robot["status"], "ready", "{robot}");
        assert_eq!(robot["metadata"][NOTE_KEY]["half"], "kind");
        assert_eq!(robot["metadata"][NOTE_KEY]["executor"], "equipment:gates");
        assert_eq!(robot["metadata"][NOTE_KEY]["demanded"], "automaton");
        assert_eq!(robot["metadata"][NOTE_KEY]["found"], "robot");
        let other = stored(&base, "p2", "gate").await;
        assert_eq!(other["status"], "ready", "{other}");
        assert_eq!(other["metadata"][NOTE_KEY]["half"], "capability");
        assert!(
            other["metadata"][NOTE_KEY]["found"]
                .as_str()
                .unwrap()
                .contains("operated by automation:gate-runner"),
            "{other}"
        );
        let own = stored(&base, "p3", "gate").await;
        assert_eq!(own["status"], "active");
        assert_eq!(own["assignee_id"], RUNNER);
        assert!(own["metadata"].get(NOTE_KEY).is_none());
        let model = stored(&base, "p4", "build").await;
        assert_eq!(model["metadata"][NOTE_KEY]["executor"], "model:opus");
        let released: Vec<String> = writes
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w.0 == "PUT")
            .map(|w| w.1.clone())
            .collect();
        let mut released = released;
        released.sort();
        assert_eq!(released, ["p1-gate", "p2-gate", "p4-build"]);
    }

    /// THE SWEEP FINDS STANDING RESIDUE. A claim made before this arm,
    /// or carried onto a declaring version by a convert, announces
    /// nothing; the hourly tick finds it on the open board, clears the
    /// run edge it carried, leaves the admitted and the undeclared, and
    /// a second tick writes nothing more.
    #[tokio::test]
    async fn the_sweep_releases_standing_residue_once() {
        let claimed = with_step_metadata(
            held("p1", 2, "build", "active", CODEX),
            EDGE_KEY,
            json!("aaaaaaaa-1111-4111-8111-111111111111"),
        );
        let (base, writes) = mock(
            vec![
                claimed,
                held("p2", 2, "build", "active", CLAUDE_B),
                held("p3", 1, "build", "active", CODEX),
            ],
            agents(),
        )
        .await;
        let h = handler(&base);
        h.invoke(&[], &tick("2026-11-05T19:00:00Z")).await.unwrap();
        let step = stored(&base, "p1", "build").await;
        assert_eq!(step["status"], "ready", "{step}");
        assert_eq!(step["assignee_id"], Value::Null);
        assert!(step["metadata"].get(EDGE_KEY).is_none(), "{step}");
        assert_eq!(
            step["metadata"][NOTE_KEY]["from_run"],
            "aaaaaaaa-1111-4111-8111-111111111111"
        );
        assert_eq!(step["metadata"][NOTE_KEY]["status_was"], "active");
        assert_eq!(step["metadata"][NOTE_KEY]["rule"], SWEEP);
        assert_eq!(
            step["metadata"][NOTE_KEY]["at"],
            "2026-11-05T19:00:00+00:00"
        );
        assert_eq!(stored(&base, "p2", "build").await["status"], "active");
        assert_eq!(stored(&base, "p3", "build").await["status"], "active");
        assert_eq!(
            writes.lock().unwrap().len(),
            2,
            "{:?}",
            writes.lock().unwrap()
        );
        h.invoke(&[], &tick("2026-11-05T20:00:00Z")).await.unwrap();
        assert_eq!(
            writes.lock().unwrap().len(),
            2,
            "a released step is held by nobody — nothing is written twice"
        );
    }

    /// ONCE PER HOLDER PER STEP. A holder this arm already released and
    /// a lane nominated straight back is left standing and named, not
    /// released again — a second release would be a loop.
    #[tokio::test]
    async fn a_holder_released_once_and_nominated_back_is_not_looped() {
        let back = with_step_metadata(
            held("p1", 2, "build", "ready", CODEX),
            NOTE_KEY,
            json!({"holder": CODEX, "executor": "model:opus"}),
        );
        let (base, writes) = mock(vec![back], agents()).await;
        handler(&base)
            .invoke(&[], &assigned("p1", "p1-build", CODEX))
            .await
            .unwrap();
        assert!(writes.lock().unwrap().is_empty());
    }

    /// An empty agents registry is a dark or narrowed read, and judged
    /// it would make every robot spelled by its login a person: refused,
    /// with nothing written.
    #[tokio::test]
    async fn an_empty_agents_registry_is_refused_not_judged() {
        let (base, writes) = mock(
            vec![held("p1", 2, "build", "active", CLAUDE_B_LOGIN)],
            vec![],
        )
        .await;
        let err = handler(&base)
            .invoke(&[], &tick("2026-11-05T19:00:00Z"))
            .await
            .expect_err("an empty roster is no answer");
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("agents")),
            "{err}"
        );
        assert!(writes.lock().unwrap().is_empty());
    }

    /// A PERSON ON A ROLE STEP IS JUDGED BY THE ROSTER, AS AT THE CLAIM
    /// (backlog c17dec0f). This arm left a person on a `role:` step
    /// standing on kind alone — "judged by the people roster at the
    /// claim; this arm reads no roster" — so a person the claim door
    /// would refuse kept the hold the door exists to refuse. Now the
    /// arm reads the door's own roster port: the person the roster does
    /// not list among `auditor`'s holders is released with the door's
    /// capability refusal, the one it lists is untouched, and a robot is
    /// still refused on kind — the claim door's three verdicts on the
    /// same three holders, through [`judge_holder`].
    #[tokio::test]
    async fn a_role_demand_releases_a_person_lacking_the_role_and_keeps_its_holder() {
        let (base, writes) = mock(
            vec![
                held("p1", 2, "audit", "active", CLERK),
                held("p2", 2, "audit", "active", AUDITOR),
                held("p3", 2, "audit", "ready", CODEX),
            ],
            agents(),
        )
        .await;
        let h = handler(&base);
        h.invoke(&[], &assigned("p1", "p1-audit", CLERK))
            .await
            .expect("the event is judged");
        h.invoke(&[], &tick("2026-11-05T19:00:00Z")).await.unwrap();

        let clerk = stored(&base, "p1", "audit").await;
        assert_eq!(clerk["status"], "ready", "{clerk}");
        assert_eq!(clerk["assignee_id"], Value::Null, "{clerk}");
        let note = &clerk["metadata"][NOTE_KEY];
        assert_eq!(note["executor"], "role:auditor");
        assert_eq!(note["executor_kind"], "person");
        assert_eq!(note["half"], "capability");
        assert_eq!(note["demanded"], "role auditor");
        assert!(
            note["found"]
                .as_str()
                .unwrap()
                .contains("emp-clerk does not hold auditor"),
            "{note}"
        );

        let auditor = stored(&base, "p2", "audit").await;
        assert_eq!(auditor["status"], "active");
        assert_eq!(auditor["assignee_id"], AUDITOR);
        assert!(auditor["metadata"].get(NOTE_KEY).is_none(), "{auditor}");

        let robot = stored(&base, "p3", "audit").await;
        assert_eq!(robot["metadata"][NOTE_KEY]["half"], "kind");
        assert_eq!(robot["metadata"][NOTE_KEY]["demanded"], "person");

        let mut released: Vec<String> = writes
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w.0 == "PUT")
            .map(|w| w.1.clone())
            .collect();
        released.sort();
        assert_eq!(released, ["p1-audit", "p3-audit"]);
    }

    /// THE VERDICT IS THE FRESH READ'S (backlog efc60d6d, folded into
    /// c17dec0f). The sweep judges the slim listing, then reads the
    /// packet whole before it writes — and that read used to be checked
    /// for the holder alone, so a packet `boss job convert` moved in
    /// between was released under the version it had LEFT, its note
    /// naming that version. Here the listing still shows fixture@2,
    /// whose `build` demands `model:opus` and refuses Codex, while the
    /// packet as it stands is pinned to fixture@1, which demands
    /// nothing: the fresh read is judged again, and nothing is written.
    #[tokio::test]
    async fn a_hold_whose_packet_moved_since_the_listing_is_judged_on_the_fresh_read() {
        let (base, writes) = mock_listed(
            vec![held("p1", 1, "build", "active", CODEX)],
            Some(vec![held("p1", 2, "build", "active", CODEX)]),
            agents(),
        )
        .await;
        handler(&base)
            .invoke(&[], &tick("2026-11-05T19:00:00Z"))
            .await
            .unwrap();
        assert!(
            writes.lock().unwrap().is_empty(),
            "fixture@1 admits the holder its listing's fixture@2 refused: {:?}",
            writes.lock().unwrap()
        );
        let step = stored(&base, "p1", "build").await;
        assert_eq!(step["status"], "active");
        assert_eq!(step["assignee_id"], CODEX);
    }

    /// The verdicts this arm does not take as a verdict on the holder,
    /// and one it does: a robot whose row records no class is refused
    /// on a model step, as the claim door refuses it; a station this arm
    /// cannot read, and a role the roster lists nobody holding — a dark
    /// or narrowed people read — are left, not released.
    #[tokio::test]
    async fn a_robot_with_no_class_is_refused_and_an_unread_registry_is_left() {
        let door = Roster::door();
        let model = ExecutorDemand::Model(vec!["opus".into()]);
        let unclassed = vec![json!({"id": CODEX, "model_class": null, "aliases": []})];
        match judge(&model, CODEX, &unclassed, &[], door.as_ref()).await {
            Ok(Verdict::Refused(r)) => {
                assert!(r.found.contains("records no model class"), "{r:?}")
            }
            other => panic!("{other:?}"),
        }
        let gates = ExecutorDemand::Equipment("gates".into());
        assert!(matches!(
            judge(&gates, RUNNER, &agents(), &[], door.as_ref()).await,
            Ok(Verdict::Unjudged(_))
        ));
        let role = ExecutorDemand::Role("auditor".into());
        let dark = Roster(vec![]);
        assert!(matches!(
            judge(&role, CLERK, &agents(), &[], &dark).await,
            Ok(Verdict::Unjudged(why)) if why.contains("lists no holder of auditor")
        ));
    }

    /// THE SHIPPED RULES ARE THE ONES THIS HANDLER SERVES: one on the
    /// assignment marker, one hourly, both naming this handler, parsed
    /// through the registry's own parser — and the cascade table knows
    /// what it emits.
    #[test]
    fn the_shipped_rules_name_this_handler_on_both_triggers() {
        let dir = boss_testing::dispatcher_rules_dir();
        let raw = boss_dispatcher::rules::registry::parse_raw_path(&dir)
            .unwrap_or_else(|e| panic!("parse {}: {e}", dir.display()));
        let find = |name: &str| {
            raw.rules
                .iter()
                .find(|r| r.name == name)
                .unwrap_or_else(|| panic!("{name} is in the shipped registry"))
        };
        let event = find(RULE);
        assert_eq!(event.on_event.as_deref(), Some("step.assigned.*"));
        let sweep = find(SWEEP);
        assert!(sweep.on_event.is_none() && sweep.schedule.is_some());
        for rule in [event, sweep] {
            assert_eq!(
                rule.do_steps
                    .iter()
                    .map(|d| d.handler.as_str())
                    .collect::<Vec<_>>(),
                [HANDLER],
                "{}",
                rule.name
            );
        }
        assert_eq!(handler("http://unused").name(), HANDLER);
        assert_eq!(
            crate::cascade::handler_emits().get(HANDLER).cloned(),
            Some(vec!["jobs.step.updated"])
        );
    }
}
