//! `jobs.silent_session_watch` — an actor that holds claimed work and
//! whose open work-session has had no write of its own for a bound is
//! an alarm that names the actor, the silence and the last note it
//! wrote (backlog 628dc540, David's request, 2026-10-28).
//!
//! THE CASE. Measured 2026-10-28: agent-codex, the factory, hit a
//! temporary model-capacity limit at about 06:42Z (its last note on
//! work-session d43a79c5) and nothing restarted it until David did at
//! about 19:20Z — the factory down about 12.5 h. It stalled again from
//! about 20:57Z to 22:38Z. Nothing noticed either time: the factory ran
//! as an interactive TUI that waits at an error prompt and never exits;
//! `agent-run-dies-when-building-is-silent` judges a run only after 4 h
//! without a completed step, and codex's builds are mostly not runs;
//! `an-idle-actor-holding-urgent-nominations-is-alarmed` fires only for
//! an actor with NO open run and READY urgent work, and a stalled
//! factory holds ACTIVE claims. The conductor's detection was by hand.
//!
//! WHY NO NEIGHBOUR COVERED IT, AND WHY NO ARG COULD MAKE ONE. The
//! idle-actor watch reads the opposite case — an actor that has claimed
//! nothing — and skips every actor with an open run, which is the case
//! here. The session's own died rule (`work-session-ends-when-silent`)
//! reads `last_active_at`, the Claude prompt hook's heartbeat, which a
//! codex session never writes, and ends the session at 6 h rather than
//! alarming. Neither reads WHO wrote to the session, which is the
//! measurement this needs.
//!
//! THE CONDITION, per registered agent (every spelling the agents
//! registry ties to it) — the ACTOR is watched, not one session
//! (review 97bb7698 finding 1, decided 2026-10-29 on 628dc540):
//!   - it HOLDS work: an open agent-run naming it, or an ACTIVE step
//!     assigned to it on a packet that is not a work-session or a run;
//!   - its sessions are read: every OPEN work-session, or, when none is
//!     open, its NEWEST work-session of any status. A holder with no open
//!     session is itself the case: `work-session-ends-when-silent` ends a
//!     session six hours after its last `last_active_at`, and measured on
//!     2026-10-28 it ended the factory's d43a79c5 at 11:00:45Z, 6 h 14 min
//!     after its last heartbeat, while it held claimed work. Before, no
//!     open session was no reading and a Hold, so from then on a stall
//!     filed nothing. A holder with no session at all is an alarm at once;
//!   - no write SIGNED BY IT has landed on those sessions for the bound —
//!     read off each packet's own slice of the audit log
//!     (`GET /api/jobs/{id}/events`), whose `actor` is the `_actor` the
//!     server stamped. A note the CONDUCTOR writes onto the factory's
//!     session (an order) is somebody else's write and does not count:
//!     on 2026-10-28 the conductor wrote six orders onto 243c9e91 while
//!     the factory said nothing;
//!   - a write that changes ANY key under a declared `ignore_key_prefixes`
//!     prefix is the SUPERVISOR's and does not count either. The shipped
//!     rule names `factory_restart_`, the key infra/dev/factory-loop.sh
//!     writes when it relaunches the CLI after a failed turn, and
//!     `factory_turn`, which rides with the `last_active_at` the loop
//!     stamps at every turn start and end so the age-out never closes a
//!     live factory. A supervisor that is alive while the model is not is
//!     still a factory that builds nothing, and its writes (a turn start
//!     at least every 30 min while it backs off a limit) would otherwise
//!     hold this alarm off forever. The Claude prompt hook's heartbeat
//!     (`last_active_at`, `prompt_count`) carries no such key: a session
//!     being prompted is the actor speaking;
//!   - it is not MOVING (finding 2): no step of its agent-runs was
//!     completed BY it inside the bound — a completion signed by
//!     automation (the run-died rule, a train landing, a marker) is not
//!     the actor moving (review 1c7ab966 finding 2) — and no gate-run one
//!     of its open runs launched has moved its branch inside the bound;
//!   - a BOUND WORKER DEFERS, it does not hold (review 1c7ab966 finding
//!     1, decided_2026_10_29_factory_watch_repair_2): an open run of it
//!     with its declared worker receipt recorded (`worker_started`, the
//!     receipt `a-declared-worker-receipt-is-observed-every-five-minutes`
//!     observes against its 15-minute expectation) beside its binding
//!     (`worker_session`) and not retired (`worker_retirement`) stretches
//!     the bound to TWICE `silent_minutes` from the actor's own last
//!     write, never longer — nothing observes a worker after its receipt,
//!     so a bound worker is not proof of a live one. Measured: run
//!     909a90a9 built this car for 59 minutes before its gate with no
//!     step completing, which the deferral covers; and run 320b62e1 held
//!     a bound worker from 06:31:29Z until it died at 11:00:25Z while the
//!     factory, stopped at a model limit since 06:42:08Z, built nothing,
//!     which it no longer covers: that stall files at the 08:15Z tick. A
//!     restart note (`restart_key_prefixes`) newer than the actor's last
//!     own write removes the deferral: the supervisor is saying the CLI
//!     itself is down. The actor's last own write is the newer of its
//!     session reading and its own last run completion (backlog
//!     7de4cc46): an actor with no open session has no session write that
//!     can move, and measured on 2026-11-12 the deferral ran from a
//!     day-old session floor and filed 48 minutes after codex briefed the
//!     run it was building;
//!   - no alarm of this rule for it is OPEN.
//!
//! THE WINDOW. The newest [`EVENT_WINDOW`] events of each session are
//! read (a codex session's event carries its whole metadata, about
//! 32 KB on 2026-10-28, so the whole history is not read every five
//! minutes). When the window holds no counted write of the actor, the
//! silence is AT LEAST back to the window's oldest event — the bound
//! below is judged on that floor, so a full window of other actors'
//! writes can delay the alarm but never fire it falsely. When the window
//! is FULL, its oldest event has no predecessor in it, so what that
//! event changed cannot be read: it is UNKNOWN (finding 3). It sets the
//! floor and nothing else — never the actor speaking, never a
//! withdrawal. Before, an actor-signed restart note in that place read
//! as the factory writing again, withdrew the open alarm, and the next
//! tick filed a new one: an urgent packet about every 30 minutes.
//!
//! IT CLOSES ITSELF. An open alarm is withdrawn
//! (`common::retraction`, stamped `cleared_by` this handler) when a
//! counted write of the actor lands AFTER the instant the alarm
//! recorded as `silent_since` — the factory spoke again — or when the
//! actor is MOVING after that instant, by the same read that keeps the
//! watch from filing: it completed a step of one of its own runs, or a
//! gate-run of an open run of it moved its branch, recently enough that
//! this tick would not file (backlog 7de4cc46). Measured 2026-11-12:
//! agent-codex had no open session, so its ended one's window of other
//! actors' peer messages read as a floor; the alarm filed at 15:55Z,
//! three minutes before codex completed its own run's `building`, and
//! stood open through 24 landed runs, because only a session write could
//! withdraw it and codex had no session to write to. The withdrawal
//! names what moved and when. Nothing else closes it: claims released by
//! the conductor's failover do not mean the factory came back.
//!
//! IT IS HELD OPEN FIRST (backlog f4b0ad41, THE HOLD — decided by David
//! 2026-11-17). The withdrawal above does not close the alarm: it writes
//! the RECOVERED note (`common::hold_note`) and the alarm stays open for
//! the rule's `hold_days` window (seven by default). A silence inside
//! the window is a RELAPSE: the tick judges as if no alarm were open,
//! and what it would file is appended to this alarm through
//! `alarm_item`, which withdraws the note. A window that passes with no
//! relapse withdraws the alarm at the step `common::retraction` names,
//! stamped `cleared_by` this handler; that close re-arms the watch, so
//! the next silence files again. Measured by run 3f4ed839: closing on
//! the clear had filed 62 silent-session items the hold would have
//! appended to an open one.
//!
//! IT DOES NOT REPORT THROUGH ITS SUBJECT. The alarm is owned by the
//! platform owner and assigned to nobody — routing it to the actor that
//! stopped would be an alarm only the silent can read — and the owner is
//! TOLD by a `direct` inbox message beside the filing, re-sent under the
//! same collapsing id while the alarm holds (`common::tell_owner`,
//! backlog 35dabc3d: a79f9097 fired at 08:01Z on 2026-11-17, assigned to
//! nobody, and its only reader was the session that had stopped). It
//! files and withdraws, and never routes: releasing the silent factory's claims is
//! the conductor's act on a read of the alarm (the failover half of
//! 628dc540's decision). Its text sends the conductor to check worker
//! liveness and branch movement FIRST and to release only a run whose
//! worker is gone, because the first version said "carry the urgent
//! builds it holds (`boss step release` …)", which on a false alarm frees
//! a healthy factory's claim and puts two builders on one car. For the
//! factory PROCESS it names one door, `boss factory restart` (backlog
//! ccd985ef): on 2026-10-29 this alarm fired at 06:10Z on an interactive
//! factory idle at its prompt since about 04:01Z, and nothing acted on it
//! for about 8 h, because the restart loop covered only option 6 and the
//! conductor had no verb to wake or start the factory.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value as Json, json};

use super::alarm_item::{self, Occurrence, Raised};
use super::common::{
    Hold, api_client, complete_step, empty_roster_refusal, get_json, held_evidence, hold,
    hold_end_note, hold_note, hold_priority, hold_window, jobs_where, jobs_where_slim,
    owner_for_filing, recovery_note, retraction, rows_or_refuse, tell_owner, told_recovered,
    with_lane, with_note, withdrawal_fields, write_json,
};
use super::jobs_idle_actor_watch::{Actor, actors, containment, own_rows, query};

/// The handler's registered name.
pub const HANDLER: &str = "jobs.silent_session_watch";

/// The alarm's dedup key: the agent id it names, read with
/// `metadata_has` over OPEN backlog items only, so a closed alarm
/// re-arms.
pub const ACTOR_KEY: &str = "silent_session_actor";

/// The `scope` every alarm this handler files carries.
pub const SCOPE: &str = "silent-factory-session";

/// The instant the alarm recorded as the start of the silence — what a
/// later write must be newer than to close it.
pub const SINCE_KEY: &str = "silent_since";

/// How many of a session's newest events one tick reads.
pub const EVENT_WINDOW: usize = 25;

const SESSION_KIND: &str = "work-session";
const SESSION_ACTOR: &str = "actor";
const RUN_KIND: &str = "agent-run";
const RUN_AGENT: &str = "agent";

/// The jobs API's own page ceiling for a queue read.
const QUEUE_LIMIT: usize = 1000;

/// The key the schedule runner writes the firing instant under.
const TICK_AT: &str = "_at";

/// The rule's declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Bounds {
    pub silent_minutes: i64,
    pub ignore_key_prefixes: Vec<String>,
    /// The keys of the supervisor's RESTART note: one newer than the
    /// actor's last own write overrides a bound worker (review 1c7ab966
    /// finding 1). Each must also be ignored as speech.
    pub restart_key_prefixes: Vec<String>,
}

impl Bounds {
    /// The bound: how long an actor that is not moving may be silent.
    pub fn bound(&self) -> Option<Duration> {
        Duration::try_minutes(self.silent_minutes)
    }

    /// How long a bound, unretired worker may defer the alarm, measured
    /// from the actor's own last write: twice the bound, never longer
    /// (decided_2026_10_29_factory_watch_repair_2 on 628dc540). Derived
    /// rather than declared, so it cannot drift from the bound it doubles.
    pub fn worker_deferral(&self) -> Option<Duration> {
        self.silent_minutes
            .checked_mul(2)
            .and_then(Duration::try_minutes)
    }
}

/// A comma-separated list of key prefixes, absent as none.
fn prefixes(args: &[(String, Value)], name: &str) -> Result<Vec<String>, String> {
    match arg(args, name) {
        None => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(s
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()),
        Some(other) => Err(format!(
            "`{name}` must be a comma-separated string of key prefixes, got a {} ({other:?})",
            other.kind()
        )),
    }
}

/// Parse the rule's args. The bound is required; a malformed one
/// refuses by name — a watch reading nothing is a watch that is not
/// running.
pub fn bounds(args: &[(String, Value)]) -> Result<Bounds, String> {
    let silent_minutes = match arg(args, "silent_minutes") {
        Some(Value::Int(n))
            if *n > 0 && n.checked_mul(2).and_then(Duration::try_minutes).is_some() =>
        {
            *n
        }
        Some(other) => {
            return Err(format!(
                "`silent_minutes` must be a positive whole number, got a {} ({other:?})",
                other.kind()
            ));
        }
        None => {
            return Err(
                "`silent_minutes` is not declared on the rule — the bound is the rule's to state"
                    .into(),
            );
        }
    };
    let ignore_key_prefixes = prefixes(args, "ignore_key_prefixes")?;
    let restart_key_prefixes = prefixes(args, "restart_key_prefixes")?;
    // A restart note the rule does not ALSO ignore is the actor speaking,
    // so it is the actor's last write and can never be newer than it: the
    // override it declares would never fire. Refused by name.
    if let Some(p) = restart_key_prefixes.iter().find(|p| {
        !ignore_key_prefixes
            .iter()
            .any(|i| p.starts_with(i.as_str()))
    }) {
        return Err(format!(
            "`restart_key_prefixes` names `{p}`, which no `ignore_key_prefixes` entry covers — a \
             restart note the rule does not ignore reads as the actor speaking, so it could never \
             be newer than the actor's last write"
        ));
    }
    Ok(Bounds {
        silent_minutes,
        ignore_key_prefixes,
        restart_key_prefixes,
    })
}

fn text<'a>(v: &'a Json, k: &str) -> &'a str {
    v.get(k).and_then(Json::as_str).unwrap_or("")
}

fn instant(v: Option<&Json>) -> Option<DateTime<Utc>> {
    v.and_then(Json::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// One thing the actor holds: an open run, or an active step.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    pub packet: String,
    pub kind: String,
    pub what: String,
}

/// The actor's open runs, as the open-runs read listed them. Pure.
pub fn runs_of(open_runs: &[Json], actor: &Actor) -> Vec<Json> {
    open_runs
        .iter()
        .filter(|r| text(r, "status") == "open")
        .filter(|r| {
            r.pointer(&format!("/metadata/{RUN_AGENT}"))
                .and_then(Json::as_str)
                .is_some_and(|w| actor.spellings.contains(w))
        })
        .cloned()
        .collect()
}

/// The actor's open runs, as held work. Pure.
pub fn runs_held(open_runs: &[Json], actor: &Actor) -> Vec<Held> {
    runs_of(open_runs, actor)
        .iter()
        .map(|r| Held {
            packet: text(r, "id").to_string(),
            kind: RUN_KIND.to_string(),
            what: format!(
                "agent-run for packet {}",
                r.pointer("/metadata/packet")
                    .and_then(Json::as_str)
                    .unwrap_or("(unnamed)")
            ),
        })
        .collect()
}

/// Whether an open run's dev-build worker is live as the record can say
/// it (finding 2): its declared worker receipt (`worker_started`) and its
/// binding (`worker_session`) are both recorded FOR THIS RUN, and no
/// retirement (`worker_retirement`) has been. The receipt carries no
/// timestamp — it is observed against the building step's 15-minute
/// `worker_start_expectation` by
/// `a-declared-worker-receipt-is-observed-every-five-minutes` — and
/// nothing in the record observes the worker pod after that, so this is
/// a bound worker not yet retired, and the alarm says so. Pure.
///
/// A bound worker is therefore NOT proof of a live one, and it defers
/// the alarm only for [`Bounds::worker_deferral`] from the actor's own
/// last write (review 1c7ab966 finding 1): measured on 2026-10-28, run
/// 320b62e1's worker stayed bound and unretired from 06:31:29Z until
/// the run died at 11:00:25Z while the factory, stopped at a model limit
/// since 06:42:08Z, built nothing.
pub fn worker_live(run: &Json) -> bool {
    let id = text(run, "id");
    let names_run = |key: &str| {
        run.pointer(&format!("/metadata/{key}/run"))
            .and_then(Json::as_str)
            .is_some_and(|r| !id.is_empty() && r == id)
    };
    names_run("worker_started")
        && names_run("worker_session")
        && run
            .pointer("/metadata/worker_retirement")
            .is_none_or(Json::is_null)
}

/// The instant a gate-run last moved its branch: the newer of when it
/// was filed and when it launched the runner on a head. Pure.
pub fn gate_moved_at(gate: &Json) -> Option<DateTime<Utc>> {
    [
        instant(gate.get("opened_at")),
        instant(gate.pointer("/metadata/launched_at")),
    ]
    .into_iter()
    .flatten()
    .max()
}

/// The ACTIVE steps the actor's queue holds, on packets that are not a
/// work-session (its own session) or a run (read as a run above). Pure;
/// `rows` is the assignments read (`AssignmentRow`s).
pub fn claimed(rows: &[Json], actor: &Actor) -> Vec<Held> {
    let mut seen = BTreeSet::new();
    rows.iter()
        .filter(|r| !matches!(text(r, "workflow"), SESSION_KIND | RUN_KIND))
        .filter_map(|r| {
            let step = r.get("step")?;
            if text(step, "status") != "active"
                || !actor.spellings.contains(text(step, "assignee_id"))
            {
                return None;
            }
            seen.insert(text(step, "id").to_string()).then(|| Held {
                packet: text(r, "job_id").to_string(),
                kind: text(r, "workflow").to_string(),
                what: format!(
                    "{} `{}` ({}, {})",
                    text(r, "workflow"),
                    text(step, "spec_slug"),
                    text(r, "priority"),
                    text(r, "job_title"),
                ),
            })
        })
        .collect()
}

/// What one session's event window says about the actor's last write.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub session: String,
    /// The newest counted write of the actor — or, when the window holds
    /// none, the window's oldest event (`exact = false`): the silence is
    /// at least back to there.
    pub since: DateTime<Utc>,
    pub exact: bool,
    /// The metadata keys that write changed — what the last note was.
    /// Empty when it cannot be read (a step event, or a floor). A full
    /// window's first event is never this write: it is unknown.
    pub keys: Vec<String>,
    /// The newest supervisor RESTART note the actor's spelling signed in
    /// the window (a key under `restart_key_prefixes`), if any. One newer
    /// than the actor's last own write is the loop saying the CLI itself
    /// is down, which overrides a bound worker.
    pub restart: Option<DateTime<Utc>>,
}

/// Whether a restart note is newer than the actor's last own write, as
/// the readings place it (the newest `since` across its sessions). Pure.
pub fn restarted(readings: &[Reading]) -> bool {
    let Some(last) = readings.iter().map(|r| r.since).max() else {
        return false;
    };
    readings.iter().filter_map(|r| r.restart).any(|t| t > last)
}

/// The newest movement of the actor the record shows: when, and what —
/// the run and step it completed, or the gate-run that moved a run's
/// branch — so a withdrawal can say what moved (backlog 7de4cc46).
#[derive(Debug, Clone, PartialEq)]
pub struct Moved {
    pub at: DateTime<Utc>,
    pub what: String,
}

/// The newer of two movements. Pure.
fn newer(a: Option<Moved>, b: Option<Moved>) -> Option<Moved> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.at > a.at { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// The newest step of a listed run completed at or past `since` BY the
/// actor, under any of its spellings (review 1c7ab966 finding 2). The
/// `step_completed_since` read matches a run when any of its steps
/// completed past the cutoff, WHOEVER completed it, so the server's
/// count is not this answer: measured on 2026-10-28, the run-died rule
/// completing 320b62e1's `building` at 11:00:25Z and the train landing
/// 038f261a and c1d57ed2 at 21:08Z and 21:10Z each read as the factory
/// moving for 45 minutes. Automation never counts. Pure.
pub fn last_own_completion(runs: &[Json], actor: &Actor, since: DateTime<Utc>) -> Option<Moved> {
    runs.iter()
        .flat_map(|r| {
            r.get("steps")
                .and_then(Json::as_array)
                .into_iter()
                .flatten()
                .map(move |s| (r, s))
        })
        .filter(|(_, s)| actor.spellings.contains(text(s, "completed_by")))
        .filter_map(|(r, s)| {
            let at = instant(s.get("completed_at")).filter(|t| *t >= since)?;
            Some(Moved {
                at,
                what: format!(
                    "{} completed step `{}` of its agent-run {}",
                    text(s, "completed_by"),
                    text(s, "spec_slug"),
                    text(r, "id"),
                ),
            })
        })
        .max_by_key(|m| m.at)
}

/// The top-level metadata keys `after` adds, changes or removes against
/// `before`, sorted. Pure.
pub fn written_keys(before: Option<&Json>, after: &Json) -> Vec<String> {
    let empty = serde_json::Map::new();
    let b = before.and_then(Json::as_object).unwrap_or(&empty);
    let a = after.as_object().unwrap_or(&empty);
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect()
}

/// Read one session's event window (oldest first, as the API answers
/// it). `None` for an empty window — no history is no reading, never a
/// silence. Pure.
///
/// A window shorter than [`EVENT_WINDOW`] is the session's WHOLE
/// history, so its first event's predecessor is the empty metadata and
/// what it wrote can be read. A FULL window's first metadata event has
/// its predecessor outside the window: what it changed is UNKNOWN, and
/// an unknown event sets the floor only — it is never the actor
/// speaking, so it can neither hold the alarm off nor withdraw one
/// (review 97bb7698 finding 3).
pub fn reading(session: &str, events: &[Json], actor: &Actor, b: &Bounds) -> Option<Reading> {
    let whole_history = events.len() < EVENT_WINDOW;
    let empty = Json::Object(serde_json::Map::new());
    let mut previous: Option<&Json> = whole_history.then_some(&empty);
    let mut newest: Option<(DateTime<Utc>, Vec<String>)> = None;
    let mut oldest: Option<DateTime<Utc>> = None;
    let mut restart: Option<DateTime<Utc>> = None;
    for (i, event) in events.iter().enumerate() {
        let Some(at) = instant(event.get("at")) else {
            continue;
        };
        oldest = Some(oldest.map_or(at, |o| o.min(at)));
        let metadata = event.pointer("/payload/metadata");
        // A full window's oldest event, and any metadata event before
        // the window holds a predecessor for it: unknown, skipped.
        if (i == 0 && !whole_history) || (previous.is_none() && metadata.is_some()) {
            if metadata.is_some() {
                previous = metadata;
            }
            continue;
        }
        // What this write changed, when both it and the one before it
        // carry the job's metadata; a step event changed something this
        // window cannot name.
        let keys = match (previous, metadata) {
            (Some(before), Some(after)) => written_keys(Some(before), after),
            _ => Vec::new(),
        };
        if metadata.is_some() {
            previous = metadata;
        }
        if !actor.spellings.contains(text(event, "actor")) {
            continue;
        }
        // The supervisor's restart note, read from a KNOWN event only (a
        // full window's first is skipped above).
        let restart_note = keys.iter().any(|k| {
            b.restart_key_prefixes
                .iter()
                .any(|p| k.starts_with(p.as_str()))
        });
        if restart_note && restart.is_none_or(|r| at > r) {
            restart = Some(at);
        }
        // The supervisor's write: it changed a key under an ignored
        // prefix. The loop's heartbeat changes `last_active_at` beside
        // `factory_turn`, so ANY key decides it, not every key.
        let ignored = keys.iter().any(|k| {
            b.ignore_key_prefixes
                .iter()
                .any(|p| k.starts_with(p.as_str()))
        });
        if ignored {
            continue;
        }
        if newest.as_ref().is_none_or(|(n, _)| at >= *n) {
            newest = Some((at, keys));
        }
    }
    match newest {
        Some((since, keys)) => Some(Reading {
            session: session.to_string(),
            since,
            exact: true,
            keys,
            restart,
        }),
        None => oldest.map(|since| Reading {
            session: session.to_string(),
            since,
            exact: false,
            keys: Vec::new(),
            restart,
        }),
    }
}

/// What one tick does for one actor.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    /// File the alarm on this reading.
    File(Reading),
    /// File the alarm with no reading: the actor holds work and has no
    /// work-session at all, open or ended, to be heard on (finding 1).
    FileUnheard,
    /// Withdraw the open alarm: the actor wrote at this reading.
    Withdraw(Reading),
    /// Withdraw the open alarm: the actor is moving, as this says,
    /// after the silence the alarm recorded (backlog 7de4cc46).
    WithdrawMoving(Moved),
    Hold,
}

/// The judgement, pure. `moved` is the actor's newest movement the
/// record shows — a step of one of its runs completed BY it, or a
/// gate-run of an open run of it moving its branch — read back as far
/// as a movement could still hold this tick; `worker_bound` whether an
/// open run of it has a bound, unretired dev-build worker
/// ([`worker_live`]); `alarm` the actor's open alarm, if any; `readings`
/// one per session read (its open sessions, or its newest one when none
/// is open).
///
/// A movement HOLDS the alarm while it is inside the bound. A bound
/// worker DEFERS the alarm, it does not hold it: from the actor's own
/// last write — the newer of its session reading and its own last
/// movement (backlog 7de4cc46) — the bound becomes
/// [`Bounds::worker_deferral`] (twice the bound) and never longer, and a
/// restart note newer than its last session write ([`restarted`])
/// removes the deferral — the loop is saying the CLI itself is down
/// (review 1c7ab966 finding 1). With neither a reading nor a movement
/// there is no last write to measure a deferral from, so a holder with
/// nothing to hear it on files whatever its workers say.
///
/// An open alarm is withdrawn by a counted session write newer than the
/// silence it recorded, or by a movement newer than that silence which
/// holds this tick by the same measure — so the next tick, with no alarm
/// open, holds on it too rather than filing the alarm again.
pub fn judge(
    readings: &[Reading],
    held: &[Held],
    moved: Option<&Moved>,
    worker_bound: bool,
    alarm: Option<&Json>,
    now: DateTime<Utc>,
    b: &Bounds,
) -> Act {
    let limit = if worker_bound && !restarted(readings) {
        b.worker_deferral()
    } else {
        b.bound()
    };
    // The movement alone keeps the actor from being filed this tick:
    // inside the bound (the read's own `>=` cutoff), or inside the
    // deferral a bound worker allows from it.
    let holds = |m: &&Moved| {
        b.bound().is_some_and(|l| now - m.at <= l) || limit.is_some_and(|l| now - m.at < l)
    };
    if let Some(alarm) = alarm {
        // A counted write of the actor, newer than the silence the alarm
        // recorded, closes it. A floor reading moves forward as the
        // window slides under other actors' writes, and must never read
        // as the factory speaking.
        let recorded = instant(alarm.pointer(&format!("/metadata/{SINCE_KEY}")));
        let spoke = readings
            .iter()
            .filter(|r| r.exact)
            .filter(|r| recorded.is_some_and(|t| r.since > t))
            .max_by_key(|r| r.since);
        if let Some(r) = spoke {
            return Act::Withdraw(r.clone());
        }
        // So does the actor moving after that silence (backlog
        // 7de4cc46). A movement before it is what the alarm already
        // weighed, and never closes it.
        return match moved
            .filter(|m| recorded.is_some_and(|t| m.at > t))
            .filter(holds)
        {
            Some(m) => Act::WithdrawMoving(m.clone()),
            None => Act::Hold,
        };
    }
    if held.is_empty() || moved.is_some_and(|m| holds(&m)) {
        return Act::Hold;
    }
    // The NEWEST reading across the actor's sessions: a write in any of
    // them is the actor speaking, and a floor is an upper bound on its
    // last write in that session, so the max is the silence it has
    // provably kept. No reading at all, while it holds work, is a holder
    // nothing can hear — the case the six-hour age-out made silent.
    let Some(newest) = readings.iter().max_by_key(|r| r.since) else {
        return Act::FileUnheard;
    };
    match limit {
        Some(limit) if now - newest.since >= limit => Act::File(newest.clone()),
        _ => Act::Hold,
    }
}

fn minutes(since: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    (now - since).num_minutes()
}

fn last_note(r: &Reading) -> String {
    if r.keys.is_empty() {
        "unreadable from the window".into()
    } else {
        r.keys.join(", ")
    }
}

/// What a withdrawal says, by what closed the alarm: the session write
/// it read, or the movement it read — what moved, and when — never a
/// bare withdrawal (backlog 7de4cc46). Pure.
pub fn withdrawal_evidence(
    actor: &str,
    act: &Act,
    alarm: &Json,
    now: DateTime<Utc>,
) -> Option<String> {
    let recorded = alarm
        .pointer(&format!("/metadata/{SINCE_KEY}"))
        .and_then(Json::as_str)
        .unwrap_or("(unrecorded)");
    match act {
        Act::Withdraw(r) => Some(format!(
            "{HANDLER} read work-session {} at {}: {actor} wrote to it again at {} (changed: \
             {}), after the silence this alarm recorded.",
            r.session,
            now.to_rfc3339(),
            r.since.to_rfc3339(),
            last_note(r),
        )),
        Act::WithdrawMoving(m) => Some(format!(
            "{HANDLER} read the record at {}: {} at {}, after the silence this alarm recorded \
             from {recorded}. {actor} is moving — the same read that keeps this watch from \
             filing — whatever its work-session says: an actor with no open session has none to \
             write to (backlog 7de4cc46).",
            now.to_rfc3339(),
            m.what,
            m.at.to_rfc3339(),
        )),
        Act::File(_) | Act::FileUnheard | Act::Hold => None,
    }
}

/// What the watch heard the actor on: the newest reading across the
/// sessions it read, if any, and whether those were OPEN sessions or,
/// none being open, the actor's newest ended one (finding 1).
#[derive(Debug, Clone, Copy)]
pub struct Heard<'a> {
    pub reading: Option<&'a Reading>,
    pub session_open: bool,
    /// An open run of it has a bound, unretired worker ([`worker_live`]).
    pub worker_bound: bool,
    /// A restart note is newer than its last own write ([`restarted`]).
    pub restarted: bool,
}

/// The alarm one silent actor files.
pub fn alarm_body(
    actor: &str,
    heard: Heard<'_>,
    held: &[Held],
    b: &Bounds,
    owner: &str,
    now: DateTime<Utc>,
    rule: &str,
) -> Json {
    let id8 = |s: &str| s[..s.len().min(8)].to_string();
    // With nothing to hear it on, the silence is recorded from now: the
    // first write of its own after this instant withdraws the alarm.
    let since = heard.reading.map_or(now, |r| r.since);
    let exact = heard.reading.is_some_and(|r| r.exact);
    let session = heard.reading.map(|r| r.session.clone());
    let keys = heard.reading.map(|r| r.keys.clone()).unwrap_or_default();
    let note = heard
        .reading
        .map_or_else(|| "none (no work-session)".to_string(), last_note);
    let m = minutes(since, now);
    let silence = match heard.reading {
        None => "as long as it has had no work-session — there is none, open or ended, to hear \
                 it on"
            .to_string(),
        Some(r) if r.exact => format!(
            "{m} minute(s) — its last write landed {}",
            r.since.to_rfc3339()
        ),
        Some(r) => format!(
            "at least {m} minute(s) — none of the {EVENT_WINDOW} newest events on the session, \
             back to {}, is its own",
            r.since.to_rfc3339()
        ),
    };
    let where_heard = match (&session, heard.session_open) {
        (Some(s), true) => format!("its open work-session {s}"),
        (Some(s), false) => format!(
            "its newest work-session {s} — it has no OPEN work-session (the six-hour age-out, \
             work-session-ends-when-silent, closes a session that stops writing \
             `last_active_at`)"
        ),
        (None, _) => "any work-session: it has no OPEN work-session, and none ended".to_string(),
    };
    let holds: Vec<String> = held
        .iter()
        .map(|h| format!("{} on {}", h.what, h.packet))
        .collect();
    let deferral = b.silent_minutes.saturating_mul(2);
    let workers = match (heard.worker_bound, heard.restarted) {
        (false, _) => "None of its open runs has a bound, unretired dev-build worker.".to_string(),
        (true, true) => "One of its open runs still has a bound, unretired dev-build worker, but \
                         a restart note newer than its last own write says the supervised CLI \
                         itself is down, so the worker defers nothing."
            .to_string(),
        (true, false) => format!(
            "One of its open runs still has a bound, unretired dev-build worker, which defers \
             this alarm for at most {deferral} minutes from its last own write or own run step \
             completion and never longer: \
             nothing in the record observes a worker after its receipt, so a bound worker is not \
             proof of a live one."
        ),
    };
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog 628dc540) at {now}: {actor} holds {n} claimed \
         item(s) and has written nothing of its own to {where_heard} for {silence}. Not moving \
         inside the {bound}-minute bound, as the record shows it: no step of its agent-runs was \
         completed BY it (a completion by automation — the run-died rule, a train landing, a \
         marker — is not the actor moving), and no gate-run of them moved its branch. {workers} \
         Its last note changed: {note}. Held: {holds}. A factory that stops on a model-capacity or rate limit \
         looks like this; so does a long build whose worker the record cannot see. If it runs \
         under infra/dev/factory-loop.sh, its `factory_restart_*` notes on the session say \
         whether it is backing off a limit. THE DOOR for a stalled or absent factory process is \
         `boss factory restart --why <text>`, run by the conductor in the dev pod (backlog \
         ccd985ef): it does nothing when the supervised loop runs, wakes an interactive session \
         idle at its prompt with a labelled peer message (and types nothing into a busy one), \
         starts infra/dev/factory-loop.sh when no factory runs, never kills anything, and \
         records what it saw and did as a `factory_restart_*` note on the session and on this \
         alarm. BEFORE RELEASING ANYTHING, check worker liveness \
         and branch movement: whether the dev-build worker each held run names \
         (`worker_session`) is still running, and whether its branch moved on the forge. \
         Failover is the conductor's, and it is to release only a run whose worker is gone \
         (`boss step release <packet> --step <slug> --why <text>` frees its claim), then carry \
         that build and hand it back when {actor} writes again; a live worker or a moving \
         branch is a factory working. This alarm marks itself RECOVERED when {actor} writes \
         to its session again, or is seen moving after the silence it recorded (it completes a \
         step of one of its own runs, or a gate-run of one moves its branch). It then stays \
         open for the rule's `hold_days` window: a silence inside it is appended here as a \
         relapse, and a window with none closes it (backlog f4b0ad41).",
        now = now.to_rfc3339(),
        n = held.len(),
        bound = b.silent_minutes,
        holds = holds.join("; "),
    );
    let metadata = json!({
        "area": "agents",
        "scope": SCOPE,
        ACTOR_KEY: actor,
        "session": session,
        "session_open": heard.session_open,
        SINCE_KEY: since.to_rfc3339(),
        "silence_is_a_floor": !exact,
        "silent_minutes": m,
        "bound_minutes": b.silent_minutes,
        "worker_bound": heard.worker_bound,
        "restart_overrode_worker": heard.worker_bound && heard.restarted,
        "last_note_keys": keys,
        "held": held.iter().map(|h| json!({
            "packet": h.packet,
            "kind": h.kind,
            "what": h.what,
        })).collect::<Vec<_>>(),
        "measured_at": now.to_rfc3339(),
        "detail": detail,
    });
    json!({
        "kind": "backlog-item",
        "title": match &session {
            Some(s) => format!(
                "SILENT: {actor} has written nothing to {}work-session {} for {}{}m while \
                 holding {} claimed item(s) — last note {}",
                if heard.session_open { "" } else { "its ended " },
                id8(s),
                m,
                if exact { "" } else { "+" },
                held.len(),
                note,
            ),
            None => format!(
                "SILENT: {actor} holds {} claimed item(s) and has no work-session to be heard on",
                held.len(),
            ),
        },
        "subject": {"subject_kind": "custom", "id": actor},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

/// Every OPEN alarm of this handler, keyed by the actor it names.
pub fn alarms_by_actor(rows: &[Json]) -> BTreeMap<String, Json> {
    rows.iter()
        .filter(|r| text(r, "status") == "open")
        .filter_map(|r| {
            let who = r
                .pointer(&format!("/metadata/{ACTOR_KEY}"))
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())?;
            Some((who.to_string(), r.clone()))
        })
        .collect()
}

fn zulu(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub struct JobsSilentSessionWatch {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    messages_base: String,
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsSilentSessionWatch {
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
            rows.extend(
                rows_or_refuse::<Json>(&body, "GET /api/jobs/assignments")
                    .map_err(HandlerError::Downstream)?,
            );
        }
        Ok(rows)
    }

    /// The sessions the actor is heard on, and whether they are open:
    /// every OPEN work-session of it, or, when none is open, its NEWEST
    /// work-session of any status, per spelling (finding 1).
    async fn sessions(
        &self,
        actor: &Actor,
        rule: &str,
    ) -> Result<(Vec<String>, bool), HandlerError> {
        let open = self.open_sessions(actor, rule).await?;
        if !open.is_empty() {
            return Ok((open, true));
        }
        let mut ids = BTreeSet::new();
        for who in &actor.spellings {
            let body = get_json(
                &self.client,
                &format!(
                    "{}/api/jobs?{}",
                    self.base(),
                    query(&[
                        ("kind", SESSION_KIND),
                        ("metadata", &containment(SESSION_ACTOR, who)),
                        ("order", "newest"),
                        ("limit", "1"),
                        ("full", "false"),
                    ])
                ),
                rule,
            )
            .await?;
            let rows: Vec<Json> =
                rows_or_refuse(&body, "GET /api/jobs").map_err(HandlerError::Downstream)?;
            for row in own_rows(rows, SESSION_ACTOR, actor, "work-session")
                .map_err(HandlerError::Downstream)?
            {
                if !text(&row, "id").is_empty() {
                    ids.insert(text(&row, "id").to_string());
                }
            }
        }
        Ok((ids.into_iter().collect(), false))
    }

    /// The actor's open work-sessions, every spelling, each once.
    async fn open_sessions(&self, actor: &Actor, rule: &str) -> Result<Vec<String>, HandlerError> {
        let mut ids = BTreeSet::new();
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
            for row in own_rows(rows, SESSION_ACTOR, actor, "work-session")
                .map_err(HandlerError::Downstream)?
            {
                if text(&row, "status") == "open" && !text(&row, "id").is_empty() {
                    ids.insert(text(&row, "id").to_string());
                }
            }
        }
        Ok(ids.into_iter().collect())
    }

    async fn events(&self, session: &str, rule: &str) -> Result<Vec<Json>, HandlerError> {
        let Ok(id) = session.parse::<uuid::Uuid>() else {
            return Err(HandlerError::Downstream(format!(
                "work-session id `{session}` is not a uuid — its events cannot be read"
            )));
        };
        let body = get_json(
            &self.client,
            &format!("{}/api/jobs/{id}/events?limit={EVENT_WINDOW}", self.base()),
            rule,
        )
        .await?;
        rows_or_refuse(&body, "GET /api/jobs/{id}/events").map_err(HandlerError::Downstream)
    }

    /// The newest step of one of its runs the ACTOR completed at or after
    /// `since`, if any. The server narrows to runs with any step
    /// completed past the cutoff; which of those completions are the
    /// actor's is read off each listed step's `completed_by`
    /// ([`last_own_completion`]), so every matched row is read, not a
    /// count (review 1c7ab966 finding 2).
    async fn completed_since(
        &self,
        actor: &Actor,
        since: DateTime<Utc>,
        rule: &str,
    ) -> Result<Option<Moved>, HandlerError> {
        let mut last = None;
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
                        ("limit", &QUEUE_LIMIT.to_string()),
                    ])
                ),
                rule,
            )
            .await?;
            let rows: Vec<Json> =
                rows_or_refuse(&body, "GET /api/jobs").map_err(HandlerError::Downstream)?;
            let rows =
                own_rows(rows, RUN_AGENT, actor, "agent-run").map_err(HandlerError::Downstream)?;
            let total = body.get("total").and_then(Json::as_u64).ok_or_else(|| {
                HandlerError::Downstream(format!(
                    "the agent-run movement read for {} carried no `total` — a count that is \
                     not there is not zero",
                    actor.id
                ))
            })?;
            // A limit is not a filter: an unread row may hold the one
            // completion that is the actor's.
            if (rows.len() as u64) < total {
                return Err(HandlerError::Downstream(format!(
                    "the agent-run movement read for {} answered {} of {total} row(s) — the rows \
                     it did not return may hold the actor's own completion",
                    actor.id,
                    rows.len()
                )));
            }
            last = newer(last, last_own_completion(&rows, actor, since));
        }
        Ok(last)
    }

    /// Did a gate-run the open run launched move its branch at or after
    /// `since`, and when? A gate launches on a pushed head, so this is
    /// the branch movement the record holds (a run names no branch of its
    /// own until it gates). Its newest gate-run only.
    async fn gated_since(
        &self,
        run: &str,
        since: DateTime<Utc>,
        rule: &str,
    ) -> Result<Option<Moved>, HandlerError> {
        let body = get_json(
            &self.client,
            &format!(
                "{}/api/jobs?{}",
                self.base(),
                query(&[
                    ("kind", "gate-run"),
                    ("metadata", &containment("agent_run", run)),
                    ("order", "newest"),
                    ("limit", "1"),
                    ("full", "false"),
                ])
            ),
            rule,
        )
        .await?;
        let rows: Vec<Json> =
            rows_or_refuse(&body, "GET /api/jobs").map_err(HandlerError::Downstream)?;
        if let Some(stray) = rows
            .iter()
            .find(|g| g.pointer("/metadata/agent_run").and_then(Json::as_str) != Some(run))
        {
            return Err(HandlerError::Downstream(format!(
                "the gate-run read for run {run} answered {} that another run launched — the \
                 metadata filter was not applied",
                text(stray, "id")
            )));
        }
        Ok(rows
            .iter()
            .filter_map(|g| {
                let at = gate_moved_at(g).filter(|t| *t >= since)?;
                Some(Moved {
                    at,
                    what: format!(
                        "gate-run {} of its agent-run {run} moved branch {}",
                        text(g, "id"),
                        g.pointer("/metadata/branch")
                            .and_then(Json::as_str)
                            .unwrap_or("(unnamed)"),
                    ),
                })
            })
            .max_by_key(|m| m.at))
    }

    /// The actor's newest MOVEMENT at or after `since` (finding 2), if
    /// any: a step of one of its runs it completed, or a gate-run of one
    /// of its open runs moving its branch. A bound worker is not
    /// movement: it only defers the alarm, in [`judge`] (review 1c7ab966
    /// finding 1).
    async fn movement(
        &self,
        actor: &Actor,
        runs: &[Json],
        since: DateTime<Utc>,
        rule: &str,
    ) -> Result<Option<Moved>, HandlerError> {
        let mut last = self.completed_since(actor, since, rule).await?;
        for run in runs {
            last = newer(last, self.gated_since(text(run, "id"), since, rule).await?);
        }
        Ok(last)
    }

    /// The open alarm read clear this tick: THE HOLD (backlog f4b0ad41,
    /// `common::hold`). A clear is a RECOVERED note and the alarm stays
    /// open; a told alarm is left alone until its window passes with no
    /// relapse, and then withdrawn. `evidence` says what this tick read.
    /// True when it withdrew the alarm.
    async fn withdraw(
        &self,
        alarm: &Json,
        evidence: &str,
        now: DateTime<Utc>,
        window: Duration,
        rule: &str,
    ) -> Result<bool, HandlerError> {
        let id = text(alarm, "id");
        let Some(retraction) = retraction(alarm) else {
            tracing::warn!(rule, packet = %id, "{HANDLER}: the open alarm has no triage step to close");
            return Ok(false);
        };
        let note = match hold(alarm, retraction, Some(now), window) {
            // A hold already running still owes its priority (595e32f6).
            Hold::Holding => {
                hold_priority(&self.client, self.base(), alarm, rule).await?;
                return Ok(false);
            }
            Hold::Withdraw {
                step_id,
                recovered_at,
                ..
            } => {
                let said = format!("{} {evidence}", held_evidence(alarm, HANDLER, window, now));
                complete_step(
                    &self.client,
                    self.base(),
                    id,
                    &step_id,
                    withdrawal_fields(&said, HANDLER, &recovered_at),
                    rule,
                )
                .await?;
                return Ok(true);
            }
            Hold::Note => hold_note(evidence, HANDLER, &now.to_rfc3339(), window),
            Hold::End { why_open } => hold_end_note(evidence, &why_open),
            Hold::Annotate { why_open } => {
                recovery_note(evidence, HANDLER, &now.to_rfc3339(), &why_open)
            }
        };
        let note = Json::Object(note);
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{}/api/jobs/{id}/metadata", self.base()),
            &note,
            rule,
        )
        .await?;
        // The note first: the lowering is guarded on the hold it lowers for.
        hold_priority(&self.client, self.base(), &with_note(alarm, &note), rule).await?;
        Ok(false)
    }
}

#[async_trait]
impl Handler for JobsSilentSessionWatch {
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
        let window = hold_window(args).map_err(HandlerError::Permanent)?;
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
        let open_runs = jobs_where_slim(
            &self.client,
            self.base(),
            &query(&[("kind", RUN_KIND), ("status", "open")]),
            rule,
        )
        .await?;
        let alarms = alarms_by_actor(
            &jobs_where(
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
        // How far back a movement can still hold this tick: the bound, or
        // the deferral when a bound worker stretches it ([`judge`]).
        let reach = |d: Option<Duration>| {
            d.and_then(|d| now.checked_sub_signed(d)).ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "silent_minutes {} reaches outside the calendar",
                    b.silent_minutes
                ))
            })
        };
        let (cutoff, deferred_cutoff) = (reach(b.bound())?, reach(b.worker_deferral())?);
        let (mut filed, mut held_open, mut withdrawn) = (0usize, 0usize, 0usize);
        for actor in &actors {
            let alarm = alarms.get(&actor.id);
            let mut held = runs_held(&open_runs, actor);
            held.extend(claimed(&self.queue(actor, rule).await?, actor));
            if held.is_empty() && alarm.is_none() {
                continue;
            }
            let mut readings = Vec::new();
            let (sessions, session_open) = self.sessions(actor, rule).await?;
            for session in sessions {
                if let Some(r) = reading(&session, &self.events(&session, rule).await?, actor, &b) {
                    readings.push(r);
                }
            }
            let runs = runs_of(&open_runs, actor);
            let worker_bound = runs.iter().any(worker_live);
            // Read whether or not an alarm is open: the movement that
            // stops a filing also withdraws one (backlog 7de4cc46).
            let since = if worker_bound {
                deferred_cutoff
            } else {
                cutoff
            };
            let moved = self.movement(actor, &runs, since, rule).await?;
            let file = |r: Option<&Reading>| {
                alarm_body(
                    &actor.id,
                    Heard {
                        reading: r,
                        session_open,
                        worker_bound,
                        restarted: restarted(&readings),
                    },
                    &held,
                    &b,
                    &owner,
                    now,
                    rule,
                )
            };
            // An alarm told the actor RECOVERED stands for no live
            // silence: the tick is judged as if none were open, and a
            // silence it would file is a relapse appended to that alarm
            // (backlog f4b0ad41) — it held instead, so the packet went on
            // saying RECOVERED over a silent actor.
            let standing = alarm.filter(|a| alarm_item::already_recorded(Some(a), None));
            let act = judge(
                &readings,
                &held,
                moved.as_ref(),
                worker_bound,
                standing,
                now,
                &b,
            );
            let body = match &act {
                Act::File(r) => Some(file(Some(r))),
                Act::FileUnheard => Some(file(None)),
                Act::Withdraw(_) | Act::WithdrawMoving(_) => {
                    if let Some(alarm) = alarm
                        && let Some(evidence) = withdrawal_evidence(&actor.id, &act, alarm, now)
                    {
                        // Held open RECOVERED, not closed (f4b0ad41).
                        self.withdraw(alarm, &evidence, now, window, rule).await?;
                        held_open += 1;
                    }
                    None
                }
                Act::Hold => {
                    // A RECOVERED alarm whose actor has not gone silent
                    // again: the hold, which withdraws it once its window
                    // has passed with no relapse (backlog f4b0ad41). It
                    // is not standing, so nobody is told of it.
                    if let Some(alarm) = alarm.filter(|a| told_recovered(a)) {
                        let quiet = format!(
                            "At {} {HANDLER} read no silence of {} past its {}-minute bound.",
                            now.to_rfc3339(),
                            actor.id,
                            b.silent_minutes
                        );
                        if self.withdraw(alarm, &quiet, now, window, rule).await? {
                            withdrawn += 1;
                        }
                    }
                    // An open alarm is assigned to nobody, so its owner
                    // is told directly; the re-send collapses on its id,
                    // and repairs a send that failed after the filing
                    // (backlog 35dabc3d). A withdrawal tells no one.
                    else if let Some(alarm) = alarm {
                        tell_owner(
                            &self.client,
                            &self.messages_base,
                            SCOPE,
                            text(alarm, "id"),
                            alarm,
                            text(alarm, "owner_id"),
                            rule,
                        )
                        .await?;
                    }
                    None
                }
            };
            if let Some(body) = body {
                // A relapse carries the new silence whole, so the next
                // tick measures from it rather than from the old one.
                let refresh = alarm
                    .and_then(|_| body["metadata"].as_object().cloned())
                    .unwrap_or_default();
                let evidence = match &act {
                    Act::File(r) => format!("{} silent since {}", actor.id, r.since.to_rfc3339()),
                    _ => format!(
                        "{} holds claimed work with no session to hear it on",
                        actor.id
                    ),
                };
                let occ = Occurrence::new(now.to_rfc3339(), evidence);
                let raised = alarm_item::raise_minted(
                    &self.client,
                    self.base(),
                    alarm,
                    body.clone(),
                    &occ,
                    refresh,
                    rule,
                )
                .await?;
                // The owner is told of a new alarm and of a relapse on an
                // open one, under the alarm's collapsing id (35dabc3d).
                let (id, to) = match &raised {
                    Raised::Filed(id) => (id.clone().unwrap_or_default(), owner.clone()),
                    Raised::Appended(id) | Raised::Held(id) => (
                        id.clone(),
                        alarm
                            .map(|a| text(a, "owner_id"))
                            .unwrap_or_default()
                            .to_string(),
                    ),
                };
                tell_owner(
                    &self.client,
                    &self.messages_base,
                    SCOPE,
                    &id,
                    &body,
                    &to,
                    rule,
                )
                .await?;
                if matches!(raised, Raised::Filed(_)) {
                    filed += 1;
                }
            }
        }
        tracing::info!(
            rule,
            actors = actors.len(),
            open_alarms = alarms.len(),
            filed,
            held_open,
            withdrawn,
            "{HANDLER}: read {} actor(s)",
            actors.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::RECOVERED_AT;
    use super::super::listing_stub;
    use super::*;

    const CODEX: &str = "agent-codex";
    const CODEX_ALIAS: &str = "codex@algedonic.dev";
    const SESSION: &str = "243c9e91-4ab3-403c-a280-4dec3251096a";
    /// The factory's last write before the measured stall's tail.
    const LAST_WRITE: &str = "2026-10-28T20:57:58Z";

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn plus(s: &str, minutes: i64, seconds: i64) -> DateTime<Utc> {
        at(s) + Duration::minutes(minutes) + Duration::seconds(seconds)
    }

    fn b() -> Bounds {
        Bounds {
            silent_minutes: 45,
            ignore_key_prefixes: vec!["factory_restart_".into(), "factory_turn".into()],
            restart_key_prefixes: vec!["factory_restart_".into()],
        }
    }

    fn codex() -> Actor {
        Actor {
            id: CODEX.into(),
            spellings: BTreeSet::from([CODEX.to_string(), CODEX_ALIAS.to_string()]),
        }
    }

    /// One `jobs.job.updated` row as `GET /api/jobs/{id}/events` answers
    /// it: the whole job after the write, signed by `actor`.
    fn update(actor: &str, at: &str, metadata: Json) -> Json {
        json!({
            "kind": "jobs.job.updated", "actor": actor, "at": at, "job_id": SESSION,
            "step_id": null, "payload": {"_actor": actor, "id": SESSION, "metadata": metadata},
        })
    }

    /// The measured shape: the factory's own note, then the conductor's
    /// order written onto the factory's session.
    fn measured_window() -> Vec<Json> {
        let base = json!({"actor": CODEX});
        let mut with_note = base.clone();
        with_note["pvc_independent_review_target_20261028T2057Z"] = json!({"by": CODEX});
        let mut with_order = with_note.clone();
        with_order["conductor_order_add_20261028T2133Z"] = json!({"by": "conductor"});
        vec![
            update(CODEX, "2026-10-28T20:42:02Z", base),
            update(CODEX, LAST_WRITE, with_note),
            update("agent-claude", "2026-10-28T21:33:49Z", with_order),
        ]
    }

    fn held() -> Vec<Held> {
        vec![Held {
            packet: "b111a1df".into(),
            kind: "backlog-item".into(),
            what: "backlog-item `build` (urgent, an urgent build)".into(),
        }]
    }

    /// The actor completing a step of its own run at `t`.
    fn moved_at(t: DateTime<Utc>) -> Moved {
        Moved {
            at: t,
            what: format!("{CODEX} completed step `building` of its agent-run run-1"),
        }
    }

    fn read_measured() -> Reading {
        reading(SESSION, &measured_window(), &codex(), &b()).expect("a window is a reading")
    }

    #[test]
    fn the_declaration_is_a_positive_minutes_bound_and_optional_prefixes() {
        let ok = bounds(&[
            ("silent_minutes".into(), Value::Int(45)),
            (
                "ignore_key_prefixes".into(),
                Value::String("factory_restart_, other_".into()),
            ),
        ])
        .unwrap();
        assert_eq!(ok.silent_minutes, 45);
        assert_eq!(ok.ignore_key_prefixes, vec!["factory_restart_", "other_"]);
        assert!(
            ok.restart_key_prefixes.is_empty(),
            "no restart override unless declared"
        );
        assert!(bounds(&[]).is_err(), "the bound is the rule's to state");
        assert!(bounds(&[("silent_minutes".into(), Value::Int(0))]).is_err());
        assert!(bounds(&[("silent_minutes".into(), Value::String("45".into()))]).is_err());
    }

    /// The last counted write is the factory's OWN, and the conductor's
    /// order after it does not count; the note it wrote is named.
    #[test]
    fn the_silence_runs_from_the_actors_own_last_write_and_names_its_note() {
        let r = read_measured();
        assert_eq!(r.since, at(LAST_WRITE));
        assert!(r.exact);
        assert_eq!(r.keys, vec!["pvc_independent_review_target_20261028T2057Z"]);
    }

    /// 628dc540's own test: the rule fires at 45 minutes of silence and
    /// not at 44.
    #[test]
    fn it_fires_at_forty_five_minutes_and_not_at_forty_four() {
        let r = read_measured();
        let r = vec![r];
        assert_eq!(
            judge(
                &r,
                &held(),
                None,
                false,
                None,
                plus(LAST_WRITE, 44, 59),
                &b()
            ),
            Act::Hold,
            "44:59 is inside the bound"
        );
        assert_eq!(
            judge(
                &r,
                &held(),
                None,
                false,
                None,
                plus(LAST_WRITE, 45, 0),
                &b()
            ),
            Act::File(r[0].clone()),
            "45:00 is the bound"
        );
    }

    /// An actor that holds no claimed work is not a stalled factory,
    /// however long its session has been quiet.
    #[test]
    fn an_actor_holding_no_claimed_work_is_not_alarmed() {
        let r = vec![read_measured()];
        assert_eq!(
            judge(&r, &[], None, false, None, plus(LAST_WRITE, 600, 0), &b()),
            Act::Hold
        );
    }

    /// A run of the actor that moved inside the bound is the factory
    /// working, whatever its session says.
    #[test]
    fn a_run_that_moved_holds_the_alarm() {
        let r = vec![read_measured()];
        assert_eq!(
            judge(
                &r,
                &held(),
                Some(&moved_at(plus(LAST_WRITE, 80, 0))),
                false,
                None,
                plus(LAST_WRITE, 90, 0),
                &b()
            ),
            Act::Hold
        );
    }

    fn open_alarm(since: &str) -> Json {
        json!({
            "id": "al-1", "kind": "backlog-item", "status": "open",
            "metadata": {"scope": SCOPE, ACTOR_KEY: CODEX, SINCE_KEY: since},
            "steps": [{"id": "al-1-triage", "spec_slug": "triage", "status": "ready",
                       "metadata": {"authority_role": "platform-admin"}}],
        })
    }

    /// 628dc540's own test: a factory that writes again clears the
    /// alarm — and only its OWN write does: the conductor's order, or
    /// the supervisor's restart note, leaves it standing.
    #[test]
    fn the_actors_own_write_clears_the_alarm_and_nothing_else_does() {
        let alarm = open_alarm(LAST_WRITE);
        let now = plus(LAST_WRITE, 120, 0);
        let mut window = measured_window();
        let last = window.last().unwrap()["payload"]["metadata"].clone();

        // A restart note alone, signed by the factory's own alias.
        let mut restarted = last.clone();
        restarted["factory_restart_20261028T2210Z"] = json!({"exit_code": 1});
        window.push(update(
            CODEX_ALIAS,
            "2026-10-28T22:10:00Z",
            restarted.clone(),
        ));
        let r = vec![reading(SESSION, &window, &codex(), &b()).unwrap()];
        assert_eq!(
            judge(&r, &held(), None, false, Some(&alarm), now, &b()),
            Act::Hold,
            "a supervisor's restart note is not the factory speaking"
        );

        // The factory's own note.
        let mut spoke = restarted;
        spoke["handoff_20261028_2245Z"] = json!({"by": CODEX});
        window.push(update(CODEX, "2026-10-28T22:44:31Z", spoke));
        let r = reading(SESSION, &window, &codex(), &b()).unwrap();
        assert_eq!(r.since, at("2026-10-28T22:44:31Z"));
        assert_eq!(r.keys, vec!["handoff_20261028_2245Z"]);
        assert_eq!(
            judge(
                std::slice::from_ref(&r),
                &held(),
                None,
                false,
                Some(&alarm),
                now,
                &b()
            ),
            Act::Withdraw(r)
        );
    }

    /// Backlog 7de4cc46, pure: an open alarm on a FLOOR reading (no write
    /// of the actor's own on its session) is withdrawn by the actor moving
    /// AFTER the silence it recorded, and only while that movement would
    /// hold the next tick's filing — a movement before the silence, or
    /// one aged past the bound, leaves it standing, and so does the floor
    /// itself. A bound worker stretches the window to the deferral.
    #[test]
    fn an_actor_moving_after_the_silence_withdraws_and_nothing_older_does() {
        let floor = "2026-11-11T23:56:45.681628Z";
        let alarm = open_alarm(floor);
        let r = vec![Reading {
            session: "2a0c73be-cb07-40b0-9551-8ae7b0682619".into(),
            since: at(floor),
            exact: false,
            keys: vec![],
            restart: None,
        }];
        let now = at("2026-11-12T16:00:00Z");
        let judged = |m: Option<&Moved>, worker: bool| {
            judge(&r, &held(), m, worker, Some(&alarm), now, &b())
        };
        assert_eq!(judged(None, false), Act::Hold, "a floor never withdraws");
        let built = moved_at(at("2026-11-12T15:58:20Z"));
        assert_eq!(
            judged(Some(&built), false),
            Act::WithdrawMoving(built.clone())
        );
        let before = moved_at(plus(floor, -1, 0));
        assert_eq!(
            judged(Some(&before), true),
            Act::Hold,
            "a movement before the silence is what the alarm already weighed"
        );
        let aged = moved_at(now - Duration::minutes(46));
        assert_eq!(
            judged(Some(&aged), false),
            Act::Hold,
            "past the bound it would not hold the next tick, so it does not withdraw"
        );
        assert_eq!(
            judged(Some(&aged), true),
            Act::WithdrawMoving(aged.clone()),
            "inside a bound worker's deferral it would"
        );
        let mut unrecorded = alarm.clone();
        unrecorded["metadata"][SINCE_KEY] = Json::Null;
        assert_eq!(
            judge(
                &r,
                &held(),
                Some(&built),
                false,
                Some(&unrecorded),
                now,
                &b()
            ),
            Act::Hold,
            "a silence the alarm never recorded cannot be moved past"
        );
        let said = withdrawal_evidence(CODEX, &Act::WithdrawMoving(built), &alarm, now).unwrap();
        assert!(
            said.contains("`building`")
                && said.contains("2026-11-12T15:58:20")
                && said.contains(floor),
            "{said}"
        );
    }

    /// A window with none of the actor's writes is a FLOOR on the
    /// silence: it may fire on it, and a window that merely slides (new
    /// foreign writes, the oldest one dropping out) never reads as the
    /// actor speaking.
    #[test]
    fn a_window_without_the_actors_write_is_a_floor_that_never_clears() {
        let orders: Vec<Json> = (0..3)
            .map(|i| {
                update(
                    "agent-claude",
                    &format!("2026-10-28T21:{:02}:00Z", 10 + i),
                    json!({"actor": CODEX, format!("conductor_order_{i}"): i}),
                )
            })
            .collect();
        let r = reading(SESSION, &orders, &codex(), &b()).unwrap();
        assert!(!r.exact);
        assert_eq!(r.since, at("2026-10-28T21:10:00Z"));
        assert_eq!(
            judge(
                std::slice::from_ref(&r),
                &held(),
                None,
                false,
                None,
                plus("2026-10-28T21:10:00Z", 45, 0),
                &b()
            ),
            Act::File(r.clone())
        );
        let alarm = open_alarm("2026-10-28T21:10:00Z");
        let slid = reading(SESSION, &orders[1..], &codex(), &b()).unwrap();
        assert_eq!(
            judge(
                &[slid],
                &held(),
                None,
                false,
                Some(&alarm),
                plus("2026-10-28T21:10:00Z", 90, 0),
                &b()
            ),
            Act::Hold
        );
        assert_eq!(reading(SESSION, &[], &codex(), &b()), None);
    }

    #[test]
    fn the_held_work_is_open_runs_and_active_claims_off_the_session() {
        let runs = vec![
            json!({"id": "run-1", "status": "open", "metadata": {"agent": CODEX_ALIAS, "packet": "p9"}}),
            json!({"id": "run-2", "status": "open", "metadata": {"agent": "agent-claude"}}),
        ];
        assert_eq!(runs_held(&runs, &codex()).len(), 1);
        let row = |wf: &str, sid: &str, status: &str, who: &str| {
            json!({"job_id": format!("j-{sid}"), "job_title": "t", "workflow": wf,
                   "priority": "urgent",
                   "step": {"id": sid, "status": status, "assignee_id": who, "spec_slug": "build"}})
        };
        let queue = vec![
            row("backlog-item", "s1", "active", CODEX),
            row("backlog-item", "s1", "active", CODEX),
            row("backlog-item", "s2", "ready", CODEX),
            row("work-session", "s3", "active", CODEX),
            row("agent-run", "s4", "active", CODEX),
            row("backlog-item", "s5", "active", "agent-claude"),
        ];
        let c = claimed(&queue, &codex());
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].packet, "j-s1");
    }

    #[test]
    fn the_alarm_names_the_actor_the_silence_and_the_last_note() {
        let r = read_measured();
        let now = plus(LAST_WRITE, 46, 0);
        let heard = Heard {
            reading: Some(&r),
            session_open: true,
            worker_bound: false,
            restarted: false,
        };
        let body = alarm_body(CODEX, heard, &held(), &b(), "emp-owner", now, "a-rule");
        assert_eq!(body["priority"], "urgent");
        assert_eq!(body["owner_id"], "emp-owner");
        assert_eq!(body["metadata"][ACTOR_KEY], CODEX);
        assert_eq!(body["metadata"][SINCE_KEY], at(LAST_WRITE).to_rfc3339());
        assert_eq!(body["metadata"]["silent_minutes"], 46);
        let title = body["title"].as_str().unwrap();
        assert!(title.contains(CODEX), "{title}");
        assert!(title.contains("46m"), "{title}");
        assert!(
            title.contains("pvc_independent_review_target_20261028T2057Z"),
            "{title}"
        );
        assert_eq!(body["metadata"]["input_channel"], "telemetry/monitoring");
    }

    /// The alarm names its door (backlog ccd985ef): on 2026-10-29 alarm
    /// 7e947dbf fired at 06:10Z on an interactive factory idle at its
    /// prompt, and nothing could act on it for about 8 h because its
    /// text left the restart to whoever read it, with no verb to do it.
    #[test]
    fn the_alarm_names_boss_factory_restart_as_its_door() {
        let r = read_measured();
        let now = plus(LAST_WRITE, 46, 0);
        let heard = Heard {
            reading: Some(&r),
            session_open: true,
            worker_bound: false,
            restarted: false,
        };
        let body = alarm_body(CODEX, heard, &held(), &b(), "emp-owner", now, "a-rule");
        let detail = body["metadata"]["detail"].as_str().unwrap();
        assert!(
            detail.contains("`boss factory restart --why <text>`"),
            "{detail}"
        );
        for case in [
            "does nothing when the supervised loop runs",
            "wakes an interactive session idle at its prompt",
            "starts infra/dev/factory-loop.sh when no factory runs",
            "never kills",
        ] {
            assert!(detail.contains(case), "{case}: {detail}");
        }
    }

    // ---- end to end, through a stub jobs API ----

    const NOW: &str = "2026-10-28T21:45:00Z";

    fn ctx(now: &str) -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "a-silent-factory-holding-claimed-work-is-alarmed".into(),
            triggering_event_id: format!("clock-tick:{now}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": &now[..10], "_at": now}),
        }
    }

    fn args() -> Vec<(String, Value)> {
        vec![
            ("silent_minutes".into(), Value::Int(45)),
            (
                "ignore_key_prefixes".into(),
                Value::String("factory_restart_,factory_turn".into()),
            ),
        ]
    }

    fn listing(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    fn session_row(status: &str) -> Json {
        json!({"id": SESSION, "status": status, "metadata": {"actor": CODEX}})
    }

    fn stub_routes(alarms: Vec<Json>, events: Vec<Json>) -> Vec<(&'static str, Json)> {
        world(alarms, events, vec![], vec![session_row("open")], vec![])
    }

    /// The far side as the handler reads it: the open runs, the open
    /// sessions (and, behind them, the actor's NEWEST session of any
    /// status, which the six-hour age-out closed), and the gate-runs a
    /// run launched.
    fn world(
        alarms: Vec<Json>,
        events: Vec<Json>,
        runs: Vec<Json>,
        open_sessions: Vec<Json>,
        gates: Vec<Json>,
    ) -> Vec<(&'static str, Json)> {
        vec![
            (
                "/api/agents",
                listing(vec![json!({"id": CODEX, "aliases": [CODEX_ALIAS]})]),
            ),
            ("/api/jobs?kind=agent-run&status=open", listing(runs)),
            ("/api/jobs?kind=gate-run", listing(gates)),
            (
                "/api/jobs?kind=backlog-item&status=open&metadata_has=silent_session_actor",
                listing(alarms),
            ),
            (
                "/api/jobs?kind=agent-run&step_completed_since=2026-10-28T21%3A00%3A00Z",
                listing(vec![]),
            ),
            // A bound worker's actor is read back twice the bound.
            (
                "/api/jobs?kind=agent-run&step_completed_since=2026-10-28T20%3A15%3A00Z",
                listing(vec![]),
            ),
            (
                "/api/jobs/assignments",
                listing(vec![
                    json!({"job_id": "b111a1df", "job_title": "urgent build",
                    "workflow": "backlog-item", "priority": "urgent",
                    "step": {"id": "s-build", "status": "active", "assignee_id": CODEX,
                             "spec_slug": "build"}}),
                ]),
            ),
            (
                "/api/jobs?kind=work-session&status=open",
                listing(open_sessions),
            ),
            (
                "/api/jobs?kind=work-session",
                listing(vec![session_row("closed")]),
            ),
            (
                "/api/jobs/243c9e91-4ab3-403c-a280-4dec3251096a/events",
                json!({"data": events}),
            ),
        ]
    }

    fn handler(stub: &listing_stub::Stub) -> Arc<JobsSilentSessionWatch> {
        JobsSilentSessionWatch::new(
            stub.base.clone(),
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    #[tokio::test]
    async fn a_replay_of_the_measured_stall_files_exactly_one_alarm() {
        let stub = listing_stub::serve(stub_routes(vec![], measured_window())).await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "one alarm and its owner's message: {sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        assert_eq!(sent[0].1["metadata"][ACTOR_KEY], CODEX);
        assert_eq!(sent[0].1["metadata"]["session"], SESSION);
        // Assigned to nobody, so the platform owner is told directly
        // (backlog 35dabc3d: a79f9097 fired at 08:01Z on 2026-11-17 and
        // its only reader was the session that had stopped).
        assert_eq!(sent[1].0, "POST /api/messages/send");
        let msg = &sent[1].1;
        assert_eq!(msg["kind"], "direct");
        assert_eq!(msg["recipient_id"], "emp-owner");
        assert_eq!(msg["id"], format!("{SCOPE}:stub-created:emp-owner"));
        assert_eq!(msg["subject"], sent[0].1["title"]);
    }

    /// A held tick re-sends the open alarm's message under its own id,
    /// which the messages store collapses — the repair for a send that
    /// failed after the filing (35dabc3d). A withdrawal tells no one.
    #[tokio::test]
    async fn an_open_alarm_resends_only_its_owners_message() {
        let mut alarm = open_alarm(LAST_WRITE);
        alarm["owner_id"] = json!("emp-owner");
        alarm["title"] = json!("SILENT SESSION");
        let stub = listing_stub::serve(stub_routes(vec![alarm], measured_window())).await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        let sent = stub.sent();
        assert_eq!(stub.writes(), vec!["POST /api/messages/send"], "{sent:?}");
        assert_eq!(sent[0].1["id"], format!("{SCOPE}:al-1:emp-owner"));
        assert_eq!(sent[0].1["subject"], "SILENT SESSION");
    }

    /// The measured window with the factory's own write at 21:40Z — five
    /// minutes before [`NOW`], so it is not silent.
    fn spoke_window() -> Vec<Json> {
        let mut window = measured_window();
        window.push(update(
            CODEX,
            "2026-10-28T21:40:00Z",
            json!({"actor": CODEX, "handoff_20261028_2140Z": {}}),
        ));
        window
    }

    /// THE HOLD, pin 1 (backlog f4b0ad41): the actor's write clears the
    /// alarm, and the clear is a RECOVERED note on an alarm that stays
    /// OPEN — no step is written.
    #[tokio::test]
    async fn an_open_alarm_holds_the_next_tick_and_a_write_keeps_it_open_and_recovered() {
        let stub =
            listing_stub::serve(stub_routes(vec![open_alarm(LAST_WRITE)], measured_window())).await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());

        let stub =
            listing_stub::serve(stub_routes(vec![open_alarm(LAST_WRITE)], spoke_window())).await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        let sent = stub.sent();
        assert_eq!(
            stub.writes(),
            vec!["PATCH /api/jobs/al-1/metadata"],
            "one note, no step: {sent:?}"
        );
        assert_eq!(sent[0].1[RECOVERED_AT], at(NOW).to_rfc3339());
        assert_eq!(sent[0].1["recovered_by"], HANDLER);
        let text = sent[0].1["recovery"].as_str().unwrap();
        assert!(text.contains("handoff_20261028_2140Z"), "{text}");
        assert!(text.contains("HELD OPEN for 7 day(s)"), "{text}");
    }

    /// The alarm the clear above left: open, told RECOVERED at `told`.
    fn held_alarm(told: &str) -> Json {
        let mut alarm = open_alarm(LAST_WRITE);
        alarm["metadata"][RECOVERED_AT] = json!(told);
        alarm["metadata"]["recovered_by"] = json!(HANDLER);
        alarm["metadata"]["recovery"] = json!("RECOVERED — codex wrote again. HELD OPEN ...");
        alarm
    }

    /// Inside its window a held alarm is left alone — and, standing for
    /// no live silence, its owner is not re-told of it.
    #[tokio::test]
    async fn a_recovery_hold_ends_on_an_active_silence_alarm_without_completing_it() {
        let mut alarm = held_alarm("2026-10-22T21:45:01+00:00");
        alarm["metadata"]["held_days"] = json!(7);
        alarm["steps"][0]["status"] = json!("active");
        let stub = listing_stub::serve(stub_routes(vec![alarm], spoke_window())).await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        let sent = stub.sent();
        assert_eq!(
            stub.writes(),
            vec!["PATCH /api/jobs/al-1/metadata"],
            "{sent:?}"
        );
        assert_eq!(sent[0].1.get("held_days"), Some(&Json::Null));
        assert!(
            sent[0].1["recovery"]
                .as_str()
                .unwrap()
                .contains("HOLD ENDED")
        );
    }

    #[tokio::test]
    async fn a_held_alarm_inside_its_window_writes_nothing() {
        let mut alarm = held_alarm("2026-10-22T21:45:01+00:00");
        alarm["owner_id"] = json!("emp-owner");
        let stub = listing_stub::serve(stub_routes(vec![alarm], spoke_window())).await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.sent());
    }

    /// THE HOLD, pin 3: the first quiet tick at or past the window
    /// withdraws the alarm at its triage step, stamped `cleared_by`, with
    /// the instant it was told and the hold said in the evidence.
    #[tokio::test]
    async fn a_quiet_window_withdraws_the_held_alarm() {
        let told = "2026-10-21T21:45:00+00:00";
        let stub = listing_stub::serve(stub_routes(vec![held_alarm(told)], spoke_window())).await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "the merge, then the flip: {sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/al-1/steps/al-1-triage/metadata");
        assert_eq!(sent[0].1["disposition"], "stale");
        assert_eq!(sent[0].1["cleared_by"], HANDLER);
        assert_eq!(sent[0].1[RECOVERED_AT], told);
        let evidence = sent[0].1["evidence"].as_str().unwrap();
        assert!(
            evidence.contains("held this item open 7 day(s)"),
            "{evidence}"
        );
        assert!(evidence.contains(told), "{evidence}");
        assert_eq!(sent[1].0, "PUT /api/jobs/al-1/steps/al-1-triage");
    }

    /// THE HOLD, pin 2: a silence inside the window is a relapse,
    /// appended to the held alarm through `alarm_item` with the note
    /// withdrawn — never a second item, never a close.
    #[tokio::test]
    async fn a_relapse_in_the_window_appends_to_the_held_alarm() {
        let stub = listing_stub::serve(stub_routes(
            vec![held_alarm("2026-10-27T21:45:00+00:00")],
            measured_window(),
        ))
        .await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        let sent = stub.sent();
        assert_eq!(
            stub.writes(),
            vec!["PATCH /api/jobs/al-1/metadata"],
            "appended, not filed: {sent:?}"
        );
        assert_eq!(sent[0].1[RECOVERED_AT], Json::Null);
        assert_eq!(sent[0].1["recovery"], Json::Null);
        assert_eq!(sent[0].1[alarm_item::OCCURRENCES], 2);
        assert_eq!(sent[0].1[alarm_item::OCCURRENCE_LOG][0]["relapse"], true);
        assert_eq!(
            sent[0].1[SINCE_KEY],
            at(LAST_WRITE).to_rfc3339(),
            "the relapse carries the new silence"
        );
    }

    #[tokio::test]
    async fn an_empty_agents_registry_is_refused_not_read_as_no_agents() {
        let mut routes = stub_routes(vec![], measured_window());
        routes[0].1 = listing_stub::empty_listing();
        let stub = listing_stub::serve(routes).await;
        assert!(handler(&stub).invoke(&args(), &ctx(NOW)).await.is_err());
        assert!(stub.writes().is_empty());
    }

    #[tokio::test]
    async fn a_firing_without_an_instant_is_a_permanent_refusal() {
        let stub = listing_stub::serve(stub_routes(vec![], measured_window())).await;
        let mut c = ctx(NOW);
        c.event_payload = json!({"_day": "2026-10-28"});
        assert!(matches!(
            handler(&stub).invoke(&args(), &c).await,
            Err(HandlerError::Permanent(_))
        ));
    }

    /// The shipped row: 45 minutes, the supervisor's restart prefix, on
    /// a cadence of minutes.
    #[test]
    fn the_shipped_rule_watches_for_forty_five_minutes_on_a_minutes_tick() {
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
        assert_eq!(declared.silent_minutes, 45);
        assert_eq!(
            declared.ignore_key_prefixes,
            vec!["factory_restart_", "factory_turn"]
        );
        assert_eq!(declared.restart_key_prefixes, vec!["factory_restart_"]);
        assert_eq!(
            declared.worker_deferral(),
            Some(Duration::minutes(90)),
            "a bound worker defers for at most 90 minutes"
        );
        assert!(
            args.iter()
                .any(|(k, _)| k == super::super::common::HOLD_DAYS),
            "{}: the hold window is declared on the row, not left to the default",
            rule.name
        );
        assert_eq!(
            hold_window(&args),
            Ok(Duration::days(7)),
            "the seven-day hold of David's decision (f4b0ad41)"
        );
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

    /// The two supervisor prefixes live twice — the rule's arg and the
    /// keys the loop writes — so they are pinned (CLAUDE.md §9a): a
    /// supervisor that renamed its restart note or its turn heartbeat
    /// would hold this alarm off forever.
    #[test]
    fn the_ignored_prefix_is_the_key_the_factory_loop_writes() {
        let script =
            std::fs::read_to_string(boss_testing::repo_root().join("infra/dev/factory-loop.sh"))
                .expect("infra/dev/factory-loop.sh");
        assert!(
            script.contains("note_key=factory_restart_"),
            "factory-loop.sh writes its restart note under `factory_restart_<stamp>`"
        );
        assert!(
            script.contains("factory_turn: {"),
            "factory-loop.sh writes its turn heartbeat beside `factory_turn`"
        );
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsSilentSessionWatch::new(
            "http://unused",
            "http://unused",
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        assert_eq!(h.name(), HANDLER);
        assert!(
            crate::cascade::handler_emits().contains_key(HANDLER),
            "the cascade table knows what this handler emits"
        );
    }

    // ---- review 97bb7698 (CHANGES at 3346b34d): each finding's RED leg,
    // ---- now a car test (decided_2026_10_29_factory_watch_repair).

    /// Twenty-five events after the recorded silence, as a long stall
    /// under factory-loop.sh writes them: the loop's restart notes
    /// (signed by the factory's alias) and the conductor's orders.
    /// `oldest_is_note` picks which one the window's FIRST event is.
    fn slid_window(oldest_is_note: bool) -> Vec<Json> {
        let mut md = json!({"actor": CODEX, "pvc_independent_review_target_20261028T2057Z": 1});
        (0..EVENT_WINDOW)
            .map(|i| {
                let note = if i == 0 { oldest_is_note } else { i % 2 == 0 };
                let t = plus(LAST_WRITE, 60 + 30 * i as i64, 0).to_rfc3339();
                if note {
                    md[format!("factory_restart_{i:03}")] = json!({"exit_code": 1});
                    update(CODEX_ALIAS, &t, md.clone())
                } else {
                    md[format!("conductor_order_add_{i:03}")] = json!({"by": "conductor"});
                    update("agent-claude", &t, md.clone())
                }
            })
            .collect()
    }

    /// B3: a slid window's OLDEST event has no predecessor in the window,
    /// so what it changed cannot be read. An actor-signed restart note in
    /// that place read as the factory speaking (exact, keys empty) and
    /// withdrew the open alarm while the factory was still silent — and
    /// every later note sliding into that place did it again, an urgent
    /// packet about every 30 minutes. It is UNKNOWN: never the actor
    /// speaking, never a withdrawal.
    #[test]
    fn a_slid_window_whose_oldest_event_is_a_restart_note_holds_the_alarm() {
        let alarm = open_alarm(LAST_WRITE);
        let window = slid_window(true);
        let now = plus(LAST_WRITE, 60 + 30 * EVENT_WINDOW as i64, 0);
        let r = vec![reading(SESSION, &window, &codex(), &b()).unwrap()];
        assert!(!r[0].exact, "the oldest event is unknown: {:?}", r[0]);
        assert_eq!(
            judge(&r, &held(), None, false, Some(&alarm), now, &b()),
            Act::Hold,
            "no write of the factory's own landed; reading = {:?}",
            r[0]
        );
    }

    /// The control beside B3: the same slid window whose first event is
    /// the conductor's order holds the alarm on both heads.
    #[test]
    fn a_slid_window_whose_oldest_event_is_an_order_holds_the_alarm() {
        let alarm = open_alarm(LAST_WRITE);
        let window = slid_window(false);
        let now = plus(LAST_WRITE, 60 + 30 * EVENT_WINDOW as i64, 0);
        let r = vec![reading(SESSION, &window, &codex(), &b()).unwrap()];
        assert_eq!(
            judge(&r, &held(), None, false, Some(&alarm), now, &b()),
            Act::Hold
        );
    }

    /// B1: an actor that holds claimed work and has NO readable session
    /// is itself an alarm. Before, no reading was a Hold, so a factory
    /// whose session the six-hour age-out closed was never watched again.
    #[test]
    fn a_holder_whose_session_aged_out_is_still_watched() {
        assert_ne!(
            judge(
                &[],
                &held(),
                None,
                false,
                None,
                plus(LAST_WRITE, 600, 0),
                &b()
            ),
            Act::Hold,
            "ten hours holding claimed work with nothing heard"
        );
    }

    /// B1 end to end: the factory holds an urgent build and has no OPEN
    /// work-session (measured: d43a79c5 was ended at 11:00:45Z, 6 h 14 min
    /// after its last heartbeat). The watch reads its NEWEST session,
    /// whatever its status, and files on the silence it shows there.
    #[tokio::test]
    async fn a_holder_with_no_open_session_is_read_from_its_newest_session() {
        let stub =
            listing_stub::serve(world(vec![], measured_window(), vec![], vec![], vec![])).await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        let sent = filings(&stub);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        assert_eq!(sent[0].1["metadata"]["session"], SESSION);
        assert_eq!(
            sent[0].1["metadata"][SINCE_KEY],
            at(LAST_WRITE).to_rfc3339()
        );
        let detail = sent[0].1["metadata"]["detail"].as_str().unwrap();
        assert!(detail.contains("no OPEN work-session"), "{detail}");
    }

    const RUN_ID: &str = "9cf1e1ff-865c-4c5e-9ae5-1b7beed4ef26";

    /// An open agent-run of the factory as the open-runs read lists it,
    /// with its dev-build worker bound and its receipt recorded — the
    /// shape run 9cf1e1ff carried on 2026-10-29 at 01:58Z.
    fn building_run(worker_retired: bool) -> Json {
        json!({
            "id": RUN_ID, "kind": "agent-run", "status": "open",
            "metadata": {
                "agent": CODEX, "packet": "b111a1df",
                "worker_session": {"run": RUN_ID, "schema": 1, "session": "dev-build-23960b566d994c37"},
                "worker_started": {"actor": CODEX, "run": RUN_ID, "schema": 1},
                "worker_retirement": if worker_retired {
                    json!({"run": RUN_ID, "session": "dev-build-23960b566d994c37"})
                } else {
                    Json::Null
                },
            },
        })
    }

    /// What "live" reads, and what it refuses: a receipt or a binding
    /// naming another run, a missing one, or a retirement.
    #[test]
    fn a_worker_is_live_only_with_its_own_receipt_and_binding_and_no_retirement() {
        assert!(worker_live(&building_run(false)));
        assert!(!worker_live(&building_run(true)), "retired");
        let mut other = building_run(false);
        other["metadata"]["worker_started"]["run"] = json!("another-run");
        assert!(!worker_live(&other), "a receipt for another run");
        let mut unbound = building_run(false);
        unbound["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("worker_session");
        assert!(!worker_live(&unbound), "no binding");
        let mut legacy = building_run(false);
        legacy["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("worker_started");
        assert!(!worker_live(&legacy), "no receipt");
    }

    /// A holder with no session at all files at once, records the
    /// silence from now (so its first write after withdraws it), and says
    /// it has no session to be heard on.
    #[test]
    fn a_holder_with_no_session_at_all_files_an_unheard_alarm() {
        let now = plus(LAST_WRITE, 10, 0);
        assert_eq!(
            judge(&[], &held(), None, false, None, now, &b()),
            Act::FileUnheard
        );
        let heard = Heard {
            reading: None,
            session_open: false,
            worker_bound: false,
            restarted: false,
        };
        let body = alarm_body(CODEX, heard, &held(), &b(), "emp-owner", now, "a-rule");
        assert_eq!(body["metadata"][SINCE_KEY], now.to_rfc3339());
        assert_eq!(body["metadata"]["session"], Json::Null);
        let title = body["title"].as_str().unwrap();
        assert!(title.contains("no work-session"), "{title}");
        assert_eq!(
            judge(
                &[],
                &held(),
                Some(&moved_at(now - Duration::minutes(5))),
                false,
                None,
                now,
                &b()
            ),
            Act::Hold,
            "a moving actor is not alarmed for want of a session"
        );
        assert_eq!(
            judge(&[], &held(), None, true, None, now, &b()),
            Act::FileUnheard,
            "a bound worker defers from the actor's last write, and there is none to measure from"
        );
    }

    /// The deferral, pure: a bound worker stretches the bound to twice
    /// itself from the actor's last own write — 89 minutes holds, 90
    /// files — and a restart note newer than that write takes the
    /// deferral away, while one OLDER than it does not.
    #[test]
    fn a_bound_worker_defers_to_twice_the_bound_and_a_newer_restart_note_removes_it() {
        let r = read_measured();
        let one = std::slice::from_ref(&r);
        assert_eq!(
            judge(
                one,
                &held(),
                None,
                true,
                None,
                plus(LAST_WRITE, 89, 59),
                &b()
            ),
            Act::Hold
        );
        assert_eq!(
            judge(
                one,
                &held(),
                None,
                true,
                None,
                plus(LAST_WRITE, 90, 0),
                &b()
            ),
            Act::File(r.clone())
        );
        let mut noted = r.clone();
        noted.restart = Some(plus(LAST_WRITE, 5, 0));
        assert_eq!(
            judge(
                std::slice::from_ref(&noted),
                &held(),
                None,
                true,
                None,
                plus(LAST_WRITE, 45, 0),
                &b()
            ),
            Act::File(noted.clone())
        );
        let mut older = r.clone();
        older.restart = Some(plus(LAST_WRITE, -5, 0));
        assert_eq!(
            judge(
                std::slice::from_ref(&older),
                &held(),
                None,
                true,
                None,
                plus(LAST_WRITE, 45, 0),
                &b()
            ),
            Act::Hold
        );
    }

    /// B2: a build longer than the bound is the factory working while its
    /// dev-build worker is live. Measured: run 909a90a9 built this car
    /// for 59 minutes before its gate with no step completing, and an
    /// urgent SILENT alarm mid-build told the conductor to release a
    /// healthy factory's claim.
    #[tokio::test]
    async fn a_healthy_build_longer_than_the_bound_with_a_live_worker_is_not_alarmed() {
        let stub = listing_stub::serve(world(
            vec![],
            measured_window(),
            vec![building_run(false)],
            vec![session_row("open")],
            vec![],
        ))
        .await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.sent());
    }

    /// B2's other half: a run with no live worker whose branch was gated
    /// inside the bound is moving too.
    #[tokio::test]
    async fn a_run_whose_branch_was_gated_inside_the_bound_is_moving() {
        let gate = json!({
            "id": "a1b2c3d4-0000-4000-8000-000000000001", "kind": "gate-run", "status": "open",
            "opened_at": "2026-10-28T21:30:00Z",
            "metadata": {"agent_run": RUN_ID, "branch": "feat/x", "sha": "abc",
                         "launched_at": "2026-10-28T21:31:00Z"},
        });
        let stub = listing_stub::serve(world(
            vec![],
            measured_window(),
            vec![building_run(true)],
            vec![session_row("open")],
            vec![gate],
        ))
        .await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.sent());
    }

    /// The control beside B2: the same run whose worker was retired and
    /// whose branch has not moved IS alarmed — and the alarm sends the
    /// conductor to check the worker and the branch before releasing
    /// anything, never straight to `boss step release`.
    #[tokio::test]
    async fn a_run_whose_worker_is_gone_is_alarmed_and_the_alarm_says_check_first() {
        let stub = listing_stub::serve(world(
            vec![],
            measured_window(),
            vec![building_run(true)],
            vec![session_row("open")],
            vec![],
        ))
        .await;
        handler(&stub).invoke(&args(), &ctx(NOW)).await.unwrap();
        let sent = filings(&stub);
        assert_eq!(sent.len(), 1, "{sent:?}");
        let detail = sent[0].1["metadata"]["detail"].as_str().unwrap();
        assert!(
            !detail.contains("carry the urgent builds it holds"),
            "the alarm must not send the conductor to release claims: {detail}"
        );
        assert!(
            detail.contains("worker") && detail.contains("branch"),
            "check worker liveness and branch movement first: {detail}"
        );
        assert!(
            detail.contains("release only a run whose worker is gone"),
            "{detail}"
        );
    }

    /// B1's other half: infra/dev/factory-loop.sh writes `last_active_at`
    /// at every turn start and end, so the six-hour age-out never closes
    /// a live factory's session. That heartbeat is the SUPERVISOR's — it
    /// rides with a `factory_turn` key — and must not read as the factory
    /// speaking, or a loop relaunching a limited CLI every 30 minutes
    /// would hold the alarm off forever.
    #[test]
    fn the_loops_turn_heartbeat_is_not_the_factory_speaking() {
        let mut window = measured_window();
        let mut md = window.last().unwrap()["payload"]["metadata"].clone();
        md["last_active_at"] = json!("2026-10-28T21:40:00Z");
        md["factory_turn"] = json!({"turn": 7, "phase": "start"});
        window.push(update(CODEX_ALIAS, "2026-10-28T21:40:00Z", md));
        let r = reading(SESSION, &window, &codex(), &b()).unwrap();
        assert_eq!(r.since, at(LAST_WRITE), "{r:?}");
    }

    /// The control beside it: the Claude prompt hook's heartbeat
    /// (`last_active_at` and `prompt_count`) IS a session being prompted.
    #[test]
    fn a_prompt_hooks_heartbeat_is_the_actor_speaking() {
        let mut window = measured_window();
        let mut md = window.last().unwrap()["payload"]["metadata"].clone();
        md["last_active_at"] = json!("2026-10-28T21:40:00Z");
        md["prompt_count"] = json!(12);
        window.push(update(CODEX_ALIAS, "2026-10-28T21:40:00Z", md));
        let r = reading(SESSION, &window, &codex(), &b()).unwrap();
        assert_eq!(r.since, at("2026-10-28T21:40:00Z"), "{r:?}");
        assert!(r.exact);
    }

    // ---- review 1c7ab966 (CHANGES at 66ca3e33): the real stalls,
    // ---- replayed (decided_2026_10_29_factory_watch_repair_2 on 628dc540).
    //
    // The fixtures below are the live record, read through boss-api GET
    // by that review and kept in its run directory: each session window
    // is the 25 newest events of the session's own audit slice at the
    // tick (instant, signer, and the top-level keys each event changed),
    // and each run is its listing row as it stood at the tick.

    /// The factory's session on the morning of 2026-10-28.
    const MORNING_SESSION: &str = "d43a79c5-5d2b-404a-a694-6a949e048825";
    /// The run it held all morning: opened 06:28:57Z, its dev-build
    /// worker bound at 06:31:29Z and never retired while it was open; it
    /// died at 11:00:25Z, completed by the run-died rule.
    const MORNING_RUN: &str = "320b62e1-056b-43c5-968e-3e82cd8131a6";
    /// The factory's last own write before it stopped on a model limit.
    const MORNING_LAST_WRITE: &str = "2026-10-28T06:42:08.977222Z";

    /// d43a79c5's 25 newest events at 08:15Z: `(at, signer, keys)`.
    const MORNING: [(&str, &str, &str); 25] = [
        (
            "2026-10-28T03:36:38.887235Z",
            CODEX,
            "builder_cycle_20261028T0336Z,last_active_at",
        ),
        (
            "2026-10-28T03:49:54.752585Z",
            CODEX,
            "builder_cycle_20261028T0350Z,last_active_at",
        ),
        (
            "2026-10-28T03:57:27.802295Z",
            "agent-claude",
            "conductor_order_add_20261028T0357Z",
        ),
        (
            "2026-10-28T04:03:40.258497Z",
            CODEX,
            "builder_cycle_20261028T0404Z,last_active_at",
        ),
        (
            "2026-10-28T04:09:29.178510Z",
            CODEX,
            "builder_cycle_20261028T0410Z,last_active_at",
        ),
        (
            "2026-10-28T04:16:33.664798Z",
            "agent-claude",
            "conductor_order_add_20261028T0416Z",
        ),
        (
            "2026-10-28T04:23:29.450963Z",
            CODEX,
            "builder_cycle_20261028T0423Z,last_active_at",
        ),
        (
            "2026-10-28T04:28:20.470951Z",
            CODEX,
            "builder_cycle_20261028T0428Z,last_active_at",
        ),
        (
            "2026-10-28T04:38:07.160306Z",
            "agent-claude",
            "conductor_order_add_20261028T0438Z",
        ),
        (
            "2026-10-28T04:46:14.695821Z",
            CODEX,
            "builder_cycle_20261028T0446Z,last_active_at",
        ),
        (
            "2026-10-28T05:05:02.662185Z",
            CODEX,
            "handoff_20261028_0506Z_from_agent_codex",
        ),
        (
            "2026-10-28T05:20:03.648315Z",
            CODEX,
            "handoff_20261028_0520Z_relief_FULL_from_agent_codex",
        ),
        (
            "2026-10-28T05:26:40.282897Z",
            CODEX,
            "builder_admission_250_20261028T0527Z",
        ),
        (
            "2026-10-28T05:36:07.755361Z",
            CODEX,
            "builder_heartbeat_20261028T0536Z",
        ),
        (
            "2026-10-28T05:41:37.499865Z",
            CODEX,
            "builder_250_prepublish_conservation_gap_20261028T0542Z",
        ),
        (
            "2026-10-28T05:56:27.832840Z",
            CODEX,
            "builder_heartbeat_20261028T0557Z",
        ),
        (
            "2026-10-28T06:05:04.048389Z",
            CODEX,
            "builder_GREEN_review_request_8a_20261028T0605Z",
        ),
        (
            "2026-10-28T06:17:07.563441Z",
            CODEX,
            "builder_250_FULL_submission_20261028T0617Z",
        ),
        (
            "2026-10-28T06:26:21.808692Z",
            "agent-claude",
            "conductor_order_add_20261028T0626Z",
        ),
        (
            "2026-10-28T06:26:55.122803Z",
            CODEX,
            "builder_review_changes_ready_20261028T0627Z",
        ),
        (
            "2026-10-28T06:30:56.372493Z",
            CODEX,
            "builder_urgent_relief_repair_admitted_20261028T0631Z",
        ),
        (
            "2026-10-28T06:33:22.405667Z",
            "agent-claude",
            "conductor_decision_20261028T0633Z",
        ),
        (
            MORNING_LAST_WRITE,
            CODEX,
            "builder_parallel_factory_20261028T0642Z",
        ),
        (
            "2026-10-28T07:09:37.695196Z",
            "agent-claude",
            "conductor_order_add_20261028T0709Z",
        ),
        (
            "2026-10-28T07:23:44.964381Z",
            "agent-claude",
            "conductor_order_add_20261028T0723Z",
        ),
    ];

    /// The factory's session that evening.
    const EVENING_SESSION: &str = SESSION;
    /// Its last own write before the 23:37Z stall.
    const EVENING_LAST_WRITE: &str = "2026-10-28T23:37:33.299147Z";

    /// 243c9e91's 25 newest events at 00:25Z on 2026-10-29.
    const EVENING: [(&str, &str, &str); 25] = [
        (
            "2026-10-28T19:27:31.800514Z",
            CODEX,
            "builder_ready_20261028T1928Z",
        ),
        (
            "2026-10-28T19:30:32.151537Z",
            CODEX,
            "conductor_order_add_20261028T1926Z,order_copy_provenance_1926",
        ),
        (
            "2026-10-28T19:37:50.666204Z",
            CODEX,
            "builder_factory_cycle_20261028T1939Z",
        ),
        (
            "2026-10-28T19:46:50.715562Z",
            CODEX,
            "builder_factory_cycle_20261028T1947Z",
        ),
        (
            "2026-10-28T19:53:04.073097Z",
            CODEX,
            "builder_factory_cycle_20261028T1954Z",
        ),
        (
            "2026-10-28T19:57:36.298994Z",
            CODEX,
            "builder_factory_cycle_20261028T1958Z",
        ),
        (
            "2026-10-28T20:03:08.498989Z",
            CODEX,
            "handoff_20261028_2003Z",
        ),
        (
            "2026-10-28T20:09:17.002853Z",
            CODEX,
            "handoff_20261028_2009Z",
        ),
        (
            "2026-10-28T20:10:56.266516Z",
            CODEX,
            "handoff_20261028_2011Z",
        ),
        (
            "2026-10-28T20:21:58.598289Z",
            CODEX,
            "handoff_20261028_2021Z",
        ),
        (
            "2026-10-28T20:23:21.594496Z",
            "agent-claude",
            "conductor_order_add_20261028T2023Z",
        ),
        (
            "2026-10-28T20:23:42.271863Z",
            CODEX,
            "handoff_20261028_2023Z",
        ),
        (
            "2026-10-28T20:29:31.105111Z",
            CODEX,
            "handoff_20261028_2029Z",
        ),
        (
            "2026-10-28T20:34:45.024548Z",
            CODEX,
            "handoff_20261028_2034Z",
        ),
        (
            "2026-10-28T20:35:46.554559Z",
            CODEX,
            "stable_plan_independent_review_admission_20261028T2035Z",
        ),
        (
            "2026-10-28T20:38:45.978236Z",
            CODEX,
            "handoff_20261028_2038Z",
        ),
        (
            "2026-10-28T20:42:12.942422Z",
            CODEX,
            "handoff_20261028_2042Z",
        ),
        (
            "2026-10-28T20:58:00.193681Z",
            CODEX,
            "pvc_independent_review_target_20261028T2057Z",
        ),
        (
            "2026-10-28T21:33:52.287562Z",
            "agent-claude",
            "conductor_order_add_20261028T2133Z",
        ),
        (
            "2026-10-28T21:50:13.019827Z",
            "agent-claude",
            "conductor_order_add_20261028T2150Z",
        ),
        (
            "2026-10-28T22:42:16.094868Z",
            "agent-claude",
            "conductor_order_add_20261028T2240Z",
        ),
        (
            "2026-10-28T22:44:47.865545Z",
            CODEX,
            "handoff_20261028_2245Z",
        ),
        (
            "2026-10-28T23:26:15.608826Z",
            "agent-claude",
            "conductor_order_add_20261028T2326Z",
        ),
        (EVENING_LAST_WRITE, CODEX, "handoff_20261028_2339Z"),
        (
            "2026-10-28T23:49:08.726802Z",
            "agent-claude",
            "conductor_order_add_20261028T2349Z",
        ),
    ];

    /// A session window rebuilt from `(at, signer, keys)` rows: each row
    /// writes its keys (comma-separated) with its own instant as the
    /// value, onto the metadata the rows before it left, as the job's
    /// whole metadata rides on every `jobs.job.updated` event.
    fn replayed(session: &str, rows: &[(&str, &str, &str)]) -> Vec<Json> {
        let mut metadata = json!({"actor": CODEX});
        let mut events = Vec::new();
        for (at, signer, keys) in rows {
            for k in keys.split(',') {
                metadata[k] = json!({"at": at});
            }
            events.push(json!({
                "kind": "jobs.job.updated", "actor": signer, "at": at, "job_id": session,
                "step_id": null,
                "payload": {"_actor": signer, "id": session, "metadata": metadata.clone()},
            }));
        }
        events
    }

    /// An open agent-run of the factory as the open-runs read lists it:
    /// its worker receipt and binding recorded for it, and — `retired` —
    /// a retirement recorded beside them.
    fn factory_run(id: &str, packet: &str, worker: &str, retired: bool) -> Json {
        let mut run = json!({
            "id": id, "kind": "agent-run", "status": "open",
            "metadata": {
                "agent": CODEX, "packet": packet,
                "worker_session": {"run": id, "schema": 1, "session": worker},
                "worker_started": {"actor": CODEX, "run": id, "schema": 1},
            },
        });
        if retired {
            run["metadata"]["worker_retirement"] = json!({"run": id, "session": worker});
        }
        run
    }

    /// A run as the `step_completed_since` read lists it: the server
    /// matches it because ANY of its steps completed at or past the
    /// cutoff, whoever completed it. `steps` is `(slug, completed_at,
    /// completed_by)`.
    fn listed_run(id: &str, status: &str, steps: &[(&str, &str, Option<&str>)]) -> Json {
        json!({
            "id": id, "kind": "agent-run", "status": status,
            "metadata": {"agent": CODEX},
            "steps": steps.iter().map(|(slug, at, by)| json!({
                "spec_slug": slug, "status": "completed", "completed_at": at,
                "completed_by": by, "slim": true,
            })).collect::<Vec<_>>(),
        })
    }

    /// A run that LANDED on its gate's green: every step after `briefed`
    /// completed by automation (the record of 038f261a and c1d57ed2).
    fn landed_run(id: &str, briefed: &str, landed: [&str; 3]) -> Json {
        listed_run(
            id,
            "closed",
            &[
                ("claimed", briefed, None),
                ("briefed", briefed, Some(CODEX)),
                (
                    "building",
                    landed[0],
                    Some("automation:rule:agent-run-lands-on-gate-green"),
                ),
                (
                    "reported",
                    landed[1],
                    Some("automation:rule:agent-run-lands-a-report-sent-before-green"),
                ),
                (
                    "landed",
                    landed[2],
                    Some("automation:rule:complete-marker-on-step-ready"),
                ),
            ],
        )
    }

    /// The shipped declaration: the bound, the supervisor prefixes, and
    /// the prefix of the loop's restart note.
    fn shipped_args() -> Vec<(String, Value)> {
        let mut a = args();
        a.push((
            "restart_key_prefixes".into(),
            Value::String("factory_restart_".into()),
        ));
        a
    }

    /// The far side at `now`: one open session and its window, the open
    /// runs, no gate-run, and what the `step_completed_since` read at
    /// `now` minus the bound answers — and at `now` minus twice the
    /// bound, how far back an actor with a bound worker is read.
    fn replay(
        now: &str,
        session: &'static str,
        events: Vec<Json>,
        runs: Vec<Json>,
        completed: Vec<Json>,
    ) -> Vec<(&'static str, Json)> {
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        let cutoff = query(&[(
            "step_completed_since",
            &zulu(at(now) - Duration::minutes(45)),
        )]);
        let deferred = query(&[(
            "step_completed_since",
            &zulu(at(now) - Duration::minutes(90)),
        )]);
        vec![
            (
                "/api/agents",
                listing(vec![json!({"id": CODEX, "aliases": [CODEX_ALIAS]})]),
            ),
            ("/api/jobs?kind=agent-run&status=open", listing(runs)),
            ("/api/jobs?kind=gate-run", listing(vec![])),
            (
                "/api/jobs?kind=backlog-item&status=open&metadata_has=silent_session_actor",
                listing(vec![]),
            ),
            (
                leak(format!("/api/jobs?kind=agent-run&{deferred}")),
                listing(completed.clone()),
            ),
            (
                leak(format!("/api/jobs?kind=agent-run&{cutoff}")),
                listing(completed),
            ),
            ("/api/jobs/assignments", listing(vec![])),
            (
                "/api/jobs?kind=work-session&status=open",
                listing(vec![
                    json!({"id": session, "status": "open", "metadata": {"actor": CODEX}}),
                ]),
            ),
            (
                leak(format!("/api/jobs/{session}/events")),
                json!({"data": events}),
            ),
        ]
    }

    async fn tick(routes: Vec<(&'static str, Json)>, now: &str) -> Vec<(String, Json)> {
        let stub = listing_stub::serve(routes).await;
        handler(&stub)
            .invoke(&shipped_args(), &ctx(now))
            .await
            .unwrap();
        filings(&stub)
    }

    /// What a tick wrote to the JOBS API. The owner's inbox message
    /// rides beside every filing (35dabc3d) and is pinned by its own
    /// tests, so the filing judgements read only the filings.
    fn filings(stub: &listing_stub::Stub) -> Vec<(String, Json)> {
        stub.sent()
            .into_iter()
            .filter(|(path, _)| path != "POST /api/messages/send")
            .collect()
    }

    fn morning_run() -> Json {
        factory_run(
            MORNING_RUN,
            "8a1d0c74-2000-45e6-b1ba-abdc0d5e53e6",
            "dev-build-27645a528f72457f",
            false,
        )
    }

    /// Finding 1, the car's own motivating stall: agent-codex stopped at
    /// a model limit after its write at 06:42:08Z, holding run 320b62e1
    /// whose worker was bound and never retired. A bound worker defers
    /// the alarm for at most twice the bound from the actor's own last
    /// write — so it holds at 08:10Z (88 minutes) and FILES at the 08:15Z
    /// tick (93 minutes). Before, the bound worker held it until the run
    /// died at 11:00:25Z, and the run's own death held it 45 more
    /// minutes: the earliest tick that could file was 11:50Z.
    #[tokio::test]
    async fn the_morning_stall_with_a_bound_worker_files_by_ninety_minutes() {
        let window = replayed(MORNING_SESSION, &MORNING);
        let held = tick(
            replay(
                "2026-10-28T08:10:00Z",
                MORNING_SESSION,
                window.clone(),
                vec![morning_run()],
                vec![],
            ),
            "2026-10-28T08:10:00Z",
        )
        .await;
        assert!(
            held.is_empty(),
            "88 minutes is inside the deferral: {held:?}"
        );
        let sent = tick(
            replay(
                "2026-10-28T08:15:00Z",
                MORNING_SESSION,
                window,
                vec![morning_run()],
                vec![],
            ),
            "2026-10-28T08:15:00Z",
        )
        .await;
        assert_eq!(sent.len(), 1, "files by 08:15Z: {sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        let md = &sent[0].1["metadata"];
        assert_eq!(md[SINCE_KEY], at(MORNING_LAST_WRITE).to_rfc3339());
        assert_eq!(md["session"], MORNING_SESSION);
        assert_eq!(
            md["last_note_keys"],
            json!(["builder_parallel_factory_20261028T0642Z"])
        );
    }

    /// Decision B1's override: under the loop, a `factory_restart_*` note
    /// newer than the actor's last own write is the supervisor saying the
    /// CLI itself is down, so a bound worker no longer defers the alarm —
    /// it files at the bound. The same window without the note holds.
    #[tokio::test]
    async fn a_restart_note_newer_than_the_last_write_overrides_a_bound_worker() {
        let now = "2026-10-28T07:30:00Z";
        let mut rows: Vec<(&str, &str, &str)> = MORNING
            .iter()
            .copied()
            .filter(|(t, _, _)| *t <= now)
            .collect();
        let quiet = replayed(MORNING_SESSION, &rows);
        let held = tick(
            replay(now, MORNING_SESSION, quiet, vec![morning_run()], vec![]),
            now,
        )
        .await;
        assert!(held.is_empty(), "a bound worker defers: {held:?}");
        let note = rows
            .iter()
            .position(|(t, _, _)| *t > MORNING_LAST_WRITE)
            .unwrap_or(rows.len());
        rows.insert(
            note,
            (
                "2026-10-28T07:00:12Z",
                CODEX,
                "factory_restart_20261028T070012Z",
            ),
        );
        let window = replayed(MORNING_SESSION, &rows[rows.len() - EVENT_WINDOW..]);
        let sent = tick(
            replay(now, MORNING_SESSION, window, vec![morning_run()], vec![]),
            now,
        )
        .await;
        assert_eq!(sent.len(), 1, "the restart note overrides: {sent:?}");
        assert_eq!(
            sent[0].1["metadata"][SINCE_KEY],
            at(MORNING_LAST_WRITE).to_rfc3339(),
            "the restart note is the supervisor's, never the factory speaking"
        );
    }

    /// Finding 2: a run's step completed by AUTOMATION is not the actor
    /// moving. Measured: at the 21:45Z tick of the 20:58Z stall the
    /// `step_completed_since` read answered 038f261a and c1d57ed2, both
    /// landed by automation:rule:agent-run-lands-on-gate-green at 21:08Z
    /// and 21:10Z, and the alarm held to 22:00Z.
    #[tokio::test]
    async fn an_automation_completion_inside_the_bound_does_not_hold_the_alarm() {
        let now = NOW;
        let landings = vec![
            landed_run(
                "038f261a-6445-4653-80a6-cf426429f772",
                "2026-10-28T19:43:55.236280Z",
                [
                    "2026-10-28T21:08:33.979283Z",
                    "2026-10-28T21:08:34.313613Z",
                    "2026-10-28T21:08:34.570309Z",
                ],
            ),
            landed_run(
                "c1d57ed2-968b-46e6-b42d-090a85f2101e",
                "2026-10-28T19:33:46.948927Z",
                [
                    "2026-10-28T21:10:07.004666Z",
                    "2026-10-28T21:10:09.439304Z",
                    "2026-10-28T21:10:10.095393Z",
                ],
            ),
        ];
        let open = factory_run(
            "4ba1e76a-c154-41a9-a9b0-37f4d3dae0b9",
            "6bc062b1-8d49-464f-abe1-6912c7d25fa3",
            "dev-build-2214a62f1629486f",
            true,
        );
        let sent = tick(
            replay(now, SESSION, measured_window(), vec![open], landings),
            now,
        )
        .await;
        assert_eq!(sent.len(), 1, "automation is not the factory: {sent:?}");
        assert_eq!(
            sent[0].1["metadata"][SINCE_KEY],
            at(LAST_WRITE).to_rfc3339()
        );
    }

    /// The control beside it: a step the ACTOR completed inside the bound,
    /// under either of its spellings, is the actor moving and holds.
    #[tokio::test]
    async fn an_own_completion_inside_the_bound_holds_the_alarm() {
        for who in [CODEX, CODEX_ALIAS] {
            let run = listed_run(
                "9544c2b4-b359-4789-a499-a4aeda9317e1",
                "open",
                &[
                    ("claimed", "2026-10-28T21:20:00Z", None),
                    ("briefed", "2026-10-28T21:20:01Z", Some(who)),
                ],
            );
            let open = factory_run(
                "9544c2b4-b359-4789-a499-a4aeda9317e1",
                "b111a1df",
                "dev-build-0000000000000000",
                true,
            );
            let sent = tick(
                replay(NOW, SESSION, measured_window(), vec![open], vec![run]),
                NOW,
            )
            .await;
            assert!(sent.is_empty(), "{who} moved: {sent:?}");
        }
    }

    /// The 23:37Z stall, replayed, still files at the bound: its runs'
    /// workers were retired by codex's own report verb (49623134 at
    /// 00:06:03Z, 6ee1b2f4 at 00:08:54Z), so nothing defers it. At 00:20Z
    /// it holds (42 minutes, and its runs' `briefed` steps, completed by
    /// codex at 23:38Z, are inside the bound); at 00:25Z it files, 47
    /// minutes after handoff_20261028_2339Z. A control: it held on
    /// 66ca3e33 too, and the deferral must not move it.
    #[tokio::test]
    async fn the_late_evening_stall_still_files_at_forty_five_minutes() {
        let window = replayed(EVENING_SESSION, &EVENING);
        let runs = || {
            vec![
                factory_run(
                    "49623134-01d1-4959-9aa9-35df5d85e2d6",
                    "b111a1df-31f6-43df-9842-b4329ba93491",
                    "dev-build-f3b30d2f121641e1",
                    true,
                ),
                factory_run(
                    "6ee1b2f4-10fe-4ce7-ab30-7d06e546044f",
                    "7a022636-d553-416e-9fb8-a886517f7700",
                    "dev-build-c42e09ed1482499c",
                    true,
                ),
            ]
        };
        let briefed = vec![
            listed_run(
                "49623134-01d1-4959-9aa9-35df5d85e2d6",
                "open",
                &[
                    ("claimed", "2026-10-28T23:38:47.220853Z", None),
                    ("briefed", "2026-10-28T23:38:47.349737Z", Some(CODEX)),
                ],
            ),
            listed_run(
                "6ee1b2f4-10fe-4ce7-ab30-7d06e546044f",
                "open",
                &[
                    ("claimed", "2026-10-28T23:38:12.518319Z", None),
                    ("briefed", "2026-10-28T23:38:14.583858Z", Some(CODEX)),
                ],
            ),
        ];
        let at_0020 = "2026-10-29T00:20:00Z";
        let held = tick(
            replay(at_0020, EVENING_SESSION, window.clone(), runs(), briefed),
            at_0020,
        )
        .await;
        assert!(held.is_empty(), "{held:?}");
        let at_0025 = "2026-10-29T00:25:00Z";
        let sent = tick(
            replay(at_0025, EVENING_SESSION, window, runs(), vec![]),
            at_0025,
        )
        .await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        let md = &sent[0].1["metadata"];
        assert_eq!(md[SINCE_KEY], at(EVENING_LAST_WRITE).to_rfc3339());
        assert_eq!(md["silent_minutes"], 47);
        assert_eq!(md["last_note_keys"], json!(["handoff_20261028_2339Z"]));
    }

    /// The restart prefix lives in the rule twice (it is ignored as
    /// speech AND it overrides a bound worker), so a restart prefix the
    /// rule does not also ignore is refused by name: such a note would
    /// read as the factory speaking and could never be newer than its
    /// last write.
    #[tokio::test]
    async fn a_restart_prefix_the_rule_does_not_ignore_is_refused() {
        let stub = listing_stub::serve(stub_routes(vec![], measured_window())).await;
        let mut a = vec![
            ("silent_minutes".into(), Value::Int(45)),
            (
                "ignore_key_prefixes".into(),
                Value::String("factory_turn".into()),
            ),
        ];
        a.push((
            "restart_key_prefixes".into(),
            Value::String("factory_restart_".into()),
        ));
        let res = handler(&stub).invoke(&a, &ctx(NOW)).await;
        match res {
            Err(HandlerError::Permanent(why)) => {
                assert!(why.contains("factory_restart_"), "{why}")
            }
            other => panic!("expected a permanent refusal, got {other:?}"),
        }
        assert!(stub.writes().is_empty(), "{:?}", stub.sent());
    }

    // ---- backlog 7de4cc46: an actor that works with no open session ----

    /// The ended session the measured alarm named.
    const ENDED_SESSION: &str = "2a0c73be-cb07-40b0-9551-8ae7b0682619";
    /// What that alarm recorded as `silent_since`: the oldest of the 25
    /// newest events on that session, every one of them another actor's
    /// peer message — a FLOOR, not a write of codex's.
    const ENDED_FLOOR: &str = "2026-11-11T23:56:45.681628Z";
    /// The run codex was building when the alarm filed at 15:55Z.
    const RELAY_RUN: &str = "2c28f32d-f8d5-4bff-a49e-5cd0b0937601";

    /// The far side of the measured 7de4cc46 case at `now`: agent-codex
    /// has NO open work-session — its newest, 2a0c73be, was aged out on
    /// 2026-11-10 and since then took only other actors' peer messages, so
    /// its window is a full window of them and reads as a floor at
    /// [`ENDED_FLOOR`]. Codex holds run 2c28f32d with its worker bound;
    /// `completed` is what the movement read answers, `alarms` the open
    /// alarms of this rule.
    fn ended_session_world(
        now: &str,
        alarms: Vec<Json>,
        completed: Vec<Json>,
    ) -> Vec<(&'static str, Json)> {
        let stamps: Vec<String> = std::iter::once(ENDED_FLOOR.to_string())
            .chain((1..EVENT_WINDOW).map(|i| format!("2026-11-12T04:{i:02}:49Z")))
            .collect();
        let keys: Vec<String> = (0..EVENT_WINDOW)
            .map(|i| format!("peer_message_{i:02}"))
            .collect();
        let rows: Vec<(&str, &str, &str)> = stamps
            .iter()
            .zip(&keys)
            .map(|(t, k)| (t.as_str(), "agent-claude", k.as_str()))
            .collect();
        let window = replayed(ENDED_SESSION, &rows);
        let run = factory_run(
            RELAY_RUN,
            "6024b8da-ac5b-4f7f-88a7-69aa9f2ab1bc",
            "dev-build-2c28f32df8d54bff",
            false,
        );
        let mut routes: Vec<(&'static str, Json)> =
            replay(now, ENDED_SESSION, window, vec![run], completed)
                .into_iter()
                .map(|(route, body)| match route {
                    "/api/jobs?kind=backlog-item&status=open&metadata_has=silent_session_actor" => {
                        (route, listing(alarms.clone()))
                    }
                    "/api/jobs?kind=work-session&status=open" => (route, listing(vec![])),
                    _ => (route, body),
                })
                .collect();
        routes.push((
            "/api/jobs?kind=work-session",
            listing(vec![
                json!({"id": ENDED_SESSION, "status": "closed", "metadata": {"actor": CODEX}}),
            ]),
        ));
        routes
    }

    /// Run 2c28f32d as the movement read lists it at 16:00Z: codex
    /// briefed it at 15:06:43Z and completed its `building` at 15:58:20Z
    /// — three minutes after the alarm filed — and automation completed
    /// its verdict after that.
    fn relay_run_built() -> Json {
        listed_run(
            RELAY_RUN,
            "open",
            &[
                ("claimed", "2026-11-12T15:06:40Z", None),
                ("briefed", "2026-11-12T15:06:43Z", Some(CODEX)),
                ("building", "2026-11-12T15:58:20Z", Some(CODEX)),
                (
                    "verdict",
                    "2026-11-12T15:59:10Z",
                    Some("automation:rule:agent-run-lands-on-gate-green"),
                ),
            ],
        )
    }

    /// Backlog 7de4cc46, measured: the alarm filed at 15:55Z on a session
    /// floor, codex completed its own run's `building` at 15:58:20Z, and
    /// the alarm stood open for 10 h of landed work, because only a write
    /// on a session could withdraw it and codex had no open session to
    /// write to. The actor moving after the silence the alarm recorded
    /// withdraws it — the same read that keeps the watch from filing — and
    /// the withdrawal names what moved and when. Since f4b0ad41 the
    /// withdrawal is the RECOVERED note on an alarm held open.
    #[tokio::test]
    async fn an_open_alarm_is_withdrawn_when_its_actor_moves_after_the_silence() {
        let now = "2026-11-12T16:00:00Z";
        let sent = tick(
            ended_session_world(now, vec![open_alarm(ENDED_FLOOR)], vec![relay_run_built()]),
            now,
        )
        .await;
        assert_eq!(sent.len(), 1, "the note, and no step: {sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/al-1/metadata");
        assert_eq!(sent[0].1["recovered_by"], HANDLER);
        let evidence = sent[0].1["recovery"].as_str().unwrap();
        for said in [
            RELAY_RUN,
            "`building`",
            "2026-11-12T15:58:20",
            "2026-11-11T23:56:45",
        ] {
            assert!(evidence.contains(said), "names {said}: {evidence}");
        }
        assert!(
            !evidence.contains("verdict"),
            "automation's completion is not what moved: {evidence}"
        );
    }

    /// The controls beside it: the same open alarm STANDS when nothing of
    /// codex's moved, and when the only completion after the silence is
    /// automation's — a train landing a run is not the actor moving.
    #[tokio::test]
    async fn an_open_alarm_stands_when_only_automation_moved() {
        let now = "2026-11-12T16:00:00Z";
        let landed = listed_run(
            RELAY_RUN,
            "open",
            &[
                ("claimed", "2026-11-12T15:06:40Z", None),
                (
                    "verdict",
                    "2026-11-12T15:59:10Z",
                    Some("automation:rule:agent-run-lands-on-gate-green"),
                ),
            ],
        );
        for completed in [vec![], vec![landed]] {
            let sent = tick(
                ended_session_world(now, vec![open_alarm(ENDED_FLOOR)], completed),
                now,
            )
            .await;
            assert!(sent.is_empty(), "{sent:?}");
        }
    }

    /// Backlog 7de4cc46 (b), measured: at the 15:55Z tick codex's last own
    /// completion was 2c28f32d's `briefed` at 15:06:43Z, 48 minutes
    /// earlier, and its worker was bound — a build in flight. The bound
    /// worker's deferral was measured from the session floor, a day old,
    /// so it filed. Measured from the actor's own last completion it holds
    /// to 89:59 and files at 90:00, on the floor it read.
    #[tokio::test]
    async fn a_bound_worker_defers_from_the_actors_own_last_completion() {
        let briefed = || {
            listed_run(
                RELAY_RUN,
                "open",
                &[
                    ("claimed", "2026-11-12T15:06:40Z", None),
                    ("briefed", "2026-11-12T15:06:43Z", Some(CODEX)),
                ],
            )
        };
        for now in ["2026-11-12T15:55:00Z", "2026-11-12T16:36:42Z"] {
            let held = tick(ended_session_world(now, vec![], vec![briefed()]), now).await;
            assert!(held.is_empty(), "{now} is inside the deferral: {held:?}");
        }
        let now = "2026-11-12T16:36:43Z";
        let sent = tick(ended_session_world(now, vec![], vec![briefed()]), now).await;
        assert_eq!(sent.len(), 1, "90:00 after its own completion: {sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        assert_eq!(
            sent[0].1["metadata"][SINCE_KEY],
            at(ENDED_FLOOR).to_rfc3339()
        );
    }
}
