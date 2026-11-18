//! `jobs.factory_idle_watch` — the WHOLE factory stopped: no car gate
//! has launched for a bound while ready builds and reviews wait, and the
//! platform owner is told directly (backlog 5627a4e1).
//!
//! THE CASE. 2026-11-17: the last car gate launched 06:31Z, the last
//! train 07:20Z (#1809), the last agent-run opened 05:57Z, and nothing
//! launched again until David noticed at 14:45Z — about seven hourly
//! trains lost, with urgent builds and held-car reviews ready all night.
//!
//! DETECTION WAS NOT THE GAP; THE READER WAS. Measured the same day, two
//! alarms DID fire inside the stall: the unheld-review watch at 07:01Z
//! (fd20fb6c) and the silent-session watch at 08:01Z (a79f9097, "agent-
//! claude has written nothing … for 53m while holding 25"). Both were
//! filed as urgent backlog items owned by the platform owner and
//! ASSIGNED TO NOBODY, so `messages.notify` sent no one anything (a
//! role-only ready step pages nobody, by design), and the only reader of
//! an urgent backlog item is a coordinator session pulling the backlog —
//! the very session that had stopped. An alarm that reports through its
//! subject dies with it (CLAUDE.md §Diagnosis). So this watch does two
//! things the per-actor watches do not:
//!
//!   1. It judges the SYSTEM, not an actor: one fact — the newest car
//!      gate is older than the bound while declared ready work exists —
//!      that holds whichever sessions are up, down or never started.
//!   2. It TELLS the platform owner: a `direct` inbox message (the kind
//!      the inbox's needs-you filter is built on), sent by the
//!      dispatcher, which is a reader path that owes nothing to any agent
//!      session. The alarm packet stays in the backlog for whichever
//!      coordinator comes back.
//!
//! WHAT COUNTS AS A LAUNCH: a CAR's gate-run. A train gate is filed by
//! the conductor on its own clock, and main's full gate by its own
//! schedule; both run with every builder down (07:23Z and 07:57Z that
//! morning), so counting them would hold the alarm off while the factory
//! is stopped. The two predicates are boss-jobs' own
//! (`stranded::is_train_gate`, `full_gate_on_main::is_main_gate`), never
//! a second spelling here.
//!
//! WHAT COUNTS AS READY WORK: the rule's `ready` declaration, a list of
//! `<workflow>.<step>` pairs read off the queue-age lens (status ready,
//! real partition). The shipped row names the build and review steps an
//! agent takes; it is registry data, so a new workable step is a row
//! edit, not code.
//!
//! IT CARRIES THE CAUSE: the minutes idle and the gate it is measured
//! from, the ready count per declared step, the open agent-runs, and
//! every open work-session with when it last wrote `last_active_at` — or
//! that none is open, which is itself the answer to "why".
//!
//! DEDUP PER IDLE EPISODE: the episode IS the newest car gate's
//! `opened_at`, stamped on the alarm as `factory_idle_since`. One alarm
//! per episode whatever its status; the next car gate starts a new
//! episode — which, while an earlier episode's alarm is still OPEN, is
//! appended to that alarm as an occurrence rather than filed beside it
//! (`alarm_item`, backlog f4b0ad41). While the episode's alarm is OPEN
//! the message is re-sent
//! under its deterministic id, which the messages store collapses
//! (`ON CONFLICT (id) DO NOTHING`), so a send that failed after the
//! filing is repaired on the next tick and a successful one is never
//! doubled. The message is `common::tell_owner`'s, keyed on the episode
//! (backlog f53b2df3) — one shape for every alarm that tells its owner.
//!
//! IT CLOSES ITSELF WHEN THE FACTORY MOVES (backlog f53b2df3). A car
//! gate that launched after the open alarm's newest episode began ends
//! that episode, so on the next tick inside the bound the alarm is
//! withdrawn at the step `common::retraction` names, `stale`, with the
//! reason "a car gate launched at <t>: <gate-run>". Until then it
//! re-armed only when a person closed it, and an alarm that outlives its
//! condition is one the owner learns to ignore.
//!
//! It files, tells and withdraws; it routes nothing, releases nothing
//! and opens no run. Station pull is the fix for idle executors (David, 2026-11-17);
//! this is its backstop.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value as Json, json};

use super::alarm_item::{self, Occurrence, Raised};
use super::common::{
    RECOVERED_AT, Retraction, api_client, complete_step, get_json, jobs_where, jobs_where_slim,
    owner_for_filing, recovery_note, retraction, rows_or_refuse, tell_owner_keyed, with_lane,
    withdrawal_fields, write_json,
};

/// The handler's registered name.
pub const HANDLER: &str = "jobs.factory_idle_watch";

/// The alarm's dedup key: the newest car gate's `opened_at`, the
/// identity of one idle episode.
pub const EPISODE_KEY: &str = "factory_idle_since";

/// The `scope` every alarm this handler files carries.
pub const SCOPE: &str = "factory-idle";

/// How many of the newest gate-runs one tick reads. Trains depart
/// hourly and main's gate daily, so fifty rows hold a car gate unless
/// the factory has been stopped for about two days; a page with none
/// says so (`idle_is_a_floor`) rather than inventing a start.
pub const GATE_PAGE: usize = 50;

/// The key the schedule runner writes the firing instant under.
const TICK_AT: &str = "_at";

/// The rule's declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Bounds {
    pub idle_minutes: i64,
    /// `<workflow>.<step slug>` pairs whose READY steps are the work a
    /// stopped factory leaves waiting.
    pub ready: Vec<(String, String)>,
}

/// Parse the rule's args; a malformed one refuses by name — a watch
/// reading nothing is a watch that is not running.
pub fn bounds(args: &[(String, Value)]) -> Result<Bounds, String> {
    let idle_minutes = match arg(args, "idle_minutes") {
        Some(Value::Int(n)) if *n > 0 && Duration::try_minutes(*n).is_some() => *n,
        Some(other) => {
            return Err(format!(
                "`idle_minutes` must be a positive whole number, got a {} ({other:?})",
                other.kind()
            ));
        }
        None => return Err("`idle_minutes` is not declared on the rule".into()),
    };
    let ready = match arg(args, "ready") {
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once('.') {
                Some((k, s)) if !k.is_empty() && !s.is_empty() && !s.contains('.') => {
                    Ok((k.to_string(), s.to_string()))
                }
                _ => Err(format!(
                    "`ready` names `{p}`, which is not `<workflow>.<step>` — it would match no step"
                )),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(other) => {
            return Err(format!(
                "`ready` must be a comma-separated string of `<workflow>.<step>`, got a {} \
                 ({other:?})",
                other.kind()
            ));
        }
        None => return Err("`ready` is not declared on the rule".into()),
    };
    if ready.is_empty() {
        return Err("`ready` names no step — the watch could never see waiting work".into());
    }
    Ok(Bounds {
        idle_minutes,
        ready,
    })
}

fn text<'a>(v: &'a Json, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Json::as_str).unwrap_or("")
}

fn instant(v: &Json, pointer: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text(v, pointer))
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Is this gate-run a CAR's — neither a train's nor main's full gate?
pub fn is_car_gate(gate_run: &Json) -> bool {
    let md = gate_run.get("metadata").cloned().unwrap_or(Json::Null);
    !boss_jobs::stranded::is_train_gate(&md) && !boss_jobs::full_gate_on_main::is_main_gate(&md)
}

/// Where the idle episode starts, read off the newest page of gate-runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    /// The newest car gate's `opened_at`, verbatim — the dedup key.
    pub since: String,
    pub since_at: DateTime<Utc>,
    /// The car gate it is measured from, when the page held one.
    pub gate: Option<(String, String)>,
    /// True when the page held no car gate: `since` is then the oldest
    /// row's launch, a lower bound on the idle time and not its start.
    pub floor: bool,
}

/// The episode a page of gate-runs (newest first, any order tolerated)
/// describes. `None` only for a page with no dated row at all.
pub fn episode(gate_runs: &[Json]) -> Option<Episode> {
    let dated = |r: &&Json| instant(r, "/opened_at").is_some();
    if let Some(car) = gate_runs
        .iter()
        .filter(dated)
        .filter(|r| is_car_gate(r))
        .max_by_key(|r| instant(r, "/opened_at"))
    {
        return Some(Episode {
            since: text(car, "/opened_at").to_string(),
            since_at: instant(car, "/opened_at")?,
            gate: Some((
                text(car, "/id").to_string(),
                text(car, "/title").to_string(),
            )),
            floor: false,
        });
    }
    let oldest = gate_runs
        .iter()
        .filter(dated)
        .min_by_key(|r| instant(r, "/opened_at"))?;
    Some(Episode {
        since: text(oldest, "/opened_at").to_string(),
        since_at: instant(oldest, "/opened_at")?,
        gate: None,
        floor: true,
    })
}

/// READY steps of the declared pairs on real packets, counted per pair
/// in declaration order (pairs with none are left out).
pub fn ready_work(lens: &[Json], b: &Bounds) -> Vec<(String, usize)> {
    b.ready
        .iter()
        .map(|(kind, slug)| {
            let n = lens
                .iter()
                .filter(|r| {
                    text(r, "/status") == "ready"
                        && text(r, "/partition") == "real"
                        && text(r, "/job_kind") == kind
                        && text(r, "/spec_slug") == slug
                })
                .count();
            (format!("{kind}.{slug}"), n)
        })
        .filter(|(_, n)| *n > 0)
        .collect()
}

/// One open work-session, as the alarm names it.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: String,
    pub actor: String,
    pub last_active_at: Option<String>,
}

pub fn sessions(rows: &[Json]) -> Vec<Session> {
    rows.iter()
        .map(|r| Session {
            id: text(r, "/id").to_string(),
            actor: text(r, "/metadata/actor").to_string(),
            last_active_at: r
                .pointer("/metadata/last_active_at")
                .and_then(Json::as_str)
                .map(str::to_string),
        })
        .collect()
}

/// Everything one alarm says.
#[derive(Debug, Clone)]
pub struct Idle {
    pub episode: Episode,
    pub idle_minutes: i64,
    pub bound_minutes: i64,
    pub ready: Vec<(String, usize)>,
    pub open_runs: usize,
    pub sessions: Vec<Session>,
}

impl Idle {
    fn ready_total(&self) -> usize {
        self.ready.iter().map(|(_, n)| n).sum()
    }

    fn idle_words(&self) -> String {
        if self.episode.floor {
            format!("at least {}", self.idle_minutes)
        } else {
            self.idle_minutes.to_string()
        }
    }

    fn sessions_words(&self) -> String {
        if self.sessions.is_empty() {
            return "NO work-session is open — nothing is running the factory".into();
        }
        self.sessions
            .iter()
            .map(|s| {
                format!(
                    "{} ({}) last active {}",
                    s.actor,
                    s.id,
                    s.last_active_at.as_deref().unwrap_or("never stamped")
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The alarm one idle episode files.
pub fn alarm_body(i: &Idle, owner: &str, now: DateTime<Utc>, rule: &str) -> Json {
    let ready = i
        .ready
        .iter()
        .map(|(k, n)| format!("{n} {k}"))
        .collect::<Vec<_>>()
        .join(", ");
    let from = match &i.episode.gate {
        Some((id, title)) => format!(
            "the newest car gate, {title} ({id}), launched {}",
            i.episode.since
        ),
        None => format!(
            "no car gate is among the newest {GATE_PAGE} gate-runs, the oldest of which launched \
             {} — so the idle time is a floor",
            i.episode.since
        ),
    };
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog 5627a4e1) at {now}: no car gate has launched \
         for {idle} minute(s) — {from} — while {total} ready step(s) of the declared work wait \
         ({ready}). Open agent-runs: {runs}. Work-sessions: {sessions}. The factory is not \
         building. Start a session that pulls the queue, or dispatch a ready build \
         (`boss dispatch --next`). This alarm files once per idle episode (keyed on {EPISODE_KEY}) \
         and tells the platform owner directly, because an urgent backlog item is otherwise read \
         only by the coordinator that has stopped. It routes nothing.",
        now = now.to_rfc3339(),
        idle = i.idle_words(),
        total = i.ready_total(),
        runs = i.open_runs,
        sessions = i.sessions_words(),
    );
    let metadata = json!({
        "area": "agents",
        "scope": SCOPE,
        EPISODE_KEY: i.episode.since,
        "idle_is_a_floor": i.episode.floor,
        "idle_minutes": i.idle_minutes,
        "bound_minutes": i.bound_minutes,
        "last_car_gate": i.episode.gate.as_ref().map(|(id, title)| json!({"id": id, "title": title})),
        "ready_steps": i.ready_total(),
        "ready_by_step": i.ready.iter().map(|(k, n)| (k.clone(), json!(n))).collect::<serde_json::Map<_, _>>(),
        "open_agent_runs": i.open_runs,
        "work_sessions": i.sessions.iter().map(|s| json!({
            "id": s.id,
            "actor": s.actor,
            "last_active_at": s.last_active_at,
        })).collect::<Vec<_>>(),
        "told": owner,
        "measured_at": now.to_rfc3339(),
        "detail": detail,
    });
    json!({
        "kind": "backlog-item",
        "title": format!(
            "FACTORY IDLE: no car gate for {}m while {} ready build/review step(s) wait — {} open \
             work-session(s)",
            i.idle_words(),
            i.ready_total(),
            i.sessions.len(),
        ),
        "subject": {"subject_kind": "custom", "id": "factory"},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

/// When the open alarm's NEWEST episode began: the episode `alarm_item`
/// last appended, else the one it was filed for. `None` for a stamp
/// that does not parse — an alarm whose start cannot be read is not
/// closed on a guess.
pub fn episode_began(alarm: &Json) -> Option<DateTime<Utc>> {
    let last = text(alarm, &format!("/metadata/{}", alarm_item::LAST_EPISODE));
    let stamp = if last.is_empty() {
        text(alarm, &format!("/metadata/{EPISODE_KEY}"))
    } else {
        last
    };
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Why an open alarm is over, when it is: the newest car gate (`ep`,
/// never a floor) launched AFTER its episode began (backlog f53b2df3).
/// The words are the recorded reason.
pub fn ended_by(alarm: &Json, ep: &Episode) -> Option<String> {
    let (id, title) = ep.gate.as_ref()?;
    (ep.since_at > episode_began(alarm)?)
        .then(|| format!("a car gate launched at {}: {title} ({id})", ep.since))
}

pub struct JobsFactoryIdleWatch {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    messages_base: String,
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsFactoryIdleWatch {
    pub fn new(
        jobs_base: impl Into<String>,
        messages_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            messages_base: messages_base.into(),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// The owner's message, through the one definition every alarm
    /// watch sends by (`common::tell_owner`, backlog 35dabc3d), keyed on
    /// the alarm's episode so each appended episode tells once.
    async fn tell(
        &self,
        alarm: &Json,
        alarm_id: &str,
        owner: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        tell_owner_keyed(
            &self.client,
            &self.messages_base,
            SCOPE,
            text(alarm, &format!("/metadata/{EPISODE_KEY}")),
            alarm_id,
            alarm,
            owner,
            rule,
        )
        .await
    }

    /// Close every open alarm a car gate launched after (backlog
    /// f53b2df3), at the step `common::retraction` names. Plain
    /// retraction, as the silent-session watch's `withdraw`: the recovery
    /// hold (car 0a3db9da) had not landed when this was written, and can
    /// adopt this call site as it adopts that one.
    async fn close_ended(
        &self,
        ep: &Episode,
        now: DateTime<Utc>,
        rule: &str,
    ) -> Result<(), HandlerError> {
        if ep.gate.is_none() {
            return Ok(());
        }
        let open = jobs_where(
            &self.client,
            self.base(),
            &format!("kind=backlog-item&status=open&metadata_has={EPISODE_KEY}"),
            rule,
        )
        .await?;
        for alarm in open.iter().filter(|r| alarm_item::is_open(r)) {
            let Some(evidence) = ended_by(alarm, ep) else {
                continue;
            };
            let id = text(alarm, "/id");
            match retraction(alarm) {
                None => {
                    tracing::warn!(rule, packet = %id, "{HANDLER}: the open alarm has no triage step to close");
                }
                Some(Retraction::Complete { step_id, .. }) => {
                    complete_step(
                        &self.client,
                        self.base(),
                        id,
                        &step_id,
                        withdrawal_fields(&evidence, HANDLER, &now.to_rfc3339()),
                        rule,
                    )
                    .await?;
                    tracing::info!(rule, packet = %id, %evidence, "{HANDLER}: the factory moved — alarm closed");
                }
                // A packet already told is not told again on every tick.
                Some(Retraction::Annotate { .. })
                    if alarm
                        .pointer(&format!("/metadata/{RECOVERED_AT}"))
                        .is_some_and(|v| !v.is_null()) => {}
                Some(Retraction::Annotate { why_open }) => {
                    write_json(
                        &self.client,
                        reqwest::Method::PATCH,
                        &format!("{}/api/jobs/{id}/metadata", self.base()),
                        &Json::Object(recovery_note(
                            &evidence,
                            HANDLER,
                            &now.to_rfc3339(),
                            &why_open,
                        )),
                        rule,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Handler for JobsFactoryIdleWatch {
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
        let now = instant(&ctx.event_payload, &format!("/{TICK_AT}")).ok_or_else(|| {
            HandlerError::Permanent(format!(
                "the firing carries no `{TICK_AT}` — {HANDLER} judges minutes and needs a \
                 sub-day cadence (every-<n>-minutes)"
            ))
        })?;

        let path = format!("/api/jobs?kind=gate-run&order=newest&full=false&limit={GATE_PAGE}");
        let page = get_json(&self.client, &format!("{}{path}", self.base()), rule).await?;
        let gates: Vec<Json> =
            rows_or_refuse(&page, &format!("GET {path}")).map_err(HandlerError::Downstream)?;
        let Some(ep) = episode(&gates) else {
            // An instance with no gate lane has no factory to watch; a
            // page of rows none of which is dated is a broken read.
            if gates.is_empty() {
                tracing::info!(
                    rule,
                    "{HANDLER}: no gate-run on record — no factory to watch"
                );
                return Ok(());
            }
            return Err(HandlerError::Downstream(format!(
                "GET {path} answered {} gate-run(s) and none carries an `opened_at` — refusing \
                 rather than reading the factory as never idle",
                gates.len()
            )));
        };
        let idle_minutes = (now - ep.since_at).num_minutes();
        if idle_minutes < b.idle_minutes {
            tracing::info!(
                rule,
                idle_minutes,
                "{HANDLER}: a car gate launched inside the bound"
            );
            // The factory moved: an alarm whose episode that gate ended
            // closes itself rather than waiting for a person to.
            return self.close_ended(&ep, now, rule).await;
        }

        let lens_path = "/api/jobs/queue-age";
        let lens = get_json(&self.client, &format!("{}{lens_path}", self.base()), rule).await?;
        let rows: Vec<Json> =
            rows_or_refuse(&lens, &format!("GET {lens_path}")).map_err(HandlerError::Downstream)?;
        let ready = ready_work(&rows, &b);
        if ready.is_empty() {
            tracing::info!(
                rule,
                idle_minutes,
                "{HANDLER}: idle, but no declared work is ready"
            );
            return Ok(());
        }

        let owner = owner_for_filing(self.owner.as_ref(), rule).await;
        let filed = jobs_where_slim(
            &self.client,
            self.base(),
            &format!("kind=backlog-item&metadata_has={EPISODE_KEY}"),
            rule,
        )
        .await?;
        let open = |r: &&Json| text(r, "/status") == "open";
        let this_episode = filed
            .iter()
            .find(|r| text(r, &format!("/metadata/{EPISODE_KEY}")) == ep.since);
        // A floor's start slides with the page, so a floor episode is
        // held by ANY open alarm of this watch, not by its key alone.
        let held = this_episode.or_else(|| ep.floor.then(|| filed.iter().find(open)).flatten());
        if let Some(alarm) = held {
            if open(&alarm) {
                // Repairs a send that failed after the filing; the
                // deterministic id collapses a send that did not.
                self.tell(alarm, text(alarm, "/id"), text(alarm, "/owner_id"), rule)
                    .await?;
            }
            tracing::info!(rule, episode = %ep.since, "{HANDLER}: this idle episode is already alarmed");
            return Ok(());
        }

        let open_runs = jobs_where_slim(
            &self.client,
            self.base(),
            "kind=agent-run&status=open",
            rule,
        )
        .await?
        .len();
        let open_sessions = sessions(
            &jobs_where_slim(
                &self.client,
                self.base(),
                "kind=work-session&status=open",
                rule,
            )
            .await?,
        );
        let idle = Idle {
            episode: ep,
            idle_minutes,
            bound_minutes: b.idle_minutes,
            ready,
            open_runs,
            sessions: open_sessions,
        };
        // A NEW episode while an earlier episode's alarm is still open is
        // an occurrence on that alarm, not a second open item about one
        // factory (backlog f4b0ad41): appended with the episode, and the
        // owner told under the new episode's message id. The stricter
        // rule above stands — an episode alarmed once, open or closed,
        // is never alarmed again.
        let alarm = alarm_body(&idle, &owner, now, rule);
        let occ = Occurrence::new(
            now.to_rfc3339(),
            format!(
                "idle {idle_minutes} minutes since the car gate of {}; {} step(s) ready",
                idle.episode.since,
                idle.ready_total()
            ),
        )
        .in_episode(idle.episode.since.as_str());
        let open_alarm = filed.iter().find(|r| alarm_item::is_open(r));
        let (id, told_to) = match alarm_item::raise_minted(
            &self.client,
            self.base(),
            open_alarm,
            alarm.clone(),
            &occ,
            Default::default(),
            rule,
        )
        .await?
        {
            Raised::Filed(id) => (id.unwrap_or_default(), owner.clone()),
            Raised::Appended(id) | Raised::Held(id) => {
                let to = open_alarm.map(|a| text(a, "/owner_id")).unwrap_or_default();
                (id, to.to_string())
            }
        };
        self.tell(&alarm, &id, &told_to, rule).await?;
        let by_step: BTreeMap<&str, usize> =
            idle.ready.iter().map(|(k, n)| (k.as_str(), *n)).collect();
        tracing::warn!(rule, alarm = %id, idle_minutes, ?by_step, "{HANDLER}: the factory is idle — alarm filed or appended");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    const NOW: &str = "2026-11-17T08:20:00Z";
    const RULE: &str = "a-factory-idle-past-an-hour-alarms-the-owner";
    const GATES: &str = "/api/jobs?kind=gate-run&order=newest";
    const LENS: &str = "/api/jobs/queue-age";
    const ALARMS: &str = "/api/jobs?kind=backlog-item&metadata_has=factory_idle_since";
    const RUNS: &str = "/api/jobs?kind=agent-run&status=open";
    const SESSIONS: &str = "/api/jobs?kind=work-session&status=open";

    fn args() -> Vec<(String, Value)> {
        vec![
            ("idle_minutes".into(), Value::Int(60)),
            (
                "ready".into(),
                Value::String("backlog-item.build,ship-a-change.landed-review".into()),
            ),
        ]
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: RULE.into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-11-17", "_at": NOW}),
        }
    }

    fn gate(id: &str, opened_at: &str, marker: Option<&str>) -> Json {
        let mut md = json!({"branch": format!("fix/{id}")});
        if let Some(k) = marker {
            md[k] = json!(true);
        }
        json!({"id": id, "title": format!("Gate: fix/{id}"), "opened_at": opened_at, "metadata": md, "status": "closed"})
    }

    /// The measured morning: a car gate 06:31Z, then a train gate 07:23Z
    /// and main's full gate 07:57Z, which ran with every builder down.
    fn measured_gates() -> Json {
        json!({"data": [
            gate(
                "main",
                "2026-11-17T07:57:22Z",
                Some(boss_jobs::full_gate_on_main::MARK),
            ),
            gate("train", "2026-11-17T07:23:30Z", Some("train_gate")),
            gate("car", "2026-11-17T06:31:25Z", None),
        ], "total": 3})
    }

    fn step(kind: &str, slug: &str, status: &str, partition: &str) -> Json {
        json!({"job_kind": kind, "spec_slug": slug, "status": status, "partition": partition})
    }

    fn measured_lens() -> Json {
        json!({"data": [
            step("backlog-item", "build", "ready", "real"),
            step("backlog-item", "build", "ready", "real"),
            step("backlog-item", "build", "active", "real"),
            step("backlog-item", "build", "ready", "simulated"),
            step("backlog-item", "triage", "ready", "real"),
            step("ship-a-change", "landed-review", "ready", "real"),
        ], "total": 6})
    }

    fn session_rows() -> Json {
        json!({"data": [
            {"id": "s-codex", "status": "open", "metadata": {"actor": "agent-codex"}},
            {"id": "s-claude", "status": "open", "metadata": {"actor": "claude@algedonic.dev", "last_active_at": "2026-11-17T07:06:26Z"}},
        ], "total": 2})
    }

    fn live(alarms: Json) -> Vec<(&'static str, Json)> {
        vec![
            (GATES, measured_gates()),
            (LENS, measured_lens()),
            (ALARMS, alarms),
            (RUNS, listing_stub::empty_listing()),
            (SESSIONS, session_rows()),
        ]
    }

    fn handler(stub: &listing_stub::Stub) -> Arc<JobsFactoryIdleWatch> {
        JobsFactoryIdleWatch::new(
            stub.base.clone(),
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    #[test]
    fn the_declaration_is_a_positive_bound_and_workflow_step_pairs() {
        let b = bounds(&args()).unwrap();
        assert_eq!(b.idle_minutes, 60);
        assert_eq!(b.ready[0], ("backlog-item".into(), "build".into()));
        let no_bound = vec![args()[1].clone()];
        assert!(bounds(&no_bound).unwrap_err().contains("idle_minutes"));
        let bad = vec![
            args()[0].clone(),
            ("ready".into(), Value::String("build".into())),
        ];
        assert!(bounds(&bad).unwrap_err().contains("`build`"));
        let none = vec![
            args()[0].clone(),
            ("ready".into(), Value::String(" , ".into())),
        ];
        assert!(bounds(&none).unwrap_err().contains("names no step"));
    }

    /// Train and main gates are not the factory launching anything.
    #[test]
    fn the_episode_starts_at_the_newest_car_gate() {
        let rows = rows_or_refuse::<Json>(&measured_gates(), "t").unwrap();
        let ep = episode(&rows).unwrap();
        assert_eq!(ep.since, "2026-11-17T06:31:25Z");
        assert!(!ep.floor);
        assert_eq!(ep.gate.unwrap().0, "car");

        let only_trains = vec![rows[0].clone(), rows[1].clone()];
        let floor = episode(&only_trains).unwrap();
        assert!(
            floor.floor,
            "no car gate on the page is a floor, not a start"
        );
        assert_eq!(floor.since, "2026-11-17T07:23:30Z");
        assert_eq!(episode(&[]), None);
    }

    #[test]
    fn ready_work_counts_only_ready_real_steps_of_the_declared_pairs() {
        let rows = rows_or_refuse::<Json>(&measured_lens(), "t").unwrap();
        let ready = ready_work(&rows, &bounds(&args()).unwrap());
        assert_eq!(
            ready,
            vec![
                ("backlog-item.build".to_string(), 2),
                ("ship-a-change.landed-review".to_string(), 1)
            ]
        );
    }

    /// The measured stall: one alarm naming the minutes, the work and
    /// the sessions, and one direct message to the platform owner.
    #[tokio::test]
    async fn the_measured_stall_files_one_alarm_and_tells_the_owner() {
        let stub = listing_stub::serve(live(listing_stub::empty_listing())).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "one alarm and one message: {sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        let alarm = &sent[0].1;
        assert_eq!(alarm["priority"], "urgent");
        assert_eq!(alarm["owner_id"], "emp-owner");
        let m = &alarm["metadata"];
        assert_eq!(m[EPISODE_KEY], "2026-11-17T06:31:25Z");
        assert_eq!(m["idle_minutes"], 108);
        assert_eq!(m["ready_steps"], 3);
        assert_eq!(m["work_sessions"].as_array().unwrap().len(), 2);
        let detail = m["detail"].as_str().unwrap();
        assert!(detail.contains("108 minute(s)"), "{detail}");
        assert!(detail.contains("2 backlog-item.build"), "{detail}");
        assert!(
            detail.contains("agent-codex (s-codex) last active never stamped"),
            "{detail}"
        );
        assert!(
            detail.contains("claude@algedonic.dev (s-claude) last active 2026-11-17T07:06:26Z"),
            "{detail}"
        );
        assert_eq!(sent[1].0, "POST /api/messages/send");
        let msg = &sent[1].1;
        assert_eq!(msg["recipient_id"], "emp-owner");
        assert_eq!(msg["kind"], "direct");
        assert_eq!(msg["id"], "factory-idle:2026-11-17T06:31:25Z:emp-owner");
        assert_eq!(msg["entity_ref"]["entity_path"], "/jobs/stub-created");
    }

    #[tokio::test]
    async fn a_car_gate_inside_the_bound_is_silent() {
        let mut answers = live(listing_stub::empty_listing());
        answers[0] = (
            GATES,
            json!({"data": [gate("car", "2026-11-17T07:30:00Z", None)], "total": 1}),
        );
        let stub = listing_stub::serve(answers).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// An open alarm whose episode began at `episode`, its triage open.
    fn open_alarm(episode: &str) -> Json {
        json!({"id": "a1", "status": "open", "owner_id": "emp-owner", "title": "FACTORY IDLE",
            "metadata": {EPISODE_KEY: episode, alarm_item::LAST_EPISODE: episode},
            "steps": [{"id": "t1", "spec_slug": "triage", "status": "ready"}]})
    }

    fn a_car_gate_at_0730(alarms: Json) -> Vec<(&'static str, Json)> {
        let mut answers = live(alarms);
        answers[0] = (
            GATES,
            json!({"data": [gate("car", "2026-11-17T07:30:00Z", None)], "total": 1}),
        );
        answers
    }

    /// THE FACTORY MOVED (backlog f53b2df3): a car gate launched after
    /// the open alarm's episode began, so the alarm withdraws itself at
    /// its open step, `stale`, naming the gate that ended the episode.
    #[tokio::test]
    async fn a_car_gate_launched_after_the_episode_closes_the_open_alarm() {
        let alarms = json!({"data": [open_alarm("2026-11-17T06:31:25Z")], "total": 1});
        let stub = listing_stub::serve(a_car_gate_at_0730(alarms)).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/a1/steps/t1/metadata");
        assert_eq!(sent[0].1["disposition"], "stale");
        assert_eq!(sent[0].1["cleared_by"], HANDLER);
        let evidence = sent[0].1["evidence"].as_str().unwrap();
        assert!(
            evidence.contains("a car gate launched at 2026-11-17T07:30:00Z: Gate: fix/car (car)"),
            "{evidence}"
        );
        assert_eq!(sent[1].0, "PUT /api/jobs/a1/steps/t1");
        assert_eq!(sent[1].1["status"], "completed");
    }

    /// The control: the newest car gate is OLDER than the open alarm's
    /// episode, so nothing launched since it began and the alarm stands.
    #[tokio::test]
    async fn a_car_gate_launched_before_the_episode_leaves_the_alarm_open() {
        let alarms = json!({"data": [open_alarm("2026-11-17T07:40:00Z")], "total": 1});
        let stub = listing_stub::serve(a_car_gate_at_0730(alarms)).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    #[tokio::test]
    async fn idle_with_no_declared_work_ready_is_silent() {
        let mut answers = live(listing_stub::empty_listing());
        answers[1] = (
            LENS,
            json!({"data": [step("backlog-item", "triage", "ready", "real")], "total": 1}),
        );
        let stub = listing_stub::serve(answers).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// Dedup per episode: an alarm for this episode, closed, holds it;
    /// an open one only re-sends the message under its collapsing id.
    #[tokio::test]
    async fn one_alarm_per_episode_and_an_open_one_only_resends_its_message() {
        let closed = json!({"id": "a1", "status": "closed", "owner_id": "emp-owner",
            "metadata": {EPISODE_KEY: "2026-11-17T06:31:25Z"}});
        let stub = listing_stub::serve(live(json!({"data": [closed], "total": 1}))).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());

        let open = json!({"id": "a1", "status": "open", "owner_id": "emp-owner", "title": "FACTORY IDLE",
            "metadata": {EPISODE_KEY: "2026-11-17T06:31:25Z", "detail": "d"}});
        let stub = listing_stub::serve(live(json!({"data": [open], "total": 1}))).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/messages/send");
        assert_eq!(
            sent[0].1["id"],
            "factory-idle:2026-11-17T06:31:25Z:emp-owner"
        );
        assert_eq!(sent[0].1["entity_ref"]["entity_id"], "a1");
    }

    /// AN OPEN ALARM UPDATES ITS ITEM (backlog f4b0ad41). A new episode
    /// while the previous episode's alarm is still OPEN is appended to
    /// it — one open item about one factory — and the owner is told under
    /// the new episode's message id. Once that alarm is CLOSED, the next
    /// episode files a new one.
    #[tokio::test]
    async fn a_new_episode_appends_to_an_open_alarm_and_files_after_a_close() {
        let old = json!({"id": "a0", "status": "open", "owner_id": "emp-owner",
            "metadata": {EPISODE_KEY: "2026-11-16T20:00:00Z"}});
        let stub = listing_stub::serve(live(json!({"data": [old.clone()], "total": 1}))).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/a0/metadata");
        assert_eq!(sent[0].1[alarm_item::OCCURRENCES], 2);
        assert_eq!(sent[0].1[alarm_item::LAST_EPISODE], "2026-11-17T06:31:25Z");
        assert_eq!(sent[1].0, "POST /api/messages/send");
        assert_eq!(
            sent[1].1["id"],
            "factory-idle:2026-11-17T06:31:25Z:emp-owner"
        );
        assert_eq!(sent[1].1["entity_ref"]["entity_id"], "a0");

        let mut closed = old;
        closed["status"] = json!("closed");
        let stub = listing_stub::serve(live(json!({"data": [closed], "total": 1}))).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert_eq!(stub.sent()[0].0, "POST /api/jobs");
    }

    #[tokio::test]
    async fn a_gate_page_with_no_data_array_refuses() {
        let mut answers = live(listing_stub::empty_listing());
        answers[0] = (GATES, listing_stub::no_data_array());
        let stub = listing_stub::serve(answers).await;
        listing_stub::assert_refused_by_name(
            handler(&stub).invoke(&args(), &ctx()).await,
            "kind=gate-run",
        );
        assert!(stub.writes().is_empty());
    }

    /// The shipped row: the bound is an hour and the declared work names
    /// only steps an agent takes.
    #[test]
    fn the_shipped_rule_declares_an_hour_and_agent_work() {
        let raw =
            boss_dispatcher::rules::registry::parse_raw_path(boss_testing::dispatcher_rules_dir())
                .expect("parse the shipped rule registry");
        let rule = raw
            .rules
            .iter()
            .find(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
            .expect("a shipped rule invokes this handler");
        assert_eq!(rule.name, RULE);
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
        let b = bounds(&args).unwrap_or_else(|e| panic!("{}: {e}", rule.name));
        assert_eq!(b.idle_minutes, 60);
        assert!(b.ready.contains(&("backlog-item".into(), "build".into())));
        let cadence = rule.schedule.as_ref().map(|s| s.cadence);
        assert!(
            matches!(
                cadence,
                Some(boss_dispatcher::rules::registry::Cadence::EveryNMinutes(n)) if n <= 15
            ),
            "{}: {cadence:?} — the bound is minutes, so the tick must be",
            rule.name
        );
    }
}
