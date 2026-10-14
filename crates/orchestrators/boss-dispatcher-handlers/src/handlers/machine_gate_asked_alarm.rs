//! `machine.gate.asked.alarm` — the reader of the machine gate's `asked`
//! rows (backlog d653e090, the builder's note on review 02e21a45 of car
//! 176d5c9d).
//!
//! THE ROW IT READS. A GET of `/api/machine-gate/accepts` carrying one
//! value that matches no slot is an ASK (`machine_gate::asks`): tallied
//! and stated on the log as `asked`, naming peer and asserted user, and
//! deliberately no caller shape `enforce` refuses — a rotation asks every
//! gate about a staged value by design. So it dirties no window, holds no
//! revoke, and is said at INFO; and until this handler nothing read it.
//! A peer trying a stale host file, or a value out of a transcript, was
//! on the record and reached no one.
//!
//! WHO IS EXPECTED TO ASK, and so is not named:
//!
//! * the dispatcher itself — the asserted id [`DISPATCHER`] FROM
//!   LOOPBACK, which is where every rule actor reaches a gated port;
//! * a depositor the RULE names in its `depositors` arg — data, held to
//!   the automation registry by the pin
//!   `the_asked_alarm_excuses_only_registered_depositors`.
//!
//! Everyone else is a stranger: an unregistered id, no id at all, or the
//! dispatcher's id from anywhere but loopback.
//!
//! WHAT THIS CANNOT SEE, measured and said rather than implied. The
//! asserted user is ASSERTED: a caller that sends a depositor's id is
//! read as that depositor. And the peer cannot tell one LAN host from
//! another — in the 72 hours of facts read 2026-10-09 every caller from
//! outside the pod network arrived as ONE address (the node's, 1,398
//! facts under 24 asserted ids) — so a depositor is excused by id alone,
//! not by where it called from. This names the caller who does not know
//! the estate's ids. It is not a boundary, and it refuses nothing.
//!
//! WHERE IT READS. The protected durable window
//! (`/api/events/gate-window?gate=machine-gate`), not a port's live
//! tally alone: the tally lives in memory and every train restarts the
//! pod, so an ask is usually gone from `/api/machine-gate/misses` before
//! an hourly reader arrives, while its `machine_gate.asked` fact is not.
//! The live rows are read beside the facts for what a recorder lost.
//!
//! WHAT IT FILES. At most one open alarm per finding, keyed on
//! [`FINDING_KEY`] — never one per asker, because ids are asserted and a
//! caller can spray them; one packet lists them, bounded:
//!
//! * [`STRANGER`] — an ask from someone not expected, inside the last
//!   [`LOOKBACK_MINUTES`]. An ask is an EVENT, not a level, so the machine
//!   never withdraws this one: that it stopped is not evidence it was
//!   benign, and a person closes it at triage. A later stranger the open
//!   alarm does not list is added to it.
//! * [`UNREADABLE`] — the window could not be judged, said rather than
//!   swallowed. Withdrawn by the next whole read; the firing still fails.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use boss_core::gate_evidence::{Fact, Gate};
use boss_core::gate_window::GateObservation;
use boss_core::machine_gate::{Misses, Presented};
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value as Json, json};

use super::common::{
    RECOVERED_AT, Retraction, api_client, complete_step, dispatcher_actor_header, jobs_where,
    owner_for_filing, post_json, recovery_note, relapse_patch, retraction, with_lane,
    withdrawal_fields, write_json,
};

/// The handler's registered name.
pub const HANDLER: &str = "machine.gate.asked.alarm";

/// The packet-metadata key naming which finding an alarm is — the dedup
/// key.
pub const FINDING_KEY: &str = "machine_gate_asked_finding";

/// An ask from someone who is neither the dispatcher nor a depositor.
pub const STRANGER: &str = "asked-by-a-stranger";
/// The window could not be read as one.
pub const UNREADABLE: &str = "asked-window-unreadable";

/// The rule arg naming the depositors: one string, ids separated by
/// commas.
pub const DEPOSITORS_ARG: &str = "depositors";

/// The id every rule actor asserts when it reads a gate — held equal to
/// `common::dispatcher_reader_header` by a test.
pub const DISPATCHER: &str = "automation:dispatcher";

/// How far back one firing reads: two of the rule's hourly cadences, so
/// one late tick cannot step over an ask.
pub const LOOKBACK_MINUTES: i64 = 120;

/// How many askers one alarm's prose lists; the rest are counted.
const LISTED: usize = 12;
/// How many askers one alarm's `askers` key holds across firings.
const RECORDED: usize = 48;

/// The user an ask past its source's share is named under: the gate
/// counted it by source and kept no id.
pub const PAST_SHARE: &str = "(not keyed: past its source's share of asks)";
/// The peer an ask no key could name is listed under.
pub const UNNAMED: &str = "(unnamed: the room asks hold was full)";

fn window_path() -> String {
    format!(
        "{}?gate=machine-gate&hours={}",
        boss_core::gate_window::PATH,
        LOOKBACK_MINUTES / 60
    )
}

/// One asker, as the gates named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asker {
    pub peer: String,
    pub user: String,
    /// The ports it asked, by `boss_ports` name.
    pub services: BTreeSet<String>,
    pub last_seen: DateTime<Utc>,
    /// A lower bound: first sightings on the log, or the live rows'
    /// counts where a row still stands, whichever is more.
    pub at_least: u64,
}

impl Asker {
    /// `<peer> as <user>` — how an alarm's `askers` key spells one.
    pub fn label(&self) -> String {
        format!("{} as {}", self.peer, self.user)
    }
}

/// Is this asker one the estate expects? See the module doc for what the
/// answer can and cannot vouch for.
pub fn expected(peer: &str, user: &str, depositors: &[String]) -> bool {
    let loopback = peer
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.to_canonical().is_loopback());
    (user == DISPATCHER && loopback) || depositors.iter().any(|d| d == user)
}

/// Every stranger who asked inside the observation, by peer and user.
/// Refuses a fact or a live answer it cannot read, so a changed contract
/// is an unreadable window and never a quiet hour.
pub fn judge(obs: &GateObservation, depositors: &[String]) -> Result<Vec<Asker>, String> {
    // (first sightings on the log, live counts) per asker.
    let mut seen: BTreeMap<(String, String), (Asker, u64, u64)> = BTreeMap::new();
    let mut note = |peer: &str, user: &str, service: &str, at: DateTime<Utc>, live: u64| {
        let entry = seen
            .entry((peer.to_string(), user.to_string()))
            .or_insert_with(|| {
                (
                    Asker {
                        peer: peer.to_string(),
                        user: user.to_string(),
                        services: BTreeSet::new(),
                        last_seen: at,
                        at_least: 0,
                    },
                    0,
                    0,
                )
            });
        entry.0.services.insert(service.to_string());
        entry.0.last_seen = entry.0.last_seen.max(at);
        if live == 0 {
            entry.1 += 1;
        } else {
            entry.2 += live;
        }
    };

    let asked = Gate::MachineGate.kind(Fact::Asked);
    let overflowed = Gate::MachineGate.kind(Fact::TallyOverflowed);
    let facts = obs.facts.as_ref().map_err(Clone::clone)?;
    for event in facts.iter().filter(|e| e.timestamp >= obs.from) {
        let text = |v: &Json, k: &str| v.get(k).and_then(Json::as_str).map(str::to_string);
        if event.kind == asked {
            // One fact per key per process — or, past a source's share,
            // one per source, which keeps no id.
            match (event.payload.get("key"), text(&event.payload, "source")) {
                (Some(key), _) => {
                    let (Some(peer), Some(user)) = (text(key, "peer"), text(key, "user")) else {
                        return Err(format!(
                            "an asked fact from {} names no peer and user: {}",
                            event.source, event.payload
                        ));
                    };
                    note(&peer, &user, &event.source, event.timestamp, 0);
                }
                (None, Some(source)) => {
                    note(&source, PAST_SHARE, &event.source, event.timestamp, 0)
                }
                (None, None) => {
                    return Err(format!(
                        "an asked fact from {} carries neither a key nor a source: {}",
                        event.source, event.payload
                    ));
                }
            }
        } else if event.kind == overflowed
            && text(&event.payload, "presented").as_deref() == Some(Presented::Asked.name())
        {
            // An ask no key could name. Past a source's share the fact
            // still names the source (a tally overflow since design
            // e3d19d1a q1, an `asked` before it); past the whole room
            // nothing says who sent it.
            match text(&event.payload, "source").filter(|s| !s.is_empty()) {
                Some(source) => note(&source, PAST_SHARE, &event.source, event.timestamp, 0),
                None => note(UNNAMED, PAST_SHARE, &event.source, event.timestamp, 0),
            }
        }
    }
    // The live half. A port that did not answer hides no fact — the log
    // is the record — but one that answered out of shape is a changed
    // contract, and is refused.
    for read in &obs.reads {
        let Ok(answer) = &read.answer else { continue };
        let tally: Misses = serde_json::from_value(answer.clone()).map_err(|e| {
            format!(
                "{}'s live tally is not in the gate's shape: {e}",
                read.service
            )
        })?;
        for row in tally
            .rows
            .iter()
            .filter(|r| r.key.presented == Presented::Asked && r.last_seen >= obs.from)
        {
            note(
                &row.key.peer,
                &row.key.user,
                &read.service,
                row.last_seen,
                row.count.max(1),
            );
        }
        for o in tally.source_overflow.iter().filter(|o| {
            o.presented == Presented::Asked && o.last_seen.is_none_or(|t| t >= obs.from)
        }) {
            note(
                &o.source,
                PAST_SHARE,
                &read.service,
                o.last_seen.unwrap_or(obs.now),
                o.count.max(1),
            );
        }
    }
    Ok(seen
        .into_values()
        .map(|(mut asker, logged, live)| {
            asker.at_least = logged.max(live);
            asker
        })
        .filter(|a| !expected(&a.peer, &a.user, depositors))
        .collect())
}

fn depositors_arg(args: &[(String, Value)]) -> Result<Vec<String>, HandlerError> {
    let raw = match args.iter().find(|(k, _)| k == DEPOSITORS_ARG) {
        Some((_, Value::String(s))) => s,
        _ => {
            return Err(HandlerError::Permanent(format!(
                "{HANDLER}: the rule passes no `{DEPOSITORS_ARG}` string — the ids excused beside \
                 the dispatcher are the rule's to name, and an empty string names none"
            )));
        }
    };
    Ok(raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

fn body(finding: &str, title: String, detail: String, extra: Json, owner: &str) -> Json {
    let mut metadata = json!({
        "area": "credentials",
        "scope": "machine-gate",
        FINDING_KEY: finding,
        "detail": detail,
    });
    if let (Some(m), Json::Object(extra)) = (metadata.as_object_mut(), extra) {
        m.extend(extra);
    }
    json!({
        "kind": "backlog-item",
        "title": title,
        "subject": {"subject_kind": "custom", "id": finding},
        "owner_id": owner,
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

fn stranger_detail(strangers: &[Asker], depositors: &[String], now: &str, rule: &str) -> String {
    let named: Vec<String> = strangers
        .iter()
        .take(LISTED)
        .map(|a| {
            format!(
                "{} (asked {}, at least {} time(s), last {})",
                a.label(),
                a.services.iter().cloned().collect::<Vec<_>>().join(", "),
                a.at_least,
                a.last_seen.to_rfc3339()
            )
        })
        .collect();
    let more = strangers.len().saturating_sub(LISTED);
    let askers = if more > 0 {
        format!("{} — and {more} more", named.join("; "))
    } else {
        named.join("; ")
    };
    format!(
        "Raised by {HANDLER} (rule {rule}, backlog d653e090) at {now}: GET {window} holds an ASK \
         — a GET of /api/machine-gate/accepts presenting one value that matches no slot — inside \
         the last {LOOKBACK_MINUTES} minutes from a caller that is neither {DISPATCHER} on \
         loopback nor a depositor the rule names ({depositors}): {askers}. The value matched \
         NOTHING, so the ask admitted no one and read no slot; what it says is that someone holds \
         a value they believe is a machine token — a host file one rotation behind, a value out \
         of a transcript, or a depositor asking under no id (the probe-reader deposit sends none, \
         and asks in the minute between a reader rotation's stage and its projection). Ids here \
         are ASSERTED by the caller and every caller from outside the pod network arrives as the \
         node's address, so an asker named as a depositor is not listed and a peer does not name \
         a host. Read the row's port and time against the rotation packets open then. This alarm \
         refuses nothing and the machine does not withdraw it: an ask that stopped is not an ask \
         explained.",
        window = window_path(),
        depositors = depositors.join(", "),
    )
}

/// The alarm an ask from a stranger files.
pub fn stranger_body(
    strangers: &[Asker],
    depositors: &[String],
    owner: &str,
    now: &str,
    rule: &str,
) -> Json {
    body(
        STRANGER,
        format!(
            "ASK FROM A STRANGER: {} caller(s) asked a machine gate about a value no slot holds",
            strangers.len()
        ),
        stranger_detail(strangers, depositors, now, rule),
        json!({
            "askers": strangers.iter().take(RECORDED).map(Asker::label).collect::<Vec<_>>(),
            "asker_count": strangers.len(),
            "measured_at": now,
        }),
        owner,
    )
}

/// The alarm a dark read files, naming the error it got.
pub fn unreadable_body(error: &str, owner: &str, now: &str, rule: &str) -> Json {
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog d653e090) at {now}: GET {window} could not be \
         judged — {error}. While it stays so, an ask from a caller nobody expects is on the log \
         and read by no one; an alarm already open stays open. Withdrawn by the next whole read.",
        window = window_path(),
    );
    body(
        UNREADABLE,
        "MACHINE GATE ASKS UNREADABLE: nobody can see who asked about a value".to_string(),
        detail,
        json!({"last_error": error, "measured_at": now}),
        owner,
    )
}

/// Every OPEN alarm this handler filed, keyed by its finding.
pub fn alarms_by_finding(rows: &[Json]) -> BTreeMap<String, Json> {
    rows.iter()
        .filter(|r| r.get("status").and_then(Json::as_str) == Some("open"))
        .filter_map(|r| {
            let f = r
                .pointer(&format!("/metadata/{FINDING_KEY}"))
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())?;
            Some((f.to_string(), r.clone()))
        })
        .collect()
}

/// What an open stranger alarm must be told: `None` when it already
/// lists every asker of this reading, else the merge that adds them.
pub fn extension(alarm: &Json, strangers: &[Asker], detail: String, now: &str) -> Option<Json> {
    let listed: Vec<String> = alarm
        .pointer("/metadata/askers")
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Json::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let new: Vec<String> = strangers
        .iter()
        .map(Asker::label)
        .filter(|l| !listed.contains(l))
        .collect();
    if new.is_empty() || listed.len() >= RECORDED {
        return None;
    }
    let all: Vec<String> = listed.into_iter().chain(new).take(RECORDED).collect();
    Some(json!({
        "askers": all,
        "asker_count": all.len(),
        "detail": detail,
        "measured_at": now,
    }))
}

pub struct MachineGateAskedAlarm {
    client: boss_core::machine_token::Client,
    evidence_base: String,
    jobs_base: String,
    /// Every gated service, by `boss_ports` name: the roster the window
    /// must bind before it is an observation.
    roster: Vec<String>,
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    wall: Arc<dyn boss_core::clock::Clock>,
}

impl MachineGateAskedAlarm {
    pub fn new(
        evidence_base: impl Into<String>,
        jobs_base: impl Into<String>,
        roster: Vec<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Self::with_clock(
            evidence_base,
            jobs_base,
            roster,
            owner,
            Arc::new(boss_core::clock::WallClock),
        )
    }

    fn with_clock(
        evidence_base: impl Into<String>,
        jobs_base: impl Into<String>,
        roster: Vec<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
        wall: Arc<dyn boss_core::clock::Clock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            evidence_base: evidence_base.into().trim_end_matches('/').to_string(),
            jobs_base: jobs_base.into().trim_end_matches('/').to_string(),
            roster,
            owner,
            wall,
        })
    }

    async fn patch(&self, alarm: &Json, merge: &Json, rule: &str) -> Result<(), HandlerError> {
        let id = alarm.get("id").and_then(Json::as_str).unwrap_or_default();
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{}/api/jobs/{id}/metadata", self.jobs_base),
            merge,
            rule,
        )
        .await
    }

    async fn file(&self, body: &Json, rule: &str) -> Result<(), HandlerError> {
        post_json(
            &self.client,
            &format!("{}/api/jobs", self.jobs_base),
            body,
            rule,
        )
        .await
    }

    /// Withdraw the open UNREADABLE alarm, if there is one, at the step
    /// it waits on (or by a note when no step may be completed).
    async fn withdraw_unreadable(
        &self,
        filed: &BTreeMap<String, Json>,
        evidence: &str,
        now: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let Some(alarm) = filed.get(UNREADABLE) else {
            return Ok(());
        };
        let id = alarm.get("id").and_then(Json::as_str).unwrap_or_default();
        let recovered = alarm
            .pointer(&format!("/metadata/{RECOVERED_AT}"))
            .is_some_and(|v| !v.is_null());
        match retraction(alarm) {
            None => {
                tracing::warn!(rule, packet = %id, "{HANDLER}: the open alarm has no triage step to close");
                Ok(())
            }
            Some(Retraction::Complete { step_id, .. }) => {
                complete_step(
                    &self.client,
                    &self.jobs_base,
                    id,
                    &step_id,
                    withdrawal_fields(evidence, HANDLER, now),
                    rule,
                )
                .await
            }
            Some(Retraction::Annotate { .. }) if recovered => Ok(()),
            Some(Retraction::Annotate { why_open }) => {
                self.patch(
                    alarm,
                    &Json::Object(recovery_note(evidence, HANDLER, now, &why_open)),
                    rule,
                )
                .await
            }
        }
    }
}

#[async_trait]
impl Handler for MachineGateAskedAlarm {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let depositors = depositors_arg(args)?;

        // The dedup read first: a dark window still needs it, to keep its
        // one UNREADABLE alarm.
        let filed = alarms_by_finding(
            &jobs_where(
                &self.client,
                &self.jobs_base,
                &format!("kind=backlog-item&status=open&metadata_has={FINDING_KEY}"),
                rule,
            )
            .await?,
        );
        let owner = owner_for_filing(self.owner.as_ref(), rule).await;

        let before = self.wall.now();
        let read = match super::gate_window_read::read(
            &self.client,
            &format!("{}{}", self.evidence_base, window_path()),
            &dispatcher_actor_header(rule),
        )
        .await
        {
            Ok(snapshot) => super::gate_window_read::observation(
                &snapshot,
                Gate::MachineGate,
                &self.roster,
                Duration::minutes(LOOKBACK_MINUTES),
                before,
                self.wall.now(),
            )
            .and_then(|obs| judge(obs, &depositors).map(|s| (s, obs.now.to_rfc3339()))),
            Err(e) => Err(e),
        };
        let (strangers, now) = match read {
            Ok(judged) => judged,
            Err(e) => {
                let now = before.to_rfc3339();
                match filed.get(UNREADABLE) {
                    Some(alarm)
                        if alarm
                            .pointer(&format!("/metadata/{RECOVERED_AT}"))
                            .is_some_and(|v| !v.is_null()) =>
                    {
                        self.patch(alarm, &relapse_patch(), rule).await?;
                    }
                    Some(_) => {}
                    None => {
                        self.file(&unreadable_body(&e, &owner, &now, rule), rule)
                            .await?;
                    }
                }
                return Err(HandlerError::Downstream(e));
            }
        };
        self.withdraw_unreadable(
            &filed,
            &format!(
                "{HANDLER} read {} whole at {now}: the watch on asks judges again.",
                window_path()
            ),
            &now,
            rule,
        )
        .await?;

        if strangers.is_empty() {
            tracing::info!(rule, "{HANDLER}: no ask from a caller nobody expects");
            return Ok(());
        }
        match filed.get(STRANGER) {
            None => {
                self.file(
                    &stranger_body(&strangers, &depositors, &owner, &now, rule),
                    rule,
                )
                .await?;
            }
            Some(alarm) => {
                let detail = stranger_detail(&strangers, &depositors, &now, rule);
                if let Some(merge) = extension(alarm, &strangers, detail, &now) {
                    self.patch(alarm, &merge, rule).await?;
                }
            }
        }
        tracing::warn!(
            rule,
            strangers = strangers.len(),
            "{HANDLER}: an ask from a caller that is neither the dispatcher nor a named depositor"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;
    use boss_core::event::Event;
    use boss_core::gate_window::{LiveRead, join_window};

    const NOW: &str = "2026-10-09T03:00:00+00:00";
    const RULE: &str = "an-ask-from-a-stranger-is-read-hourly";
    const ALARMS: &str =
        "/api/jobs?kind=backlog-item&status=open&metadata_has=machine_gate_asked_finding";
    const WINDOW: &str = "/api/events/gate-window?gate=machine-gate&hours=2";
    const DEPOSIT: &str = "automation:machine-token-deposit";
    /// The one address every caller from outside the pod network arrives
    /// as, read off the live window 2026-10-09.
    const NODE: &str = "10.244.0.179";

    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(NOW).unwrap().to_utc()
    }

    fn roster() -> Vec<String> {
        vec!["jobs".to_string(), "policy".to_string()]
    }

    fn depositors() -> Vec<String> {
        vec![DEPOSIT.to_string()]
    }

    fn args() -> Vec<(String, Value)> {
        vec![(DEPOSITORS_ARG.to_string(), Value::String(DEPOSIT.into()))]
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: RULE.into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-10-09", "_at": NOW}),
        }
    }

    /// An `asked` first sighting as the gate states it, `minutes` ago.
    fn asked(service: &str, peer: &str, user: &str, minutes: i64) -> Event {
        Event::new(
            service,
            Gate::MachineGate.kind(Fact::Asked),
            json!({"service": service, "instance": "i", "mode": "report",
                "recording_since": "2026-10-09T00:00:00Z",
                "key": {"peer": peer, "user": user, "method": "GET",
                    "route": "/api/machine-gate/accepts", "presented": "asked"}}),
            at() - Duration::minutes(minutes),
        )
    }

    /// A live tally as `/api/machine-gate/misses` answers it.
    fn tally(service: &str, rows: Vec<Json>, source_overflow: Vec<Json>) -> Json {
        json!({"service": service, "mode": "report", "rows": rows, "overflow": 0,
            "source_overflow": source_overflow,
            "recording_since": "2026-10-09T02:30:00Z"})
    }

    fn row(peer: &str, user: &str, presented: &str, minutes: i64) -> Json {
        let seen = at() - Duration::minutes(minutes);
        json!({"peer": peer, "user": user, "method": "GET",
            "route": "/api/machine-gate/accepts", "presented": presented,
            "count": 3, "first_seen": seen, "last_seen": seen})
    }

    fn window(facts: Vec<Event>, jobs: Json) -> boss_core::gate_window::JoinedWindow {
        join_window(
            Gate::MachineGate,
            &roster(),
            at() - Duration::minutes(LOOKBACK_MINUTES),
            at(),
            Ok(facts),
            vec![
                LiveRead {
                    service: "jobs".into(),
                    answer: Ok(jobs),
                },
                // A port that did not answer: the log is the record, so a
                // dark live read hides no fact.
                LiveRead {
                    service: "policy".into(),
                    answer: Err("connection refused".into()),
                },
            ],
        )
    }

    fn judged(facts: Vec<Event>, jobs: Json) -> Vec<Asker> {
        let w = window(facts, jobs);
        judge(w.observation.as_ref().unwrap(), &depositors()).unwrap()
    }

    fn labels(askers: &[Asker]) -> Vec<String> {
        askers.iter().map(Asker::label).collect()
    }

    #[test]
    fn the_dispatcher_on_loopback_and_a_named_depositor_are_expected_and_no_one_else() {
        let d = depositors();
        assert!(expected("127.0.0.1", DISPATCHER, &d));
        assert!(expected("::1", DISPATCHER, &d));
        assert!(expected("::ffff:127.0.0.1", DISPATCHER, &d));
        assert!(expected(NODE, DEPOSIT, &d));
        // The dispatcher's id from anywhere else is somebody asserting it.
        assert!(!expected(NODE, DISPATCHER, &d));
        assert!(!expected("unknown", DISPATCHER, &d));
        // No id, an unparsed one, and an id the rule does not name.
        assert!(!expected(NODE, "-", &d));
        assert!(!expected("127.0.0.1", "-", &d));
        assert!(!expected(NODE, "(unparsed)", &d));
        assert!(!expected(NODE, "automation:machine-token-effect", &d));
        // A rule that names none excuses none.
        assert!(!expected(NODE, DEPOSIT, &[]));
    }

    #[test]
    fn the_dispatcher_id_is_the_one_every_rule_actor_asserts() {
        let header: Json =
            serde_json::from_str(&super::super::common::dispatcher_reader_header()).unwrap();
        assert_eq!(header["id"], DISPATCHER);
    }

    #[test]
    fn a_strangers_ask_on_the_log_is_named_and_the_expected_askers_are_not() {
        let strangers = judged(
            vec![
                asked("jobs", "127.0.0.1", DISPATCHER, 30),
                asked("jobs", NODE, DEPOSIT, 30),
                asked("jobs", NODE, "someone", 40),
                asked("policy", NODE, "someone", 20),
                asked("jobs", NODE, "-", 10),
                asked("jobs", NODE, DISPATCHER, 5),
            ],
            tally("jobs", vec![], vec![]),
        );
        assert_eq!(
            labels(&strangers),
            vec![
                format!("{NODE} as -"),
                format!("{NODE} as {DISPATCHER}"),
                format!("{NODE} as someone"),
            ]
        );
        let someone = &strangers[2];
        assert_eq!(
            someone.services.iter().cloned().collect::<Vec<_>>(),
            vec!["jobs", "policy"]
        );
        assert_eq!(someone.at_least, 2);
        assert_eq!(someone.last_seen, at() - Duration::minutes(20));
    }

    #[test]
    fn an_ask_older_than_the_lookback_is_not_this_firings() {
        let strangers = judged(
            vec![asked("jobs", NODE, "someone", LOOKBACK_MINUTES + 1)],
            tally(
                "jobs",
                vec![row(NODE, "old", "asked", LOOKBACK_MINUTES + 1)],
                vec![],
            ),
        );
        assert!(strangers.is_empty(), "{strangers:?}");
    }

    /// The live half: a row the recorder never stated is still read, a
    /// miss is not an ask, and an asker past its source's share or past
    /// every key is named by what the gate kept of it.
    #[test]
    fn a_live_row_a_source_past_its_share_and_an_unnamed_ask_are_all_named() {
        let overflowed = Event::new(
            "jobs",
            Gate::MachineGate.kind(Fact::TallyOverflowed),
            json!({"service": "jobs", "mode": "report", "scope": "tally", "presented": "asked"}),
            at() - Duration::minutes(15),
        );
        let a_miss_overflow = Event::new(
            "jobs",
            Gate::MachineGate.kind(Fact::TallyOverflowed),
            json!({"service": "jobs", "mode": "report", "scope": "tally", "presented": "none"}),
            at() - Duration::minutes(15),
        );
        // How a gate built before design e3d19d1a stated an asker past
        // its source's share; the log keeps what earlier builds said.
        let past_share = Event::new(
            "jobs",
            Gate::MachineGate.kind(Fact::Asked),
            json!({"service": "jobs", "mode": "report", "scope": "source",
                "source": "10.20.0.66", "presented": "asked"}),
            at() - Duration::minutes(12),
        );
        // And how one states it since: a tally overflow scoped to the
        // source, which still says which source asked.
        let past_share_overflowed = Event::new(
            "policy",
            Gate::MachineGate.kind(Fact::TallyOverflowed),
            json!({"service": "policy", "mode": "report", "scope": "source",
                "source": "10.20.0.88", "presented": "asked"}),
            at() - Duration::minutes(11),
        );
        let strangers = judged(
            vec![
                overflowed,
                a_miss_overflow,
                past_share,
                past_share_overflowed,
            ],
            tally(
                "jobs",
                vec![
                    row(NODE, "live-only", "asked", 8),
                    row(NODE, "a-miss", "none", 8),
                    row("127.0.0.1", DISPATCHER, "asked", 8),
                ],
                vec![json!({"source": "10.20.0.77", "presented": "asked", "count": 9})],
            ),
        );
        assert_eq!(
            labels(&strangers),
            vec![
                format!("{UNNAMED} as {PAST_SHARE}"),
                format!("10.20.0.66 as {PAST_SHARE}"),
                format!("10.20.0.77 as {PAST_SHARE}"),
                format!("10.20.0.88 as {PAST_SHARE}"),
                format!("{NODE} as live-only"),
            ]
        );
        assert_eq!(strangers[4].at_least, 3, "the live row's count");
        assert_eq!(strangers[2].at_least, 9);
    }

    #[test]
    fn a_fact_or_a_live_answer_that_cannot_be_read_is_refused_not_skipped() {
        let mut keyless = asked("jobs", NODE, "someone", 5);
        keyless.payload.as_object_mut().unwrap().remove("key");
        let w = window(vec![keyless], tally("jobs", vec![], vec![]));
        let err = judge(w.observation.as_ref().unwrap(), &depositors()).unwrap_err();
        assert!(err.contains("neither a key nor a source"), "{err}");

        let w = window(vec![], json!({"service": "jobs", "rows": "not rows"}));
        let err = judge(w.observation.as_ref().unwrap(), &depositors()).unwrap_err();
        assert!(err.contains("jobs"), "{err}");
    }

    fn listing(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    fn alarm_row(finding: &str, askers: Vec<String>) -> Json {
        json!({
            "id": "al-1", "kind": "backlog-item", "status": "open",
            "metadata": {FINDING_KEY: finding, "askers": askers},
            "steps": [{"id": "al-1-triage", "spec_slug": "triage", "status": "ready"}],
        })
    }

    async fn serve(
        facts: Vec<Event>,
        jobs: Json,
        alarms: Vec<Json>,
    ) -> (listing_stub::Stub, Arc<MachineGateAskedAlarm>) {
        assert_eq!(WINDOW, window_path());
        let stub = listing_stub::serve(vec![
            (WINDOW, serde_json::to_value(window(facts, jobs)).unwrap()),
            (ALARMS, listing(alarms)),
        ])
        .await;
        let h = handler(&stub);
        (stub, h)
    }

    fn handler(stub: &listing_stub::Stub) -> Arc<MachineGateAskedAlarm> {
        MachineGateAskedAlarm::with_clock(
            stub.base.clone(),
            stub.base.clone(),
            roster(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
            Arc::new(boss_core::clock::SimClock::new(at())),
        )
    }

    #[tokio::test]
    async fn a_strangers_ask_files_one_alarm_naming_it() {
        let (stub, h) = serve(
            vec![
                asked("jobs", NODE, "someone", 40),
                asked("jobs", "127.0.0.1", DISPATCHER, 30),
            ],
            tally("jobs", vec![], vec![]),
            vec![],
        )
        .await;
        h.invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        let body = &sent[0].1;
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["owner_id"], "emp-owner");
        let m = &body["metadata"];
        assert_eq!(m[FINDING_KEY], STRANGER);
        assert_eq!(m["input_channel"], "telemetry/monitoring");
        assert_eq!(m["area"], "credentials");
        assert_eq!(m["askers"], json!([format!("{NODE} as someone")]));
        let detail = m["detail"].as_str().unwrap();
        assert!(detail.contains(&format!("{NODE} as someone")), "{detail}");
        assert!(detail.contains(RULE), "{detail}");
        assert!(!detail.contains("127.0.0.1"), "{detail}");
    }

    #[tokio::test]
    async fn an_hour_with_only_expected_askers_writes_nothing() {
        let (stub, h) = serve(
            vec![
                asked("jobs", "127.0.0.1", DISPATCHER, 30),
                asked("policy", NODE, DEPOSIT, 30),
            ],
            tally("jobs", vec![row(NODE, "a-miss", "none", 5)], vec![]),
            vec![],
        )
        .await;
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    #[tokio::test]
    async fn an_open_alarm_is_not_filed_twice_and_learns_a_later_stranger() {
        let known = format!("{NODE} as someone");
        let (stub, h) = serve(
            vec![asked("jobs", NODE, "someone", 40)],
            tally("jobs", vec![], vec![]),
            vec![alarm_row(STRANGER, vec![known.clone()])],
        )
        .await;
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());

        let (stub, h) = serve(
            vec![
                asked("jobs", NODE, "someone", 40),
                asked("jobs", NODE, "another", 10),
            ],
            tally("jobs", vec![], vec![]),
            vec![alarm_row(STRANGER, vec![known.clone()])],
        )
        .await;
        h.invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/al-1/metadata");
        assert_eq!(
            sent[0].1["askers"],
            json!([known, format!("{NODE} as another")])
        );
        assert!(
            sent[0].1["detail"].as_str().unwrap().contains("another"),
            "{:?}",
            sent[0].1
        );
    }

    /// The machine never withdraws a stranger alarm: a quiet hour is not
    /// an ask explained.
    #[tokio::test]
    async fn a_quiet_hour_leaves_an_open_stranger_alarm_open() {
        let (stub, h) = serve(
            vec![],
            tally("jobs", vec![], vec![]),
            vec![alarm_row(STRANGER, vec!["x as y".into()])],
        )
        .await;
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    #[tokio::test]
    async fn a_window_that_cannot_be_judged_is_said_once_and_the_firing_fails() {
        for case in ["old-producer", "roster", "interval", "keyless"] {
            let facts = if case == "keyless" {
                let mut e = asked("jobs", NODE, "someone", 5);
                e.payload.as_object_mut().unwrap().remove("key");
                vec![e]
            } else {
                vec![asked("jobs", NODE, "someone", 5)]
            };
            let mut body =
                serde_json::to_value(window(facts, tally("jobs", vec![], vec![]))).unwrap();
            match case {
                "old-producer" => {
                    body.as_object_mut().unwrap().remove("observation");
                }
                "roster" => {
                    body["observation"]["required_services"] = json!(["jobs"]);
                    body["required_services"] = json!(["jobs"]);
                }
                "interval" => {
                    body["observation"]["from"] = json!(at() - Duration::minutes(60));
                    body["from"] = json!(at() - Duration::minutes(60));
                }
                _ => {}
            }
            let stub = listing_stub::serve(vec![(WINDOW, body), (ALARMS, listing(vec![]))]).await;
            let res = handler(&stub).invoke(&args(), &ctx()).await;
            assert!(
                matches!(res, Err(HandlerError::Downstream(_))),
                "{case}: {res:?}"
            );
            let sent = stub.sent();
            assert_eq!(
                sent.len(),
                1,
                "{case}: only the unreadable finding: {sent:?}"
            );
            assert_eq!(sent[0].1["metadata"][FINDING_KEY], UNREADABLE, "{case}");
        }
        // Already open: said once, not once an hour.
        let stub =
            listing_stub::serve(vec![(ALARMS, listing(vec![alarm_row(UNREADABLE, vec![])]))]).await;
        assert!(handler(&stub).invoke(&args(), &ctx()).await.is_err());
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    #[tokio::test]
    async fn the_next_whole_read_withdraws_the_unreadable_alarm() {
        let (stub, h) = serve(
            vec![],
            tally("jobs", vec![], vec![]),
            vec![alarm_row(UNREADABLE, vec![])],
        )
        .await;
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(
            stub.writes()
                .iter()
                .any(|w| w == "PUT /api/jobs/al-1/steps/al-1-triage"),
            "{:?}",
            stub.writes()
        );
    }

    #[tokio::test]
    async fn a_rule_that_names_no_depositors_arg_is_refused_before_any_read() {
        let stub = listing_stub::serve(vec![]).await;
        let res = handler(&stub).invoke(&[], &ctx()).await;
        assert!(matches!(res, Err(HandlerError::Permanent(_))), "{res:?}");
        assert!(stub.writes().is_empty());
    }
}
