//! `jobs.idle_actor_watch` — an actor that holds READY urgent
//! nominations and has run nothing for a bound is an alarm that names
//! the actor, the work it holds, and how long it has been silent
//! (backlog 861360bc).
//!
//! THE CASE. Measured 2026-10-19 13:47Z by the conductor: from 12:52Z
//! to 13:40Z agent-claude-b had no open agent-run while four urgent
//! build steps were READY and nominated to it (f17627b6, 870dbfe4,
//! bbc13a4e, 3e2c8ddc, all nominated on 10-18), beside 16 ready
//! reviews and 45 ready triages. Nothing fired; David noticed the yard
//! was still. The session had stopped on a time-boxed chat order — the
//! cause half is backlog 37e3295c (the working window as packet data);
//! this is the detection half, because a factory can also go dark on a
//! session end, a crash or a context loss, none of which says so.
//!
//! WHY NO NEIGHBOUR COVERED IT. `an-urgent-item-is-released-when-unrun-
//! and-rescoped-when-long` reads a CLAIMED (active) urgent build with no
//! run, and an item open for days; a READY nomination nobody has claimed
//! is neither. `agent-held-real-work-is-watched-hourly` leaves
//! `backlog-item` undeclared on purpose. And both are keyed by the STEP:
//! the defect here is a property of the ACTOR — one alarm per idle
//! actor, never one per held step (four alarms for one stopped session
//! is the flat list shouted).
//!
//! THE CONDITION, per registered agent (every spelling the agents
//! registry ties to it):
//!   - it holds at least one READY step on an open packet whose priority
//!     the rule's `priorities` names (the shipped rule: urgent,
//!     emergency) — read from the actor's own queue
//!     (`/api/jobs/assignments?assignee_id=`), so a nomination on any
//!     kind counts, not only backlog builds;
//!   - NO agent-run of it is open (an open run is the actor working, and
//!     the watch is silent — packet check "silent while any run of the
//!     actor is open");
//!   - NO agent-run of it moved within `idle_minutes` — no run opened (a
//!     run's `claimed` trigger completes at dispatch) and none closed,
//!     read with `step_completed_since`, so a run that closed a minute
//!     ago holds the alarm whichever run was admitted last;
//!   - NO alarm of this rule for the actor is OPEN. Closing the alarm
//!     re-arms it: the next idle stretch files again.
//!
//! IT CARRIES THE CAUSE, NOT THE SYMPTOM. The alarm names the actor,
//! every ready urgent nomination (packet, step, when and by whom it was
//! nominated, oldest first), the actor's newest run and when it last
//! moved, and its open work-session's `last_active_at` — or that it has
//! no open session, which is itself the answer to "why".
//!
//! IT DOES NOT REPORT THROUGH ITS SUBJECT. The dispatcher's clock reads
//! the record; nothing here waits on the idle actor, its session or its
//! hooks. The alarm is owned by the platform owner and assigned to
//! nobody — routing it to the actor that stopped would be an alarm only
//! the silent can read.
//!
//! It files and never routes: no nomination is released or moved, no
//! run is opened. Those are the coordinator's acts, made on a read of
//! the alarm.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value as Json, json};

use super::common::{
    api_client, empty_roster_refusal, get_json, jobs_where_slim, owner_for_filing, post_json,
    rows_or_refuse, with_lane,
};
use super::jobs_age_out_step::last_moved;

/// The handler's registered name.
pub const HANDLER: &str = "jobs.idle_actor_watch";

/// The alarm's dedup key: the agent id it names. Read with
/// `metadata_has` over OPEN backlog items only, so a closed alarm
/// re-arms.
pub const ACTOR_KEY: &str = "idle_actor";

/// The `scope` every alarm this handler files carries.
pub const SCOPE: &str = "idle-actor-holding-urgent-work";

/// The run kind, and the key on it naming the actor it signs as
/// (infra/platform/workflows/agent-run.toml: `agent`).
const RUN_KIND: &str = "agent-run";
const RUN_AGENT: &str = "agent";

/// The session kind, the key naming its actor, and its heartbeat
/// (infra/platform/workflows/work-session.toml).
const SESSION_KIND: &str = "work-session";
const SESSION_ACTOR: &str = "actor";
const HEARTBEAT: &str = "last_active_at";

/// The step key `boss step nominate` writes (boss-cli `nominate_step`).
const NOMINATION: &str = "nomination_requested";

/// The jobs API's own page ceiling for a queue read (boss-jobs
/// `http::MAX_LIMIT`). A queue that fills it is said, never trusted as
/// whole.
const QUEUE_LIMIT: usize = 1000;

/// The key the schedule runner writes the firing instant under.
const TICK_AT: &str = "_at";

fn is_priority(word: &str) -> bool {
    serde_json::from_value::<boss_core::job::Priority>(json!(word)).is_ok()
}

/// The rule's declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Bounds {
    pub priorities: BTreeSet<String>,
    pub idle_minutes: i64,
}

/// Parse the rule's args. Both are required; a malformed one refuses by
/// name — a watch reading nothing is a watch that is not running.
pub fn bounds(args: &[(String, Value)]) -> Result<Bounds, String> {
    let priorities: BTreeSet<String> = match arg(args, "priorities") {
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
        Some(other) => {
            return Err(format!(
                "`priorities` must be a comma-separated string of job priorities, got a {} \
                 ({other:?})",
                other.kind()
            ));
        }
        None => return Err("`priorities` is not declared on the rule".into()),
    };
    if priorities.is_empty() {
        return Err("`priorities` names no priority — the watch would read nothing".into());
    }
    if let Some(bad) = priorities.iter().find(|p| !is_priority(p)) {
        return Err(format!(
            "`priorities` names `{bad}`, which is not a job priority — it would match no step"
        ));
    }
    let idle_minutes = match arg(args, "idle_minutes") {
        Some(Value::Int(n)) if *n > 0 => *n,
        Some(other) => {
            return Err(format!(
                "`idle_minutes` must be a positive whole number, got a {} ({other:?})",
                other.kind()
            ));
        }
        None => {
            return Err(
                "`idle_minutes` is not declared on the rule — the bound is the rule's to state"
                    .into(),
            );
        }
    };
    Ok(Bounds {
        priorities,
        idle_minutes,
    })
}

/// One registered agent and every spelling the registry ties to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    pub id: String,
    pub spellings: BTreeSet<String>,
}

/// The agents registry's rows as actors — id plus aliases, each actor
/// once.
pub fn actors(agents: &[Json]) -> Vec<Actor> {
    agents
        .iter()
        .filter_map(|a| {
            let id = a
                .get("id")
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())?;
            let spellings = std::iter::once(id.to_string())
                .chain(
                    a.get("aliases")
                        .and_then(Json::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Json::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                )
                .collect();
            Some(Actor {
                id: id.to_string(),
                spellings,
            })
        })
        .collect()
}

fn text<'a>(v: &'a Json, k: &str) -> &'a str {
    v.get(k).and_then(Json::as_str).unwrap_or("")
}

fn instant(v: Option<&Json>) -> Option<DateTime<Utc>> {
    v.and_then(Json::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// One READY step on a watched-priority packet, held by the actor.
#[derive(Debug, Clone, PartialEq)]
pub struct Nomination {
    pub packet: String,
    pub packet_title: String,
    pub priority: String,
    pub step_id: String,
    pub slug: String,
    pub assignee: String,
    /// `nomination_requested.at`, when the step carries one naming this
    /// holder. A step assigned some other way carries none.
    pub nominated_at: Option<DateTime<Utc>>,
    pub nominated_by: Option<String>,
}

/// The actor's ready nominations of a watched priority, OLDEST first
/// (an unstamped one after every stamped one, then by packet id). Pure.
/// `rows` is the queue read (`AssignmentRow`s): the packet's priority
/// and the whole step ride on each row.
pub fn nominations(rows: &[Json], actor: &Actor, b: &Bounds) -> Vec<Nomination> {
    let mut out: Vec<Nomination> = rows
        .iter()
        .filter(|r| b.priorities.contains(text(r, "priority")))
        .filter_map(|r| {
            let step = r.get("step")?;
            let assignee = text(step, "assignee_id");
            if text(step, "status") != "ready" || !actor.spellings.contains(assignee) {
                return None;
            }
            let nomination = step
                .pointer(&format!("/metadata/{NOMINATION}"))
                .filter(|n| text(n, "assignee_id") == assignee);
            Some(Nomination {
                packet: text(r, "job_id").to_string(),
                packet_title: text(r, "job_title").to_string(),
                priority: text(r, "priority").to_string(),
                step_id: text(step, "id").to_string(),
                slug: text(step, "spec_slug").to_string(),
                assignee: assignee.to_string(),
                nominated_at: instant(nomination.and_then(|n| n.get("at"))),
                nominated_by: nomination
                    .map(|n| text(n, "by"))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (a.nominated_at.is_none(), a.nominated_at, &a.packet).cmp(&(
            b.nominated_at.is_none(),
            b.nominated_at,
            &b.packet,
        ))
    });
    out.dedup_by(|a, b| a.step_id == b.step_id);
    out
}

/// The actor's newest run, as the alarm names it.
#[derive(Debug, Clone, PartialEq)]
pub struct LastRun {
    pub id: String,
    pub packet: String,
    pub status: String,
    pub opened_at: Option<DateTime<Utc>>,
    /// The newest step completion on it — its close, for a closed run.
    pub last_moved: Option<DateTime<Utc>>,
}

/// The newest of the runs read (by last movement), or `None`.
pub fn last_run(runs: &[Json]) -> Option<LastRun> {
    runs.iter()
        .map(|r| LastRun {
            id: text(r, "id").to_string(),
            packet: r
                .pointer("/metadata/packet")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string(),
            status: text(r, "status").to_string(),
            opened_at: instant(r.pointer("/metadata/opened_at")),
            last_moved: last_moved(r, None),
        })
        .max_by_key(|r| r.last_moved)
}

/// The actor's open session, as the alarm names it: the newest
/// heartbeat across its open work-sessions.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: String,
    pub last_active_at: Option<DateTime<Utc>>,
}

pub fn open_session(sessions: &[Json]) -> Option<Session> {
    sessions
        .iter()
        .filter(|s| text(s, "status") == "open")
        .map(|s| Session {
            id: text(s, "id").to_string(),
            last_active_at: instant(s.pointer(&format!("/metadata/{HEARTBEAT}"))),
        })
        .max_by_key(|s| s.last_active_at)
}

/// Every actor an open run names, as the registry's id.
pub fn running(open_runs: &[Json], actors: &[Actor]) -> BTreeSet<String> {
    open_runs
        .iter()
        .filter(|r| text(r, "status") == "open")
        .filter_map(|r| {
            r.pointer(&format!("/metadata/{RUN_AGENT}"))
                .and_then(Json::as_str)
        })
        .filter_map(|who| actors.iter().find(|a| a.spellings.contains(who)))
        .map(|a| a.id.clone())
        .collect()
}

/// Every actor an OPEN alarm of this handler names.
pub fn alarmed(open_alarms: &[Json]) -> BTreeSet<String> {
    open_alarms
        .iter()
        .filter(|r| text(r, "status") == "open")
        .filter_map(|r| {
            r.pointer(&format!("/metadata/{ACTOR_KEY}"))
                .and_then(Json::as_str)
        })
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// What one alarm states.
#[derive(Debug, Clone, PartialEq)]
pub struct Idle {
    pub actor: String,
    pub nominations: Vec<Nomination>,
    pub last_run: Option<LastRun>,
    pub session: Option<Session>,
    pub bound_minutes: i64,
}

fn stamp(t: Option<DateTime<Utc>>) -> String {
    t.map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "unstamped".into())
}

fn minutes(since: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<i64> {
    since.map(|t| (now - t).num_minutes())
}

/// The alarm one idle actor files.
pub fn alarm_body(i: &Idle, owner: &str, now: DateTime<Utc>, rule: &str) -> Json {
    let oldest = &i.nominations[0];
    let id8 = |s: &str| s[..s.len().min(8)].to_string();
    let silent_since = i.last_run.as_ref().and_then(|r| r.last_moved);
    let held: Vec<String> = i
        .nominations
        .iter()
        .map(|n| {
            format!(
                "{} `{}` on {} ({}, {}) nominated {} by {}",
                n.priority,
                n.slug,
                n.packet,
                n.packet_title,
                n.assignee,
                stamp(n.nominated_at),
                n.nominated_by.as_deref().unwrap_or("an unrecorded hand"),
            )
        })
        .collect();
    let run = match &i.last_run {
        Some(r) => format!(
            "its newest agent-run is {} (for packet {}, {}), opened {} and last moved {} — \
             {} minute(s) before this tick",
            r.id,
            r.packet,
            r.status,
            stamp(r.opened_at),
            stamp(r.last_moved),
            minutes(r.last_moved, now)
                .map(|m| m.to_string())
                .unwrap_or_else(|| "an unknown number of".into()),
        ),
        None => "it has no agent-run on record at all".into(),
    };
    let session = match &i.session {
        Some(s) => format!(
            "its open work-session {} last prompted at {}",
            s.id,
            stamp(s.last_active_at)
        ),
        None => "it has NO open work-session — the session that would run this work has ended \
                 or never started"
            .into(),
    };
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog 861360bc) at {now}: {actor} holds {n} READY \
         nomination(s) of a watched priority and has opened or closed no agent-run for at least \
         {bound} minutes, with none open. Held: {held}. The oldest is {oldest_slug} on {oldest} \
         ({oldest_age}). {run}; {session}. Nothing is executing this work. Either start a \
         session that dispatches it (`boss dispatch <packet> --step <slug>`), or release it to \
         someone who will (`boss step release <packet> --step <slug> --why <text>`). This \
         alarm files once per idle stretch: closing it re-arms the watch. It refuses nothing \
         and routes nothing.",
        now = now.to_rfc3339(),
        actor = i.actor,
        n = i.nominations.len(),
        bound = i.bound_minutes,
        held = held.join("; "),
        oldest_slug = oldest.slug,
        oldest = oldest.packet,
        oldest_age = match minutes(oldest.nominated_at, now) {
            Some(m) => format!("nominated {m} minute(s) ago"),
            None => "nomination unstamped".into(),
        },
    );
    let metadata = json!({
        "area": "agents",
        "scope": SCOPE,
        ACTOR_KEY: i.actor,
        "nominations": i.nominations.iter().map(|n| json!({
            "packet": n.packet,
            "packet_title": n.packet_title,
            "priority": n.priority,
            "step": n.slug,
            "step_id": n.step_id,
            "assignee": n.assignee,
            "nominated_at": n.nominated_at.map(|t| t.to_rfc3339()),
            "nominated_by": n.nominated_by,
        })).collect::<Vec<_>>(),
        "oldest_nomination": oldest.packet,
        "last_run": i.last_run.as_ref().map(|r| json!({
            "id": r.id,
            "packet": r.packet,
            "status": r.status,
            "opened_at": r.opened_at.map(|t| t.to_rfc3339()),
            "last_moved": r.last_moved.map(|t| t.to_rfc3339()),
        })),
        "silent_since": silent_since.map(|t| t.to_rfc3339()),
        "silent_minutes": minutes(silent_since, now),
        "session": i.session.as_ref().map(|s| json!({
            "id": s.id,
            HEARTBEAT: s.last_active_at.map(|t| t.to_rfc3339()),
        })),
        "bound_minutes": i.bound_minutes,
        "measured_at": now.to_rfc3339(),
        "detail": detail,
    });
    json!({
        "kind": "backlog-item",
        "title": format!(
            "IDLE: {} holds {} ready {} nomination(s) and has run nothing for {}m — oldest {} `{}`",
            i.actor,
            i.nominations.len(),
            oldest.priority,
            minutes(silent_since, now)
                .map(|m| m.to_string())
                .unwrap_or_else(|| format!("{}+", i.bound_minutes)),
            id8(&oldest.packet),
            oldest.slug,
        ),
        "subject": {"subject_kind": "custom", "id": i.actor},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

/// A query string, form-encoded — an alias carries `@` and `.`, a
/// containment filter `{`, `"` and `:`.
pub fn query(pairs: &[(&str, &str)]) -> String {
    reqwest::Url::parse_with_params("http://q.invalid/", pairs)
        .ok()
        .and_then(|u| u.query().map(str::to_string))
        .unwrap_or_default()
}

/// `{"<key>":"<value>"}`, the containment door's argument.
fn containment(key: &str, value: &str) -> String {
    json!({ key: value }).to_string()
}

/// An RFC3339 instant in the `Z` spelling: a `+00:00` in a query string
/// is a space once decoded.
fn zulu(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Every row a per-actor read answered must be the actor's: a server
/// that ignored the filter answers somebody else's page, and judging
/// it would hold or fire on another actor's runs.
fn own_rows(rows: Vec<Json>, key: &str, actor: &Actor, what: &str) -> Result<Vec<Json>, String> {
    if let Some(stray) = rows.iter().find(|r| {
        !r.pointer(&format!("/metadata/{key}"))
            .and_then(Json::as_str)
            .is_some_and(|w| actor.spellings.contains(w))
    }) {
        return Err(format!(
            "the {what} read for {} answered {} that names another actor — the metadata filter \
             was not applied",
            actor.id,
            text(stray, "id")
        ));
    }
    Ok(rows)
}

pub struct JobsIdleActorWatch {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsIdleActorWatch {
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

    /// The actor's queue, every spelling.
    async fn queue(&self, actor: &Actor, rule: &str) -> Result<Vec<Json>, HandlerError> {
        let mut rows = Vec::new();
        for who in &actor.spellings {
            let body = get_json(
                &self.client,
                &format!(
                    "{}/api/jobs/assignments?{}",
                    self.base(),
                    query(&[("assignee_id", who), ("limit", &QUEUE_LIMIT.to_string())])
                ),
                rule,
            )
            .await?;
            let page: Vec<Json> = rows_or_refuse(&body, "GET /api/jobs/assignments")
                .map_err(HandlerError::Downstream)?;
            if page.len() >= QUEUE_LIMIT {
                tracing::warn!(
                    rule,
                    actor = %actor.id,
                    spelling = %who,
                    "{HANDLER}: the queue read filled its {QUEUE_LIMIT}-row page; a nomination \
                     past it is not seen"
                );
            }
            rows.extend(page);
        }
        Ok(rows)
    }

    /// Did any run of the actor move (open or close a step) at or after
    /// `since`? Read by count, never by page: the answer is `total`.
    async fn moved_since(
        &self,
        actor: &Actor,
        since: DateTime<Utc>,
        rule: &str,
    ) -> Result<bool, HandlerError> {
        for who in &actor.spellings {
            let body = get_json(
                &self.client,
                &format!(
                    "{}/api/jobs?{}",
                    self.base(),
                    query(&[
                        ("kind", RUN_KIND),
                        ("metadata", &containment(RUN_AGENT, who)),
                        ("step_completed_since", &zulu(since)),
                        ("full", "false"),
                        ("limit", "1"),
                    ])
                ),
                rule,
            )
            .await?;
            let rows: Vec<Json> =
                rows_or_refuse(&body, "GET /api/jobs").map_err(HandlerError::Downstream)?;
            own_rows(rows, RUN_AGENT, actor, "agent-run").map_err(HandlerError::Downstream)?;
            let total = body.get("total").and_then(Json::as_u64).ok_or_else(|| {
                HandlerError::Downstream(format!(
                    "the agent-run movement read for {} carried no `total` — a count that is \
                     not there is not zero",
                    actor.id
                ))
            })?;
            if total > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The actor's newest run, by admission, every spelling.
    async fn newest_run(&self, actor: &Actor, rule: &str) -> Result<Option<LastRun>, HandlerError> {
        let mut runs = Vec::new();
        for who in &actor.spellings {
            let body = get_json(
                &self.client,
                &format!(
                    "{}/api/jobs?{}",
                    self.base(),
                    query(&[
                        ("kind", RUN_KIND),
                        ("metadata", &containment(RUN_AGENT, who)),
                        ("order", "newest"),
                        ("full", "false"),
                        ("limit", "3"),
                    ])
                ),
                rule,
            )
            .await?;
            let rows: Vec<Json> =
                rows_or_refuse(&body, "GET /api/jobs").map_err(HandlerError::Downstream)?;
            runs.extend(
                own_rows(rows, RUN_AGENT, actor, "agent-run").map_err(HandlerError::Downstream)?,
            );
        }
        Ok(last_run(&runs))
    }

    async fn session(&self, actor: &Actor, rule: &str) -> Result<Option<Session>, HandlerError> {
        let mut sessions = Vec::new();
        for who in &actor.spellings {
            let rows = jobs_where_slim(
                &self.client,
                self.base(),
                &query(&[
                    ("kind", SESSION_KIND),
                    ("status", "open"),
                    ("metadata", &containment(SESSION_ACTOR, who)),
                ]),
                rule,
            )
            .await?;
            sessions.extend(
                own_rows(rows, SESSION_ACTOR, actor, "work-session")
                    .map_err(HandlerError::Downstream)?,
            );
        }
        Ok(open_session(&sessions))
    }
}

#[async_trait]
impl Handler for JobsIdleActorWatch {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let b = bounds(args).map_err(HandlerError::Permanent)?;
        let now = ctx
            .event_payload
            .get(TICK_AT)
            .and_then(Json::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "the firing carries no `{TICK_AT}` — {HANDLER} judges minutes and needs a \
                     sub-day cadence (every-<n>-minutes)"
                ))
            })?;

        let listing = get_json(&self.client, &format!("{}/api/agents", self.base()), rule).await?;
        let rows: Vec<Json> =
            rows_or_refuse(&listing, "GET /api/agents").map_err(HandlerError::Downstream)?;
        let actors = actors(&rows);
        if actors.is_empty() {
            return Err(empty_roster_refusal(
                HANDLER,
                "agents",
                "every instance runs at least one registered agent, and an empty read is a dark \
                 or narrowed registry — judging it would read every factory as never started",
            ));
        }
        let busy = running(
            &jobs_where_slim(
                &self.client,
                self.base(),
                &query(&[("kind", RUN_KIND), ("status", "open")]),
                rule,
            )
            .await?,
            &actors,
        );
        let raised = alarmed(
            &jobs_where_slim(
                &self.client,
                self.base(),
                &query(&[
                    ("kind", "backlog-item"),
                    ("status", "open"),
                    ("metadata_has", ACTOR_KEY),
                ]),
                rule,
            )
            .await?,
        );

        let owner = owner_for_filing(self.owner.as_ref(), rule).await;
        let cutoff = now - Duration::minutes(b.idle_minutes);
        let mut filed = 0usize;
        for actor in &actors {
            if busy.contains(&actor.id) || raised.contains(&actor.id) {
                continue;
            }
            let held = nominations(&self.queue(actor, rule).await?, actor, &b);
            if held.is_empty() || self.moved_since(actor, cutoff, rule).await? {
                continue;
            }
            let idle = Idle {
                actor: actor.id.clone(),
                nominations: held,
                last_run: self.newest_run(actor, rule).await?,
                session: self.session(actor, rule).await?,
                bound_minutes: b.idle_minutes,
            };
            post_json(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                &alarm_body(&idle, &owner, now, rule),
                rule,
            )
            .await?;
            filed += 1;
        }
        tracing::info!(
            rule,
            actors = actors.len(),
            running = busy.len(),
            already_alarmed = raised.len(),
            filed,
            "{HANDLER}: read {} actor(s)",
            actors.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    const B: &str = "agent-claude-b";
    const B_ALIAS: &str = "claude-b@algedonic.dev";

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn ctx(now: &str) -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "an-idle-actor-holding-urgent-nominations-is-alarmed".into(),
            triggering_event_id: format!("clock-tick:{now}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": &now[..10], "_at": now}),
        }
    }

    fn args() -> Vec<(String, Value)> {
        vec![
            (
                "priorities".to_string(),
                Value::String("urgent,emergency".into()),
            ),
            ("idle_minutes".to_string(), Value::Int(30)),
        ]
    }

    fn b() -> Bounds {
        bounds(&args()).unwrap()
    }

    fn agents_rows() -> Vec<Json> {
        vec![
            json!({"id": B, "aliases": [B_ALIAS]}),
            json!({"id": "agent-claude", "aliases": ["claude@algedonic.dev"]}),
        ]
    }

    fn actor_b() -> Actor {
        actors(&agents_rows()).remove(0)
    }

    /// A queue row as `/api/jobs/assignments` answers it: the packet's
    /// identity and priority, and the whole step.
    fn queue_row(
        packet: &str,
        priority: &str,
        slug: &str,
        status: &str,
        nominated: Option<&str>,
    ) -> Json {
        let mut step = json!({
            "id": format!("{packet}-{slug}"), "spec_slug": slug, "status": status,
            "assignee_id": B, "metadata": {},
        });
        if let Some(t) = nominated {
            step["metadata"][NOMINATION] = json!({
                "assignee_id": B, "at": t, "by": "claude@algedonic.dev", "why": "build it",
            });
        }
        json!({
            "job_id": packet, "job_title": format!("item {packet}"), "workflow": "backlog-item",
            "priority": priority, "step": step,
        })
    }

    /// THE MEASURED QUEUE, 2026-10-19 12:52Z–13:40Z: four urgent builds
    /// ready on agent-claude-b, nominated on 10-18, beside ready
    /// standard reviews and triages (16 and 45 live; a few here).
    fn measured_queue() -> Vec<Json> {
        let mut rows = vec![
            queue_row(
                "f17627b6-0000-4000-8000-000000000001",
                "urgent",
                "build",
                "ready",
                Some("2026-10-18T20:30:00Z"),
            ),
            queue_row(
                "870dbfe4-0000-4000-8000-000000000002",
                "urgent",
                "build",
                "ready",
                Some("2026-10-18T15:45:00Z"),
            ),
            queue_row(
                "bbc13a4e-0000-4000-8000-000000000003",
                "urgent",
                "build",
                "ready",
                Some("2026-10-18T20:29:00Z"),
            ),
            queue_row(
                "3e2c8ddc-0000-4000-8000-000000000004",
                "urgent",
                "build",
                "ready",
                Some("2026-10-18T03:28:00Z"),
            ),
        ];
        for i in 0..3 {
            rows.push(queue_row(
                &format!("review-{i}"),
                "standard",
                "review",
                "ready",
                None,
            ));
            rows.push(queue_row(
                &format!("triage-{i}"),
                "standard",
                "triage",
                "ready",
                None,
            ));
        }
        rows
    }

    /// B's last run before the stall: opened 11:40Z, closed 12:52Z.
    fn last_run_row() -> Json {
        json!({
            "id": "run-last", "kind": "agent-run", "status": "closed",
            "metadata": {"packet": "item-earlier", "agent": B, "opened_at": "2026-10-19T11:40:00Z"},
            "steps": [
                {"id": "c", "spec_slug": "claimed", "status": "completed",
                 "completed_at": "2026-10-19T11:40:00Z"},
                {"id": "b", "spec_slug": "building", "status": "completed",
                 "completed_at": "2026-10-19T12:52:00Z"},
            ],
        })
    }

    fn session_row() -> Json {
        json!({
            "id": "eaf1cfac-session", "kind": "work-session", "status": "open",
            "metadata": {"actor": B, "host": "boss-dev-0", "started_at": "2026-10-18T15:50:00Z",
                         HEARTBEAT: "2026-10-19T12:52:30Z"},
        })
    }

    fn listing(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    fn route(path: &str, pairs: &[(&str, &str)]) -> &'static str {
        Box::leak(format!("{path}?{}", query(pairs)).into_boxed_str())
    }

    /// The record as the jobs API would answer it AT `now`: the run
    /// read under `step_completed_since` holds the last run only when a
    /// step of it completed at or after the cutoff, as the server
    /// filters; the open-alarm read holds whatever this replay filed.
    struct Record {
        queue: Vec<Json>,
        runs: Vec<Json>,
        open_runs: Vec<Json>,
        alarms: Vec<Json>,
        sessions: Vec<Json>,
    }

    impl Record {
        fn measured() -> Self {
            Record {
                queue: measured_queue(),
                runs: vec![last_run_row()],
                open_runs: vec![],
                alarms: vec![],
                sessions: vec![session_row()],
            }
        }

        async fn serve(&self, now: DateTime<Utc>) -> listing_stub::Stub {
            let cutoff = now - Duration::minutes(30);
            let moved: Vec<Json> = self
                .runs
                .iter()
                .filter(|r| last_moved(r, None).is_some_and(|t| t >= cutoff))
                .cloned()
                .collect();
            let mut answers: Vec<(&'static str, Json)> =
                vec![("/api/agents", listing(agents_rows()))];
            for who in [B, B_ALIAS, "agent-claude", "claude@algedonic.dev"] {
                let mine = |rows: &[Json], key: &str| -> Vec<Json> {
                    rows.iter()
                        .filter(|r| {
                            r.pointer(&format!("/metadata/{key}"))
                                .and_then(Json::as_str)
                                == Some(who)
                        })
                        .cloned()
                        .collect()
                };
                let queue: Vec<Json> = self
                    .queue
                    .iter()
                    .filter(|r| r.pointer("/step/assignee_id").and_then(Json::as_str) == Some(who))
                    .cloned()
                    .collect();
                answers.push((
                    route("/api/jobs/assignments", &[("assignee_id", who)]),
                    listing(queue),
                ));
                answers.push((
                    route(
                        "/api/jobs",
                        &[
                            ("kind", RUN_KIND),
                            ("metadata", &containment(RUN_AGENT, who)),
                            ("step_completed_since", &zulu(cutoff)),
                        ],
                    ),
                    listing(mine(&moved, RUN_AGENT)),
                ));
                answers.push((
                    route(
                        "/api/jobs",
                        &[
                            ("kind", RUN_KIND),
                            ("metadata", &containment(RUN_AGENT, who)),
                            ("order", "newest"),
                        ],
                    ),
                    listing(mine(&self.runs, RUN_AGENT)),
                ));
                answers.push((
                    route(
                        "/api/jobs",
                        &[
                            ("kind", SESSION_KIND),
                            ("status", "open"),
                            ("metadata", &containment(SESSION_ACTOR, who)),
                        ],
                    ),
                    listing(mine(&self.sessions, SESSION_ACTOR)),
                ));
            }
            answers.push((
                route("/api/jobs", &[("kind", RUN_KIND), ("status", "open")]),
                listing(self.open_runs.clone()),
            ));
            answers.push((
                route(
                    "/api/jobs",
                    &[
                        ("kind", "backlog-item"),
                        ("status", "open"),
                        ("metadata_has", ACTOR_KEY),
                    ],
                ),
                listing_stub::backlog_listing(&self.alarms, Some(ACTOR_KEY), None),
            ));
            listing_stub::serve(answers).await
        }
    }

    fn handler(stub: &listing_stub::Stub) -> Arc<JobsIdleActorWatch> {
        JobsIdleActorWatch::new(
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    /// One tick against the record as it stands at `now`.
    async fn tick(record: &Record, now: &str) -> Vec<(String, Json)> {
        let stub = record.serve(at(now)).await;
        handler(&stub).invoke(&args(), &ctx(now)).await.unwrap();
        stub.sent()
    }

    // -- the declaration -------------------------------------------------

    #[test]
    fn the_declaration_is_a_priority_scope_and_a_positive_minutes_bound() {
        let ok = b();
        assert_eq!(
            ok.priorities,
            BTreeSet::from(["emergency".to_string(), "urgent".to_string()])
        );
        assert_eq!(ok.idle_minutes, 30);
        for bad in [Value::Int(0), Value::Int(-5), Value::String("30".into())] {
            let mut a = args();
            a[1].1 = bad.clone();
            assert!(bounds(&a).unwrap_err().contains("idle_minutes"), "{bad:?}");
        }
        assert!(bounds(&args()[..1]).unwrap_err().contains("idle_minutes"));
        for bad in ["", " , ", "urgnet"] {
            let mut a = args();
            a[0].1 = Value::String(bad.into());
            assert!(bounds(&a).unwrap_err().contains("priorities"), "{bad:?}");
        }
    }

    // -- the pure reads --------------------------------------------------

    /// Only READY steps of a watched priority on the actor's spellings,
    /// oldest nomination first.
    #[test]
    fn the_held_work_is_the_ready_urgent_nominations_oldest_first() {
        let mut rows = measured_queue();
        rows.push(queue_row("claimed", "urgent", "build", "active", None));
        let held = nominations(&rows, &actor_b(), &b());
        let order: Vec<&str> = held.iter().map(|n| &n.packet[..8]).collect();
        assert_eq!(order, ["3e2c8ddc", "870dbfe4", "bbc13a4e", "f17627b6"]);
        assert!(
            held.iter()
                .all(|n| n.priority == "urgent" && n.slug == "build")
        );
        assert_eq!(held[0].nominated_at, Some(at("2026-10-18T03:28:00Z")));
        assert_eq!(
            held[0].nominated_by.as_deref(),
            Some("claude@algedonic.dev")
        );
        let standard_only: Vec<Json> = rows
            .into_iter()
            .filter(|r| r["priority"] == "standard")
            .collect();
        assert!(nominations(&standard_only, &actor_b(), &b()).is_empty());
    }

    // -- the measured shape, replayed --------------------------------------

    /// THE FINISH LINE. A replay of 2026-10-19 12:52Z to 13:40Z, one
    /// tick every five minutes as the shipped rule fires: silent while
    /// the last run's close is inside the thirty minutes, ONE alarm at
    /// the first tick past it, and nothing after, because the alarm it
    /// filed is open. The alarm names agent-claude-b and every held
    /// nomination, f17627b6 among them, the last run, and the session's
    /// last prompt.
    #[tokio::test]
    async fn a_replay_of_the_measured_stall_files_exactly_one_alarm() {
        let mut record = Record::measured();
        let mut filed: Vec<(String, Json)> = Vec::new();
        let mut minute = 55;
        while minute <= 100 {
            let now = format!("2026-10-19T{:02}:{:02}:00Z", 12 + minute / 60, minute % 60);
            let sent = tick(&record, &now).await;
            for (path, body) in &sent {
                assert_eq!(path, "POST /api/jobs");
                let mut row = body.clone();
                row["id"] = json!(format!("alarm-{now}"));
                record.alarms.push(row);
                filed.push((now.clone(), body.clone()));
            }
            minute += 5;
        }
        assert_eq!(filed.len(), 1, "{filed:#?}");
        let (when, body) = &filed[0];
        assert_eq!(when, "2026-10-19T13:25:00Z", "first tick past 12:52Z + 30m");
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["priority"], "urgent");
        assert_eq!(body["owner_id"], "emp-owner");
        assert!(
            body.get("assignee_id").is_none(),
            "never routed to the idle actor"
        );
        let m = &body["metadata"];
        assert_eq!(m[ACTOR_KEY], B);
        assert_eq!(m["scope"], SCOPE);
        assert_eq!(m["input_channel"], "telemetry/monitoring");
        assert_eq!(m["nominations"].as_array().unwrap().len(), 4);
        assert_eq!(
            m["oldest_nomination"],
            "3e2c8ddc-0000-4000-8000-000000000004"
        );
        assert_eq!(m["last_run"]["id"], "run-last");
        assert_eq!(m["silent_since"], "2026-10-19T12:52:00+00:00");
        assert_eq!(m["silent_minutes"], 33);
        assert_eq!(m["session"]["id"], "eaf1cfac-session");
        assert_eq!(m["session"][HEARTBEAT], "2026-10-19T12:52:30+00:00");
        let detail = m["detail"].as_str().unwrap();
        for named in [
            B, "f17627b6", "870dbfe4", "bbc13a4e", "3e2c8ddc", "run-last", "eaf1cfac",
        ] {
            assert!(detail.contains(named), "the alarm names {named}: {detail}");
        }
        let title = body["title"].as_str().unwrap();
        assert!(title.contains(B) && title.contains("33m"), "{title}");
    }

    /// Closing the alarm re-arms: the same stall read with the alarm
    /// closed files again.
    #[tokio::test]
    async fn a_closed_alarm_re_arms_the_watch() {
        let mut record = Record::measured();
        record.alarms.push(json!({
            "id": "al-1", "status": "open", "metadata": {"scope": SCOPE, ACTOR_KEY: B},
        }));
        assert!(
            tick(&record, "2026-10-19T13:40:00Z").await.is_empty(),
            "held by the open alarm"
        );
        record.alarms[0]["status"] = json!("closed");
        assert_eq!(
            tick(&record, "2026-10-19T13:40:00Z").await.len(),
            1,
            "closed: re-armed"
        );
    }

    /// Silent while ANY run of the actor is open, under any spelling.
    #[tokio::test]
    async fn an_open_run_holds_the_watch_silent() {
        for spelling in [B, B_ALIAS] {
            let mut record = Record::measured();
            record.open_runs = vec![json!({
                "id": "run-open", "kind": "agent-run", "status": "open",
                "metadata": {"packet": "anything", "agent": spelling},
            })];
            assert!(
                tick(&record, "2026-10-19T13:40:00Z").await.is_empty(),
                "{spelling}"
            );
        }
    }

    /// Silent when no urgent nomination is ready: standard work only,
    /// or the urgent build already claimed (the urgent-item watch's).
    #[tokio::test]
    async fn no_ready_urgent_nomination_is_silence() {
        let mut record = Record::measured();
        record.queue.retain(|r| r["priority"] == "standard");
        assert!(tick(&record, "2026-10-19T13:40:00Z").await.is_empty());
        let mut record = Record::measured();
        for r in record
            .queue
            .iter_mut()
            .filter(|r| r["priority"] == "urgent")
        {
            r["step"]["status"] = json!("active");
        }
        assert!(tick(&record, "2026-10-19T13:40:00Z").await.is_empty());
    }

    /// A run that moved inside the bound holds it, even when it is not
    /// the newest-admitted run.
    #[tokio::test]
    async fn a_run_that_moved_inside_the_bound_holds_the_watch() {
        let mut record = Record::measured();
        let mut late = last_run_row();
        late["id"] = json!("run-older-closed-late");
        late["steps"][1]["completed_at"] = json!("2026-10-19T13:20:00Z");
        record.runs.push(late);
        assert!(tick(&record, "2026-10-19T13:40:00Z").await.is_empty());
    }

    /// No open session is named as the cause, not skipped.
    #[tokio::test]
    async fn an_actor_with_no_open_session_is_named_as_such() {
        let mut record = Record::measured();
        record.sessions.clear();
        let sent = tick(&record, "2026-10-19T13:40:00Z").await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1["metadata"]["session"], Json::Null);
        assert!(
            sent[0].1["metadata"]["detail"]
                .as_str()
                .unwrap()
                .contains("NO open work-session")
        );
    }

    #[tokio::test]
    async fn an_empty_agents_registry_is_refused_not_read_as_no_agents() {
        let stub = listing_stub::serve(vec![("/api/agents", listing_stub::empty_listing())]).await;
        let err = handler(&stub)
            .invoke(&args(), &ctx("2026-10-19T13:40:00Z"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("agents")),
            "{err}"
        );
        assert!(stub.writes().is_empty());
    }

    #[tokio::test]
    async fn a_firing_without_an_instant_is_a_permanent_refusal() {
        let stub = listing_stub::serve(vec![]).await;
        let mut c = ctx("2026-10-19T13:40:00Z");
        c.event_payload = json!({"_day": "2026-10-19"});
        let err = handler(&stub).invoke(&args(), &c).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("_at")),
            "{err}"
        );
    }

    /// The shipped row: urgent and emergency, thirty minutes, on a
    /// cadence of minutes (a daily or hourly tick would let the stall
    /// run past its bound by most of the window).
    #[test]
    fn the_shipped_rule_watches_urgent_and_emergency_for_thirty_minutes() {
        let raw =
            boss_dispatcher::rules::registry::parse_raw_path(boss_testing::dispatcher_rules_dir())
                .expect("parse the shipped rule registry");
        let rule = raw
            .rules
            .iter()
            .find(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
            .expect("a shipped rule invokes this handler");
        let args: Vec<(String, Value)> = rule
            .do_steps
            .iter()
            .flat_map(|d| d.args.iter())
            .map(|(k, v)| {
                let v = v.trim();
                let value = match v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    Some(s) => Value::String(s.to_string()),
                    None => Value::Int(v.parse().unwrap_or(-1)),
                };
                (k.clone(), value)
            })
            .collect();
        let declared = bounds(&args).unwrap_or_else(|e| panic!("{}: {e}", rule.name));
        assert_eq!(
            declared.priorities,
            BTreeSet::from(["emergency".to_string(), "urgent".to_string()])
        );
        assert_eq!(declared.idle_minutes, 30);
        let cadence = rule.schedule.as_ref().map(|s| s.cadence);
        assert!(
            matches!(
                cadence,
                Some(boss_dispatcher::rules::registry::Cadence::EveryNMinutes(n)) if n <= 10
            ),
            "{}: {cadence:?} — the bound is minutes, so the tick must be",
            rule.name
        );
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsIdleActorWatch::new(
            "http://unused",
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        assert_eq!(h.name(), HANDLER);
        assert!(
            crate::cascade::handler_emits().contains_key(HANDLER),
            "the cascade table knows what this handler emits"
        );
    }
}
