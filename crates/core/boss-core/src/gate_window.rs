//! The two halves of design 21946380's gate window, read together.
//! This reports evidence; neither a clean answer nor a caller-chosen
//! lookback authorizes a mode change. The log arithmetic stays in
//! `gate_evidence::window`; here a live tally must match its latest
//! sourced recording start, and every required service must answer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

use crate::event::Event;
use crate::gate_evidence::{
    EvidenceHealth, Fact, Gate, Window, policy_tally_reasons, policy_watch_window, previous_window,
    window,
};
use crate::machine_gate::{Misses, Mode, Presented};

pub const PATH: &str = "/api/events/gate-window";

/// One requested service's answer, or the complete transport refusal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveRead {
    pub service: String,
    pub answer: Result<Value, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveReport {
    pub service: String,
    /// The whole native tally, including rows and recorder health.
    pub snapshot: Option<Value>,
    pub error: Option<String>,
    /// Includes the producer's reasons and this reader's provenance gaps.
    pub not_clean: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JoinedWindow {
    pub gate: Gate,
    pub from: DateTime<Utc>,
    pub now: DateTime<Utc>,
    pub required_services: Vec<String>,
    pub log: Option<Window>,
    pub log_error: Option<String>,
    pub live: Vec<LiveReport>,
    pub not_clean: Vec<String>,
    pub clean_since: Option<DateTime<Utc>>,
    pub covers_requested_window: bool,
    /// Malformed inputs are different from a valid watch that began
    /// too recently. Alarm consumers can report findings on the latter
    /// while retaining every recovery obligation.
    #[serde(default)]
    pub input_errors: Vec<String>,
    /// The complete inputs, not an enforce verdict reused as a rotation
    /// decision. Older producers omit it; consumers must refuse that gap.
    #[serde(default)]
    pub observation: Option<GateObservation>,
    /// The gated services the launch record excuses, each with what the
    /// record said and what this read found at its port. Empty for a
    /// window joined without a launch record (every service required).
    #[serde(default)]
    pub not_launched: Vec<NotLaunchedReport>,
    /// Why the launch record could not be taken at its word. Any entry
    /// here means every gated service was required and the window is
    /// not clean.
    #[serde(default)]
    pub roster_errors: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GateObservation {
    pub gate: Gate,
    pub from: DateTime<Utc>,
    pub now: DateTime<Utc>,
    pub required_services: Vec<String>,
    pub facts: Result<Vec<Event>, String>,
    pub reads: Vec<LiveRead>,
    /// The excused services' names. `reads` still holds one answer for
    /// each of them: a consumer with its own roster (the rotation's
    /// drain) is owed every port's read, required here or not.
    #[serde(default)]
    pub not_launched: Vec<String>,
}

/// One gated service the launcher recorded that it did not start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotLaunched {
    pub service: String,
    pub binary: String,
    pub reason: String,
}

/// Whether anything accepts a connection at a service's port. `Ok(false)`
/// is a refused connection — no process — and nothing else is.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortProbe {
    pub service: String,
    pub listening: Result<bool, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotLaunchedReport {
    pub service: String,
    pub binary: String,
    pub reason: String,
    /// `None` when the port could not be probed or was not.
    pub listening: Option<bool>,
    pub not_clean: Vec<String>,
}

/// Which gated services a clean window requires, derived from what the
/// launcher recorded (backlog 93e0814a, 2026-10-06).
///
/// Until this existed the required set was every gated row of
/// `boss_ports`, while the launcher starts a module's service only when
/// the tenant manifest lists the module (tenant-modules.sh, 18d6a6c9).
/// On the Algedonic, LLC instance that left six rows — assets, catalog,
/// inventory, shipping, simulator, sim-control — required and never
/// started, so the window named six gaps at zero misses and could not
/// read clean.
///
/// The record can only EXCUSE, and an excuse is checked: `join_launched_window`
/// refuses a clean reading when an excused port accepts a connection,
/// answers its tally read, or has a gate fact inside the window. A
/// record that is missing, unreadable, truncated or malformed excuses
/// nothing — every service is required and `errors` says why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchRoster {
    pub required: Vec<String>,
    pub not_launched: Vec<NotLaunched>,
    pub errors: Vec<String>,
}

impl LaunchRoster {
    /// No record consulted: every named service is required.
    pub fn all_required(services: Vec<String>) -> Self {
        Self {
            required: services,
            not_launched: Vec::new(),
            errors: Vec::new(),
        }
    }
}

/// The first line of the launcher's record; `end <count>` is its last.
pub const LAUNCH_RECORD_HEADER: &str = "boss-launch-record v1";

/// Read the launcher's record (`services-launcher.sh`, `launch_record`)
/// against the gated services, each given with the binaries that could
/// serve it (`boss_ports::launcher_binaries`).
///
/// A service is excused only when the record carries a `skip` line for
/// one of its binaries and a `start` line for none. A service the
/// record does not name stays required and is an error, so a roster
/// that grows a row cannot be excused by an older launcher's silence.
pub fn launch_roster(
    gated: &[(String, Vec<String>)],
    record: Result<&str, String>,
) -> LaunchRoster {
    let all = || gated.iter().map(|(service, _)| service.clone()).collect();
    let refuse = |error: String| LaunchRoster {
        required: all(),
        not_launched: Vec::new(),
        errors: vec![error],
    };
    let text = match record {
        Ok(text) => text,
        Err(error) => return refuse(error),
    };
    let lines: Vec<&str> = text.lines().collect();
    let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
        return refuse("the launch record is empty".into());
    };
    if *first != LAUNCH_RECORD_HEADER || lines.len() < 2 {
        return refuse(format!(
            "the launch record does not begin with `{LAUNCH_RECORD_HEADER}`"
        ));
    }
    let decisions = &lines[1..lines.len() - 1];
    if last
        .strip_prefix("end ")
        .and_then(|n| n.parse::<usize>().ok())
        != Some(decisions.len())
    {
        return refuse(format!(
            "the launch record is truncated or miscounted: {} decision line(s), last line `{last}`",
            decisions.len()
        ));
    }
    // binary -> Some(reason) for a skip, None for a start.
    let mut seen: Vec<(&str, Option<&str>)> = Vec::new();
    for line in decisions {
        let mut words = line.splitn(3, ' ');
        let entry = match (words.next(), words.next(), words.next()) {
            (Some("start"), Some(binary), None) if !binary.is_empty() => (binary, None),
            (Some("skip"), Some(binary), Some(reason))
                if !binary.is_empty() && !reason.trim().is_empty() =>
            {
                (binary, Some(reason))
            }
            _ => return refuse(format!("the launch record has a malformed line `{line}`")),
        };
        if seen.iter().any(|(binary, _)| *binary == entry.0) {
            return refuse(format!(
                "the launch record names {} more than once",
                entry.0
            ));
        }
        seen.push(entry);
    }
    let mut roster = LaunchRoster::default();
    for (service, binaries) in gated {
        let mine: Vec<&(&str, Option<&str>)> = seen
            .iter()
            .filter(|(binary, _)| binaries.iter().any(|b| b == binary))
            .collect();
        let started = mine.iter().any(|(_, skip)| skip.is_none());
        match mine
            .iter()
            .find_map(|(binary, skip)| skip.map(|r| (binary, r)))
        {
            Some((binary, reason)) if !started => roster.not_launched.push(NotLaunched {
                service: service.clone(),
                binary: binary.to_string(),
                reason: reason.to_string(),
            }),
            _ => {
                if mine.is_empty() {
                    roster.errors.push(format!(
                        "the launch record names no decision for {service} (binaries {})",
                        binaries.join(", ")
                    ));
                }
                roster.required.push(service.clone());
            }
        }
    }
    roster
}

// No defaults: an older producer without either half's identity or
// health is missing evidence, never an empty clean tally.
#[derive(Deserialize)]
struct CommonTally {
    mode: Mode,
    recording_since: DateTime<Utc>,
    evidence: EvidenceHealth,
    not_clean: Vec<String>,
    overflow: u64,
    rows: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum PolicyArm {
    Unsigned,
    Service,
}

#[derive(Deserialize)]
struct PolicyRow {
    arm: PolicyArm,
    caller: String,
    role: String,
    peer: String,
    count: u64,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Enforce,
    Previous,
    PolicyWatch,
}

fn live_reasons(
    gate: Gate,
    service: &str,
    body: &Value,
    from: DateTime<Utc>,
    now: DateTime<Utc>,
    facts: &[Event],
    purpose: Purpose,
) -> Result<Vec<String>, String> {
    let previous = purpose == Purpose::Previous;
    let tally: CommonTally = serde_json::from_value(body.clone())
        .map_err(|e| format!("tally is missing or malformed: {e}"))?;
    let mut why = tally.not_clean.clone();
    if tally.mode == Mode::Off {
        why.push("mode off watches nothing".into());
    }
    if !tally.evidence.recorder {
        why.push("this process has no durable recorder".into());
    }
    if tally.evidence.instance.is_empty() {
        why.push("this process has no instance identity".into());
    }
    why.extend(tally.evidence.not_clean());
    if tally.recording_since > now {
        why.push("the tally begins after this observation".into());
    }
    if tally.overflow != 0 {
        why.push(format!("tally overflow {} names no caller", tally.overflow));
    }
    match gate {
        Gate::MachineGate => {
            if !body.get("source_overflow").is_some_and(Value::is_array) {
                return Err("machine tally has no complete source_overflow array".into());
            }
            let misses: Misses = serde_json::from_value(body.clone())
                .map_err(|e| format!("machine tally is malformed: {e}"))?;
            if previous {
                let mut expected = misses.clone();
                expected.judge();
                if expected.not_clean != tally.not_clean {
                    why.push("machine tally reasons disagree with its complete snapshot".into());
                } else {
                    // Only the producer's derivable enforcement reasons
                    // are replaced. Unknown reasons above remain a refusal.
                    why.clear();
                }
            }
            if misses.service != service {
                why.push(format!(
                    "tally names {}, requested {service}",
                    misses.service
                ));
            }
            if misses.rows.iter().any(|r| {
                r.count == 0
                    || r.first_seen > r.last_seen
                    || r.last_seen > now
                    || (previous && r.first_seen < tally.recording_since)
            }) {
                why.push("machine tally has an invalid row count or time".into());
            }
            if !previous
                && misses
                    .rows
                    .iter()
                    .any(|r| r.key.presented.enforce_refuses())
            {
                why.push("machine tally still holds an enforce-refused caller".into());
            }
            if misses
                .source_overflow
                .iter()
                .any(|r| r.count == 0 || (!previous && r.presented.enforce_refuses()))
            {
                why.push("machine tally has invalid or enforce-refused source overflow".into());
            }
            if previous {
                if misses.source_overflow.iter().any(|row| {
                    row.source.is_empty()
                        || match (row.first_seen, row.last_seen) {
                            (Some(first), Some(last)) => {
                                first > last || last > now || first < tally.recording_since
                            }
                            (None, None) => false, // Legacy unknown times remain unknown.
                            _ => true,
                        }
                }) {
                    why.push(
                        "previous drain has malformed source overflow attribution or time".into(),
                    );
                }
                if misses.mode == Mode::Off {
                    why.push("mode off watches nothing".into());
                }
                if !misses.evidence.recorder || misses.evidence.instance.is_empty() {
                    why.push("previous drain has no durable recorder identity".into());
                }
                why.extend(misses.evidence.not_clean());
                if misses.recording_since > now || misses.overflow != 0 {
                    why.push("previous drain has future recording or unattributed overflow".into());
                }
                if misses
                    .rows
                    .iter()
                    .any(|r| r.key.presented == Presented::Previous && r.last_seen >= from)
                    || misses.source_overflow.iter().any(|r| {
                        r.presented == Presented::Previous
                            && r.last_seen.is_none_or(|at| at >= from)
                    })
                {
                    why.push("live tally still holds a previous presentation".into());
                }
            }
        }
        Gate::PolicyCheck => {
            if purpose == Purpose::PolicyWatch {
                let checks = tally
                    .rows
                    .iter()
                    .try_fold(0_u64, |sum, row| {
                        row.get("count")
                            .and_then(Value::as_u64)
                            .and_then(|count| sum.checked_add(count))
                    })
                    .ok_or("policy row counts are missing or overflow")?;
                let expected = policy_tally_reasons(
                    tally.mode,
                    tally.rows.len(),
                    checks,
                    tally.overflow,
                    &tally.evidence,
                );
                if expected == tally.not_clean {
                    why.clear();
                    if !tally.evidence.recorder
                        || tally.evidence.instance.is_empty()
                        || tally.recording_since > now
                    {
                        why.push(
                            "policy watch has no valid durable recorder, mode or recording time"
                                .into(),
                        );
                    }
                    why.extend(tally.evidence.not_clean());
                } else {
                    why.push("policy tally reasons disagree with its complete snapshot".into());
                }
            }
            if service != "policy" {
                why.push("policy-check tally was requested from another service".into());
            }
            if body
                .get("service")
                .is_some_and(|s| s.as_str() != Some(service))
            {
                why.push("policy tally names a different service".into());
            }
            // The policy check has no service field; the requested port
            // and the independently sourced instance below bind it.
            for key in ["switch", "file"] {
                if body
                    .get(key)
                    .and_then(Value::as_str)
                    .is_none_or(|v| v.is_empty())
                {
                    return Err(format!("policy tally has no {key}"));
                }
            }
            match body.get("mode_error") {
                Some(Value::Null) => {}
                Some(Value::String(error)) => why.push(format!("policy mode is degraded: {error}")),
                _ => return Err("policy tally has no valid mode_error".into()),
            }
            let clean: Option<DateTime<Utc>> = serde_json::from_value(
                body.get("clean_since")
                    .cloned()
                    .ok_or("policy tally has no clean_since")?,
            )
            .map_err(|e| format!("policy clean_since is malformed: {e}"))?;
            if tally.not_clean.is_empty() && clean != Some(tally.recording_since) {
                why.push("policy clean_since disagrees with its clean live tally".into());
            }
            for row in &tally.rows {
                let row: PolicyRow = serde_json::from_value(row.clone())
                    .map_err(|e| format!("policy tally row is malformed: {e}"))?;
                if row.count == 0
                    || row.first_seen > row.last_seen
                    || row.last_seen > now
                    || (purpose == Purpose::PolicyWatch && row.first_seen < tally.recording_since)
                    || row.caller.is_empty()
                    || row.role.is_empty()
                    || row.peer.is_empty()
                {
                    why.push("policy tally has an invalid caller, count or time".into());
                }
                match row.arm {
                    PolicyArm::Unsigned | PolicyArm::Service => {}
                }
            }
            if purpose != Purpose::PolicyWatch && !tally.rows.is_empty() {
                why.push("policy tally still holds an enforce-refused caller".into());
            }
        }
    }
    let began = gate.kind(Fact::RecordingBegan);
    let latest = facts
        .iter()
        .filter(|e| {
            e.kind == began
                && e.source == service
                && e.payload.get("service").and_then(Value::as_str) == Some(service)
        })
        .max_by_key(|e| (e.timestamp, e.id));
    match latest {
        None => why.push("no sourced recording start binds this live tally to the log".into()),
        Some(e) => {
            let mode: Result<Mode, _> =
                serde_json::from_value(e.payload.get("mode").cloned().unwrap_or(Value::Null));
            let since: Result<DateTime<Utc>, _> =
                serde_json::from_value(e.payload.get("since").cloned().unwrap_or(Value::Null));
            if e.payload.get("instance").and_then(Value::as_str)
                != Some(tally.evidence.instance.as_str())
                || mode.ok() != Some(tally.mode)
                || since.ok() != Some(tally.recording_since)
            {
                why.push(format!(
                    "live instance/mode/start does not match sourced recording_began {}",
                    e.id
                ));
            }
        }
    }
    Ok(why)
}

/// Neither source alone can earn a clean reading. Preserve all answers
/// even when one fails, so a refusal names its cause and its coverage.
pub fn join_window(
    gate: Gate,
    required: &[String],
    from: DateTime<Utc>,
    now: DateTime<Utc>,
    facts: Result<Vec<Event>, String>,
    reads: Vec<LiveRead>,
) -> JoinedWindow {
    join_selected(
        gate,
        required,
        from,
        now,
        facts,
        reads,
        Purpose::Enforce,
        Excused::NONE,
    )
}

/// The enforce window over what the launcher started. `roster.required`
/// is judged exactly as `join_window` judges it; each excused service is
/// held to its excuse — see `LaunchRoster`. `reads` carries one answer
/// for every required AND every excused service, `probes` one port
/// probe for every excused one.
pub fn join_launched_window(
    gate: Gate,
    roster: &LaunchRoster,
    probes: &[PortProbe],
    from: DateTime<Utc>,
    now: DateTime<Utc>,
    facts: Result<Vec<Event>, String>,
    reads: Vec<LiveRead>,
) -> JoinedWindow {
    join_selected(
        gate,
        &roster.required,
        from,
        now,
        facts,
        reads,
        Purpose::Enforce,
        Excused {
            not_launched: &roster.not_launched,
            errors: &roster.errors,
            probes,
        },
    )
}

/// The launch record's half of a join: empty for every window that
/// consults no record.
#[derive(Clone, Copy)]
struct Excused<'a> {
    not_launched: &'a [NotLaunched],
    errors: &'a [String],
    probes: &'a [PortProbe],
}

impl Excused<'_> {
    const NONE: Self = Excused {
        not_launched: &[],
        errors: &[],
        probes: &[],
    };
}

/// A previous-slot drain uses the same complete observation and health
/// checks, while retaining its distinct dirtying facts.
pub fn join_previous_window(
    required: &[String],
    from: DateTime<Utc>,
    now: DateTime<Utc>,
    facts: Result<Vec<Event>, String>,
    reads: Vec<LiveRead>,
) -> JoinedWindow {
    join_selected(
        Gate::MachineGate,
        required,
        from,
        now,
        facts,
        reads,
        Purpose::Previous,
        Excused::NONE,
    )
}

pub fn join_policy_watch(
    required: &[String],
    from: DateTime<Utc>,
    now: DateTime<Utc>,
    facts: Result<Vec<Event>, String>,
    reads: Vec<LiveRead>,
) -> JoinedWindow {
    join_selected(
        Gate::PolicyCheck,
        required,
        from,
        now,
        facts,
        reads,
        Purpose::PolicyWatch,
        Excused::NONE,
    )
}

fn join_selected(
    gate: Gate,
    required: &[String],
    from: DateTime<Utc>,
    now: DateTime<Utc>,
    facts: Result<Vec<Event>, String>,
    reads: Vec<LiveRead>,
    purpose: Purpose,
    excused: Excused<'_>,
) -> JoinedWindow {
    let observation = GateObservation {
        gate,
        from,
        now,
        required_services: required.to_vec(),
        facts: facts.clone(),
        reads: reads.clone(),
        not_launched: excused
            .not_launched
            .iter()
            .map(|n| n.service.clone())
            .collect(),
    };
    let mut why = Vec::new();
    if from >= now {
        why.push("the requested window has no positive duration".into());
    }
    let mut required_services = required.to_vec();
    required_services.sort();
    required_services.dedup();
    if required_services.is_empty()
        || required_services.len() != required.len()
        || required_services.iter().any(String::is_empty)
    {
        why.push("required service roster is empty, ambiguous or malformed".into());
    }
    let roster_errors: Vec<String> = excused.errors.to_vec();
    why.extend(roster_errors.iter().map(|e| format!("launch record: {e}")));
    let (facts, log_error) = match facts {
        Ok(facts) => (facts, None),
        Err(e) => {
            why.push(format!("log: {e}"));
            (Vec::new(), Some(e))
        }
    };
    let mut identities = HashSet::new();
    let mut bound = crate::gate_evidence::FactReadBound::default();
    for event in &facts {
        if !identities.insert(event.id) {
            why.push(format!("log repeats immutable record {}", event.id));
        }
        if let Err(error) = bound.observe(event) {
            why.push(format!("log: {error}"));
            break;
        }
    }
    if facts
        .iter()
        .any(|e| e.timestamp > now || e.payload.get("_simulated") == Some(&Value::Bool(true)))
    {
        why.push("log contains future or simulated gate evidence".into());
    }
    // The existing coverage projection groups starts by the named
    // service. A malformed newer start must not disappear from that
    // grouping and leave an older live instance looking current.
    // A pre-window malformed start can expire only after the same
    // source states a complete later watch before the window opens.
    // Do not use a late or backdated start to bridge unknown history.
    // Keep the projection unchanged and name each relevant unbound
    // input's immutable record and payload.
    let began = gate.kind(Fact::RecordingBegan);
    if purpose != Purpose::Enforce {
        for event in facts.iter().filter(|event| {
            gate.fact_of(&event.kind)
                .is_some_and(|fact| !matches!(fact, Fact::RecordingBegan | Fact::RecordingEnded))
        }) {
            let start = facts
                .iter()
                .filter(|candidate| {
                    candidate.kind == began
                        && candidate.source == event.source
                        && candidate.timestamp <= event.timestamp
                })
                .max_by_key(|candidate| (candidate.timestamp, candidate.id));
            let bound = start.is_some_and(|start| {
                event.payload.get("service").and_then(Value::as_str) == Some(event.source.as_str())
                    && event
                        .payload
                        .get("instance")
                        .and_then(Value::as_str)
                        .is_some_and(|instance| {
                            !instance.is_empty()
                                && start.payload.get("instance").and_then(Value::as_str)
                                    == Some(instance)
                        })
                    && (gate.fact_of(&event.kind) == Some(Fact::FactsLost)
                        || (serde_json::from_value::<Mode>(
                            event.payload.get("mode").cloned().unwrap_or(Value::Null),
                        )
                        .ok()
                        .zip(
                            serde_json::from_value::<Mode>(
                                start.payload.get("mode").cloned().unwrap_or(Value::Null),
                            )
                            .ok(),
                        )
                        .is_some_and(|(event_mode, start_mode)| event_mode == start_mode)
                            && serde_json::from_value::<DateTime<Utc>>(
                                event
                                    .payload
                                    .get("recording_since")
                                    .cloned()
                                    .unwrap_or(Value::Null),
                            )
                            .ok()
                            .zip(
                                serde_json::from_value::<DateTime<Utc>>(
                                    start.payload.get("since").cloned().unwrap_or(Value::Null),
                                )
                                .ok(),
                            )
                            .is_some_and(|(event_since, start_since)| event_since == start_since)))
            });
            if !bound {
                why.push(format!(
                    "log fact {} has no matching sourced process start",
                    event.id
                ));
            }
        }
    }
    // First-sighting facts do not state a retired epoch's final use.
    // A drained recorder proves delivery, not that use stopped at the
    // first sighting. Keep the uncertainty until retirement ages out.
    if purpose != Purpose::Enforce {
        for event in facts.iter().filter(|event| event.timestamp < from) {
            let relevant = match purpose {
                Purpose::Previous => match gate.fact_of(&event.kind) {
                    Some(Fact::PreviousPresented | Fact::FactsLost) => true,
                    Some(Fact::TallyOverflowed) => {
                        event.payload.get("scope").and_then(Value::as_str) != Some("source")
                            || event
                                .payload
                                .get("source")
                                .and_then(Value::as_str)
                                .is_none_or(str::is_empty)
                            || serde_json::from_value::<crate::machine_gate::Presented>(
                                event
                                    .payload
                                    .get("presented")
                                    .cloned()
                                    .unwrap_or(Value::Null),
                            )
                            .map_or(true, |presented| {
                                presented == crate::machine_gate::Presented::Previous
                            })
                    }
                    _ => false,
                },
                Purpose::PolicyWatch => match gate.fact_of(&event.kind) {
                    Some(Fact::WouldRefuse | Fact::Refused) => {
                        event
                            .payload
                            .get("key")
                            .and_then(|key| key.get("arm"))
                            .and_then(Value::as_str)
                            != Some("unsigned")
                    }
                    Some(Fact::TallyOverflowed | Fact::FactsLost) => true,
                    _ => false,
                },
                Purpose::Enforce => false,
            };
            if !relevant {
                continue;
            }
            let retired = facts
                .iter()
                .filter(|boundary| {
                    boundary.source == event.source
                        && boundary.timestamp >= event.timestamp
                        && ((gate.fact_of(&boundary.kind) == Some(Fact::RecordingEnded)
                            && boundary.payload.get("instance") == event.payload.get("instance"))
                            || (boundary.kind == began && boundary.timestamp > event.timestamp))
                })
                .min_by_key(|boundary| (boundary.timestamp, boundary.id));
            if retired.is_some_and(|boundary| boundary.timestamp >= from) {
                why.push(format!(
                    "log fact {} has unknown final usage before epoch retirement {}",
                    event.id,
                    retired.map(|boundary| boundary.timestamp).unwrap_or(now)
                ));
            }
        }
    }
    for event in facts.iter().filter(|event| event.kind == began) {
        let superseded_before_window = event.timestamp < from
            && facts.iter().any(|later| {
                let since = later
                    .payload
                    .get("since")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<DateTime<Utc>>(value).ok());
                let mode = later
                    .payload
                    .get("mode")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<Mode>(value).ok());
                later.kind == began
                    && later.source == event.source
                    && later.payload.get("service").and_then(Value::as_str)
                        == Some(event.source.as_str())
                    && later.timestamp > event.timestamp
                    && later.timestamp <= from
                    && since.is_some_and(|since| since > event.timestamp && since <= from)
                    && mode.is_some_and(|mode| mode != Mode::Off)
                    && later
                        .payload
                        .get("instance")
                        .and_then(Value::as_str)
                        .is_some_and(|instance| !instance.is_empty())
            });
        if event
            .payload
            .get("service")
            .and_then(Value::as_str)
            .is_none_or(|service| service.is_empty() || service != event.source)
            && !superseded_before_window
        {
            why.push(format!(
                "log recording_began {} from {} has missing or conflicting service provenance: {}",
                event.id, event.source, event.payload
            ));
        }
    }
    let services: Vec<&str> = required_services.iter().map(String::as_str).collect();
    let input_errors = why.clone();
    let log = log_error.is_none().then(|| match purpose {
        Purpose::Enforce => window(gate, &services, from, now, &facts),
        Purpose::Previous => previous_window(&services, from, now, &facts),
        Purpose::PolicyWatch => policy_watch_window(&services, from, now, &facts),
    });
    if let Some(log) = &log {
        why.extend(log.not_clean.iter().map(|e| format!("log: {e}")));
    }
    let mut live = Vec::new();
    for service in &required_services {
        let mine: Vec<&LiveRead> = reads.iter().filter(|r| &r.service == service).collect();
        if mine.len() != 1 {
            why.push(format!(
                "{service}: {} live answers, expected exactly one",
                mine.len()
            ));
        }
        if mine.is_empty() {
            live.push(LiveReport {
                service: service.clone(),
                snapshot: None,
                error: Some("required live tally was not read".into()),
                not_clean: vec!["required live tally was not read".into()],
            });
        }
        for read in mine {
            let (snapshot, error, not_clean) = match &read.answer {
                Err(error) => (None, Some(error.clone()), vec![error.clone()]),
                Ok(body) => match live_reasons(gate, service, body, from, now, &facts, purpose) {
                    Ok(why) => (Some(body.clone()), None, why),
                    Err(error) => (Some(body.clone()), Some(error.clone()), vec![error]),
                },
            };
            why.extend(not_clean.iter().map(|e| format!("{service}: {e}")));
            live.push(LiveReport {
                service: service.clone(),
                snapshot,
                error,
                not_clean,
            });
        }
    }
    // An excuse is a claim that no process serves the port, and every
    // way this read could have seen one is checked. A record alone —
    // stale, doctored, or written by a launch that was not this one —
    // therefore cannot take a running service out of the window.
    let mut not_launched = Vec::new();
    for (at, excuse) in excused.not_launched.iter().enumerate() {
        let service = &excuse.service;
        let mut gaps = Vec::new();
        if required_services.contains(service)
            || excused.not_launched[..at]
                .iter()
                .any(|earlier| &earlier.service == service)
        {
            gaps.push("the roster names it more than once".to_string());
        }
        let probes: Vec<&PortProbe> = excused
            .probes
            .iter()
            .filter(|probe| &probe.service == service)
            .collect();
        let listening = match probes.as_slice() {
            [probe] => match &probe.listening {
                Ok(false) => Some(false),
                Ok(true) => {
                    gaps.push(
                        "its port accepts a connection: a process the launch record does not account for"
                            .to_string(),
                    );
                    Some(true)
                }
                Err(error) => {
                    gaps.push(format!("its port could not be probed: {error}"));
                    None
                }
            },
            other => {
                gaps.push(format!(
                    "its port has {} probes, expected exactly one",
                    other.len()
                ));
                None
            }
        };
        let mine: Vec<&LiveRead> = reads.iter().filter(|r| &r.service == service).collect();
        if mine.len() != 1 {
            gaps.push(format!("{} live answers, expected exactly one", mine.len()));
        }
        if mine.iter().any(|read| read.answer.is_ok()) {
            gaps.push("it answered its tally read".to_string());
        }
        let recorded: Vec<&Event> = facts
            .iter()
            .filter(|event| {
                event.timestamp >= from
                    && (&event.source == service
                        || event.payload.get("service").and_then(Value::as_str)
                            == Some(service.as_str()))
            })
            .collect();
        if let Some(first) = recorded.first() {
            gaps.push(format!(
                "the log holds {} gate fact(s) from or naming it inside the window, the first {}",
                recorded.len(),
                first.id
            ));
        }
        why.extend(gaps.iter().map(|gap| {
            format!(
                "{service}: recorded as not launched ({}), but {gap}",
                excuse.reason
            )
        }));
        not_launched.push(NotLaunchedReport {
            service: service.clone(),
            binary: excuse.binary.clone(),
            reason: excuse.reason.clone(),
            listening,
            not_clean: gaps,
        });
    }
    for read in reads.iter().filter(|r| {
        !required_services.contains(&r.service)
            && !excused.not_launched.iter().any(|n| n.service == r.service)
    }) {
        let error = "live answer names a service outside the required roster".to_string();
        why.push(format!("{}: {error}", read.service));
        live.push(LiveReport {
            service: read.service.clone(),
            snapshot: read.answer.clone().ok(),
            error: Some(error.clone()),
            not_clean: vec![error],
        });
    }
    let clean_since = why
        .is_empty()
        .then(|| log.as_ref().and_then(|l| l.log_clean_since))
        .flatten();
    JoinedWindow {
        gate,
        from,
        now,
        required_services,
        log,
        log_error,
        live,
        not_clean: why,
        clean_since,
        covers_requested_window: clean_since.is_some_and(|s| s <= from),
        input_errors,
        observation: Some(observation),
        not_launched,
        roster_errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use serde_json::json;
    use uuid::Uuid;

    fn at(hours: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap() + Duration::hours(hours)
    }

    #[test]
    fn the_default_enforce_log_projection_is_conserved_by_the_observation_seam() {
        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            for dirty in [false, true] {
                let mut input = facts(gate);
                if dirty {
                    input.push(event(
                        gate,
                        Fact::WouldRefuse,
                        74,
                        json!({"service":"policy"}),
                    ));
                }
                let original = window(gate, &["policy"], at(51), at(75), &input);
                let joined = join_window(
                    gate,
                    &["policy".into()],
                    at(51),
                    at(75),
                    Ok(input.clone()),
                    vec![read(snapshot(gate))],
                );
                assert_eq!(
                    serde_json::to_value(joined.log.as_ref().unwrap()).unwrap(),
                    serde_json::to_value(original).unwrap()
                );
                assert_eq!(joined.covers_requested_window, !dirty);
                assert!(joined.input_errors.is_empty());
                let retained = joined.observation.unwrap();
                assert_eq!(
                    serde_json::to_value(retained.facts.unwrap()).unwrap(),
                    serde_json::to_value(input).unwrap()
                );
            }
        }
    }

    #[test]
    fn a_consumer_receives_the_complete_observation_not_only_an_enforce_verdict() {
        let gate = Gate::PolicyCheck;
        let input = facts(gate);
        let reads = vec![LiveRead {
            service: "policy".into(),
            answer: Err("down".into()),
        }];
        let joined = join_window(
            gate,
            &["policy".into()],
            at(1),
            at(3),
            Ok(input.clone()),
            reads,
        );
        let wire = serde_json::to_value(joined).unwrap();
        assert_eq!(
            wire["observation"]["facts"]["Ok"],
            serde_json::to_value(input).unwrap()
        );
        assert_eq!(wire["observation"]["reads"][0]["answer"]["Err"], "down");
    }

    #[test]
    fn a_previous_drain_is_not_an_enforce_window_for_unrelated_callers() {
        let gate = Gate::MachineGate;
        let mut input = facts(gate);
        input.push(event(gate, Fact::WouldRefuse, 70, json!({"service":"policy","mode":"report","recording_since":at(0),"instance":"old","key":{"presented":"none","peer":"fixture","user":"caller","method":"GET","route":"/fixture"}})));
        let ordinary = join_window(
            gate,
            &["policy".into()],
            at(51),
            at(75),
            Ok(input.clone()),
            vec![read(snapshot(gate))],
        );
        assert!(!ordinary.covers_requested_window);
        let joined = join_previous_window(
            &["policy".into()],
            at(51),
            at(75),
            Ok(input),
            vec![read(snapshot(gate))],
        );
        assert!(
            joined.covers_requested_window,
            "unrelated missing-token evidence is not a previous-token presentation: {joined:?}"
        );
    }

    #[test]
    fn a_previous_drain_retains_health_provenance_and_unknown_overflow_refusals() {
        let gate = Gate::MachineGate;
        for mutation in ["retrying", "instance", "overflow", "previous", "future"] {
            let mut body = snapshot(gate);
            let mut input = facts(gate);
            match mutation {
                "retrying" => body["evidence"]["retrying"] = json!(true),
                "instance" => body["evidence"]["instance"] = json!("different"),
                "overflow" => body["overflow"] = json!(1),
                "previous" => input.push(event(
                    gate,
                    Fact::PreviousPresented,
                    70,
                    json!({"service":"policy"}),
                )),
                "future" => input.push(event(
                    gate,
                    Fact::PreviousPresented,
                    76,
                    json!({"service":"policy"}),
                )),
                _ => unreachable!(),
            }
            let mut misses: Misses = serde_json::from_value(body).unwrap();
            misses.judge();
            let joined = join_previous_window(
                &["policy".into()],
                at(51),
                at(75),
                Ok(input),
                vec![read(serde_json::to_value(misses).unwrap())],
            );
            assert!(!joined.covers_requested_window, "{mutation}: {joined:?}");
        }
    }

    #[test]
    fn malformed_named_overflow_cannot_be_excused_as_unrelated() {
        let gate = Gate::MachineGate;
        let mut input = facts(gate);
        input.push(event(
            gate,
            Fact::TallyOverflowed,
            70,
            json!({"scope":"source","presented":"none"}),
        ));
        let joined = join_previous_window(
            &["policy".into()],
            at(51),
            at(75),
            Ok(input),
            vec![read(snapshot(gate))],
        );
        assert!(
            !joined.covers_requested_window,
            "an absent named source is not an attributed overflow: {joined:?}"
        );
    }

    #[test]
    fn a_duplicate_immutable_record_is_not_a_complete_observation() {
        let gate = Gate::MachineGate;
        let mut input = facts(gate);
        input.push(input[0].clone());
        let joined = join_previous_window(
            &["policy".into()],
            at(51),
            at(75),
            Ok(input),
            vec![read(snapshot(gate))],
        );
        assert!(
            !joined.covers_requested_window,
            "duplicate immutable identity must refuse: {joined:?}"
        );
    }

    #[test]
    fn unrelated_live_overflow_still_requires_valid_attribution_and_times() {
        let gate = Gate::MachineGate;
        for (source, first, last) in [("", 70, 71), ("fixture", 72, 71), ("fixture", 70, 76)] {
            let mut body = snapshot(gate);
            body["source_overflow"] = json!([{"source":source,"presented":"none","count":1,"first_seen":at(first),"last_seen":at(last)}]);
            let mut misses: Misses = serde_json::from_value(body).unwrap();
            misses.judge();
            let joined = join_previous_window(
                &["policy".into()],
                at(51),
                at(75),
                Ok(facts(gate)),
                vec![read(serde_json::to_value(misses).unwrap())],
            );
            assert!(
                !joined.covers_requested_window,
                "malformed unrelated overflow {source}/{first}/{last}: {joined:?}"
            );
        }
    }

    #[test]
    fn a_policy_alarm_can_watch_an_unsigned_finding_without_forgiving_health() {
        let gate = Gate::PolicyCheck;
        let mut body = snapshot(gate);
        body["rows"] = json!([{"arm":"unsigned","caller":"anonymous","role":"none","peer":"fixture","count":1,"first_seen":at(74),"last_seen":at(74)}]);
        body["not_clean"] = json!(["1 caller shape(s), 1 check(s), that `enforce` refuses"]);
        body["clean_since"] = Value::Null;
        let ordinary = join_window(
            gate,
            &["policy".into()],
            at(51),
            at(75),
            Ok(facts(gate)),
            vec![read(body.clone())],
        );
        assert!(!ordinary.covers_requested_window);
        let joined = join_policy_watch(
            &["policy".into()],
            at(51),
            at(75),
            Ok(facts(gate)),
            vec![read(body)],
        );
        assert!(
            joined.covers_requested_window,
            "an unsigned finding is not a lapsed service grant or a missing watch: {joined:?}"
        );
    }

    #[test]
    fn an_unrelated_fact_from_an_unbound_process_is_not_forgiven() {
        let gate = Gate::MachineGate;
        let mut input = facts(gate);
        input.push(event(gate, Fact::WouldRefuse, 70, json!({"service":"policy","mode":"report","recording_since":at(0),"instance":"not-the-recorded-process","key":{"presented":"none"}})));
        let joined = join_previous_window(
            &["policy".into()],
            at(51),
            at(75),
            Ok(input),
            vec![read(snapshot(gate))],
        );
        assert!(
            !joined.covers_requested_window,
            "unbound process evidence is not an excuse: {joined:?}"
        );
    }

    #[test]
    fn a_valid_off_policy_snapshot_is_not_a_healthy_watch_or_an_unreadable_tally() {
        let gate = Gate::PolicyCheck;
        let mut body = snapshot(gate);
        body["mode"] = json!("off");
        body["not_clean"] =
            json!(["mode `off` records nothing, so its silence is no evidence of anything"]);
        body["clean_since"] = Value::Null;
        let mut input = facts(gate);
        input.last_mut().unwrap().payload["mode"] = json!("off");
        let joined = join_policy_watch(
            &["policy".into()],
            at(51),
            at(75),
            Ok(input),
            vec![read(body)],
        );
        assert!(!joined.covers_requested_window);
        assert!(
            joined.live[0].not_clean.is_empty(),
            "valid off is not unreadable: {joined:?}"
        );
    }

    #[test]
    fn a_policy_live_row_cannot_predate_its_recorded_process() {
        let gate = Gate::PolicyCheck;
        let mut body = snapshot(gate);
        body["rows"] = json!([{"arm":"service","caller":"automation:jobs","role":"platform-admin","peer":"fixture","count":1,"first_seen":at(70),"last_seen":at(71)}]);
        body["not_clean"] = json!(["1 caller shape(s), 1 check(s), that `enforce` refuses"]);
        body["clean_since"] = Value::Null;
        let joined = join_policy_watch(
            &["policy".into()],
            at(51),
            at(75),
            Ok(facts(gate)),
            vec![read(body)],
        );
        assert!(
            !joined.live[0].not_clean.is_empty(),
            "row from before this process is malformed evidence: {joined:?}"
        );
    }

    #[test]
    fn equivalent_recording_instants_bind_the_same_sourced_epoch() {
        let gate = Gate::MachineGate;
        let mut input = facts(gate);
        input.push(event(gate, Fact::WouldRefuse, 70, json!({"service":"policy","instance":"old","mode":"report","recording_since":at(0).to_rfc3339(),"key":{"presented":"none"}})));
        let joined = join_previous_window(
            &["policy".into()],
            at(51),
            at(75),
            Ok(input),
            vec![read(snapshot(gate))],
        );
        assert!(
            joined.covers_requested_window,
            "RFC3339 UTC and numeric-offset encodings name the same epoch: {joined:?}"
        );
    }

    #[test]
    fn a_retired_previous_sighting_has_no_proven_final_usage_time() {
        let gate = Gate::MachineGate;
        let mut input = facts(gate);
        input.push(event(gate, Fact::PreviousPresented, 1, json!({"service":"policy","instance":"old","mode":"report","recording_since":at(0),"key":{"presented":"previous"}})));
        let joined = join_previous_window(
            &["policy".into()],
            at(51),
            at(75),
            Ok(input),
            vec![read(snapshot(gate))],
        );
        assert!(
            !joined.covers_requested_window,
            "a first sighting does not bound the final use before retirement: {joined:?}"
        );
        assert!(
            joined
                .input_errors
                .iter()
                .any(|reason| reason.contains("final usage"))
        );
    }

    #[tokio::test]
    async fn the_log_port_keeps_pre_window_sightings_of_a_recently_retired_epoch() {
        use crate::gate_evidence::{GateEvidenceLog, InMemoryGateEvidence};
        let gate = Gate::MachineGate;
        let mut input = facts(gate);
        let previous = event(
            gate,
            Fact::PreviousPresented,
            1,
            json!({"service":"policy","instance":"old","mode":"report","recording_since":at(0),"key":{"presented":"previous"}}),
        );
        input.push(previous.clone());
        let log = InMemoryGateEvidence::new(input);
        let read = log.facts(gate, at(51)).await.unwrap();
        assert!(
            read.iter().any(|record| record.id == previous.id),
            "first sighting predates the window, but this process lived inside it; its final last_seen was not recorded"
        );
        let aged = log.facts(gate, at(75)).await.unwrap();
        assert!(
            !aged.iter().any(|record| record.id == previous.id),
            "the retirement boundary is now before the window: the newer opening epoch conservatively retires the uncertainty without inventing last_seen"
        );
    }
    fn event(gate: Gate, fact: Fact, hour: i64, payload: Value) -> Event {
        Event {
            id: Uuid::new_v4(),
            timestamp: at(hour),
            source: "policy".into(),
            kind: gate.kind(fact),
            payload,
        }
    }
    fn facts(gate: Gate) -> Vec<Event> {
        vec![
            event(
                gate,
                Fact::RecordingBegan,
                0,
                json!({"service":"policy","mode":"report","since":at(0),"instance":"old"}),
            ),
            event(
                gate,
                Fact::RecordingEnded,
                74,
                json!({"service":"policy","instance":"old","clean":true,"lost":0,"unstated":0}),
            ),
            event(
                gate,
                Fact::RecordingBegan,
                74,
                json!({"service":"policy","mode":"report","since":at(74),"instance":"current"}),
            ),
        ]
    }
    fn snapshot(gate: Gate) -> Value {
        let mut v = json!({"mode":"report","recording_since":at(74),"rows":[],"overflow":0,"not_clean":[],
            "evidence":{"recorder":true,"instance":"current","lost":0,"unstated":0,"retrying":false,"last_error":null}});
        match gate {
            Gate::MachineGate => {
                v["service"] = json!("policy");
                v["source_overflow"] = json!([]);
            }
            Gate::PolicyCheck => {
                v["switch"] = json!("policy check");
                v["file"] = json!("fixture-mode");
                v["mode_error"] = Value::Null;
                v["clean_since"] = json!(at(74));
            }
        }
        v
    }
    fn read(body: Value) -> LiveRead {
        LiveRead {
            service: "policy".into(),
            answer: Ok(body),
        }
    }
    fn judge(gate: Gate, log: Vec<Event>, body: Value) -> JoinedWindow {
        join_window(
            gate,
            &["policy".into()],
            at(3),
            at(75),
            Ok(log),
            vec![read(body)],
        )
    }
    fn refuses(v: &JoinedWindow) {
        assert!(
            !v.covers_requested_window && !v.not_clean.is_empty(),
            "{v:?}"
        );
        assert!(v.clean_since.is_none(), "{v:?}");
    }

    #[test]
    fn a_young_clean_live_process_joins_a_clean_durable_restart_window() {
        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            let v = judge(gate, facts(gate), snapshot(gate));
            assert!(v.covers_requested_window, "{v:?}");
            assert_eq!(v.clean_since, Some(at(3)));
            assert_eq!(v.log.unwrap().coverage[0].recording_since, Some(at(0)));
            assert_eq!(
                v.live[0].snapshot.as_ref().unwrap()["recording_since"],
                json!(at(74))
            );
        }
    }

    #[test]
    fn neither_a_clean_log_nor_a_clean_live_tally_is_enough() {
        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            let mut body = snapshot(gate);
            body["not_clean"] = json!(["caller keeps missing"]);
            if gate == Gate::PolicyCheck {
                body["clean_since"] = Value::Null;
            }
            let v = judge(gate, facts(gate), body);
            refuses(&v);
            assert_eq!(v.live[0].not_clean[0], "caller keeps missing");
            refuses(&judge(gate, vec![], snapshot(gate)));
            let v = join_window(
                gate,
                &["policy".into()],
                at(3),
                at(75),
                Err("audit read refused".into()),
                vec![read(snapshot(gate))],
            );
            refuses(&v);
            assert_eq!(v.log_error.as_deref(), Some("audit read refused"));
            assert!(
                v.live[0].snapshot.is_some(),
                "the readable half is retained"
            );
        }
    }

    #[test]
    fn a_recent_dirty_fact_shortens_the_full_requested_window() {
        let gate = Gate::PolicyCheck;
        let mut log = facts(gate);
        log.push(event(
            gate,
            Fact::WouldRefuse,
            20,
            json!({"service":"policy","key":{"caller":"old-caller"}}),
        ));
        let v = judge(gate, log, snapshot(gate));
        assert!(!v.covers_requested_window);
        assert_eq!(v.clean_since, Some(at(20) + Duration::microseconds(1)));
        assert_eq!(
            v.log.unwrap().dirty[0].payload["key"]["caller"],
            "old-caller"
        );
    }

    #[test]
    fn live_identity_mode_start_and_recording_health_are_all_required() {
        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            for path in ["instance", "recorder", "unstated", "retrying"] {
                let mut body = snapshot(gate);
                body["evidence"][path] = match path {
                    "instance" => json!("another-process"),
                    "recorder" => json!(false),
                    "unstated" => json!(1),
                    _ => json!(true),
                };
                refuses(&judge(gate, facts(gate), body));
            }
            for (key, value) in [
                ("mode", json!("off")),
                ("mode", json!("enforce")),
                ("recording_since", json!(at(73))),
                ("overflow", json!(1)),
            ] {
                let mut body = snapshot(gate);
                body[key] = value;
                refuses(&judge(gate, facts(gate), body));
            }
            for key in ["evidence", "not_clean", "rows", "mode", "recording_since"] {
                let mut body = snapshot(gate);
                body.as_object_mut().unwrap().remove(key);
                refuses(&judge(gate, facts(gate), body));
            }
        }
        let mut body = snapshot(Gate::MachineGate);
        body["service"] = json!("another-service");
        refuses(&judge(Gate::MachineGate, facts(Gate::MachineGate), body));
    }

    #[test]
    fn absent_duplicate_unexpected_and_dark_live_halves_never_disappear() {
        let gate = Gate::PolicyCheck;
        for reads in [
            vec![],
            vec![read(snapshot(gate)), read(snapshot(gate))],
            vec![LiveRead {
                service: "policy".into(),
                answer: Err("HTTP403: refused reader".into()),
            }],
            vec![LiveRead {
                service: "other".into(),
                answer: Ok(snapshot(gate)),
            }],
        ] {
            let v = join_window(
                gate,
                &["policy".into()],
                at(3),
                at(75),
                Ok(facts(gate)),
                reads,
            );
            refuses(&v);
            assert!(!v.live.is_empty());
        }
        for required in [
            vec![],
            vec!["policy".into(), "policy".into()],
            vec!["policy".into(), "missing".into()],
        ] {
            refuses(&join_window(
                gate,
                &required,
                at(3),
                at(75),
                Ok(facts(gate)),
                vec![read(snapshot(gate))],
            ));
        }
    }

    #[test]
    fn a_wrong_source_off_crash_or_long_gap_cannot_bridge_the_window() {
        let gate = Gate::PolicyCheck;
        let mut wrong = facts(gate);
        wrong[2].source = "another".into();
        let mut off = facts(gate);
        off[0].payload["mode"] = json!("off");
        let mut crash = facts(gate);
        crash.remove(1);
        let mut long = facts(gate);
        long[1].timestamp = at(73);
        for log in [wrong, off, crash, long] {
            let v = judge(gate, log, snapshot(gate));
            assert!(!v.covers_requested_window, "{v:?}");
        }
    }

    #[test]
    fn future_and_simulated_facts_are_not_real_coverage() {
        let gate = Gate::PolicyCheck;
        for (at_time, sim) in [(at(76), false), (at(74), true)] {
            let mut log = facts(gate);
            log[2].timestamp = at_time;
            log[2].payload["_simulated"] = json!(sim);
            refuses(&judge(gate, log, snapshot(gate)));
        }
    }

    #[test]
    fn accepted_previous_and_reader_presentations_do_not_break_the_window() {
        let gate = Gate::MachineGate;
        for presented in ["previous", "reader.current", "reader.previous"] {
            let mut body = snapshot(gate);
            body["rows"] = json!([{"peer":"fixture-peer","user":"fixture-reader","method":"GET",
                "route":"/fixture","presented":presented,"count":2,"first_seen":at(74),"last_seen":at(75)}]);
            assert!(judge(gate, facts(gate), body).covers_requested_window);
        }
    }

    #[test]
    fn contradictory_or_malformed_tallies_are_not_clean_by_omission() {
        let gate = Gate::PolicyCheck;
        for value in [Value::Null, json!({}), json!([]), json!(false)] {
            refuses(&judge(gate, facts(gate), value));
        }
        for (key, value) in [
            ("clean_since", Value::Null),
            ("mode_error", json!("mode file unreadable")),
            ("rows", json!([{"arm":"unknown"}])),
            ("not_clean", json!(null)),
            ("overflow", json!(-1)),
        ] {
            let mut body = snapshot(gate);
            body[key] = value;
            refuses(&judge(gate, facts(gate), body));
        }
    }

    #[test]
    fn a_newer_recording_start_cannot_hide_behind_missing_or_conflicting_service_provenance() {
        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            for service in [
                None,
                Some(Value::Null),
                Some(json!("people")),
                Some(json!(false)),
                Some(json!(42)),
                Some(json!("")),
            ] {
                let mut log = facts(gate);
                let mut newer = event(
                    gate,
                    Fact::RecordingBegan,
                    75,
                    json!({"mode":"report","since":at(75),"instance":"newer"}),
                );
                if let Some(service) = service {
                    newer.payload["service"] = service;
                }
                let identity = newer.id.to_string();
                log.push(newer);
                let body = snapshot(gate);
                let report = judge(gate, log, body.clone());
                refuses(&report);
                assert!(
                    report.not_clean.iter().any(|why| why.contains(&identity)),
                    "{report:?}"
                );
                assert_eq!(report.live[0].snapshot.as_ref(), Some(&body));
                assert!(
                    report.log.is_some(),
                    "the readable durable half must remain visible"
                );
            }
        }
    }

    #[tokio::test]
    async fn an_obsolete_unbound_start_expires_only_after_a_complete_later_watch() {
        use crate::gate_evidence::{GateEvidenceLog, InMemoryGateEvidence};

        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            for wrong_service in [None, Some(json!("people"))] {
                for (bad_at, start_at, since, covers) in [
                    (-25, 1, 1, true),
                    (2, 1, 1, false),
                    (-25, 4, 1, false),
                    (-25, 1, -26, false),
                ] {
                    let mut obsolete = event(
                        gate,
                        Fact::RecordingBegan,
                        bad_at,
                        json!({"mode":"report","since":at(bad_at),"instance":"obsolete"}),
                    );
                    if let Some(service) = wrong_service.clone() {
                        obsolete.payload["service"] = service;
                    }
                    let identity = obsolete.id;
                    let current = event(
                        gate,
                        Fact::RecordingBegan,
                        start_at,
                        json!({"service":"policy","mode":"report","since":at(since),"instance":"current"}),
                    );
                    let log = InMemoryGateEvidence::new(vec![obsolete, current])
                        .facts(gate, at(3))
                        .await
                        .unwrap();
                    assert!(log.iter().any(|event| event.id == identity));
                    let mut body = snapshot(gate);
                    body["recording_since"] = json!(at(since));
                    if gate == Gate::PolicyCheck {
                        body["clean_since"] = json!(at(since));
                    }
                    let report = judge(gate, log, body);
                    assert_eq!(report.covers_requested_window, covers, "{report:?}");
                    if covers {
                        assert_eq!(report.clean_since, Some(at(3)));
                        assert!(report.not_clean.is_empty());
                    } else {
                        refuses(&report);
                        assert!(
                            report
                                .not_clean
                                .iter()
                                .any(|why| why.contains(&identity.to_string()))
                        );
                    }
                }
            }
        }
    }

    // ── The launch record (backlog 93e0814a, 2026-10-06) ──────────────
    // Three gated services stand for the registry: one the launcher
    // starts, one it skips for a module, and the sim daemon's port.
    fn gated() -> Vec<(String, Vec<String>)> {
        vec![
            (
                "policy".into(),
                vec!["boss-policy-api".into(), "boss-policy".into()],
            ),
            (
                "assets".into(),
                vec!["boss-assets-api".into(), "boss-assets".into()],
            ),
            (
                "sim-control".into(),
                vec!["boss-sim-control-api".into(), "boss-sim-control".into()],
            ),
        ]
    }
    const RECORD: &str = "boss-launch-record v1\n\
        start boss-policy-api\n\
        skip boss-assets-api module equipment is not on in the tenant manifest\n\
        skip boss-sim-control the tick daemon is parked (BOSS_SIM_ENABLED is not on)\n\
        end 3\n";

    #[test]
    fn a_started_service_is_required_and_only_a_recorded_skip_excuses() {
        let roster = launch_roster(&gated(), Ok(RECORD));
        assert_eq!(roster.errors, Vec::<String>::new());
        assert_eq!(roster.required, ["policy"]);
        assert_eq!(
            roster.not_launched,
            [
                NotLaunched {
                    service: "assets".into(),
                    binary: "boss-assets-api".into(),
                    reason: "module equipment is not on in the tenant manifest".into(),
                },
                NotLaunched {
                    service: "sim-control".into(),
                    binary: "boss-sim-control".into(),
                    reason: "the tick daemon is parked (BOSS_SIM_ENABLED is not on)".into(),
                },
            ]
        );
        // A start under either of a service's binaries requires it,
        // whatever a skip line for the other one says.
        let both = RECORD.replace("end 3", "start boss-assets\nend 4");
        let roster = launch_roster(&gated(), Ok(&both));
        assert_eq!(roster.required, ["policy", "assets"]);
        assert_eq!(roster.not_launched.len(), 1);
    }

    #[test]
    fn a_record_that_cannot_be_read_whole_excuses_nothing() {
        let all = ["policy", "assets", "sim-control"];
        let unset: Result<&str, String> =
            Err("BOSS_LAUNCH_RECORD is unset: this process was not started by the launcher".into());
        let cases: Vec<(&str, Result<&str, String>)> = vec![
            ("unset or unreadable", unset),
            ("empty", Ok("")),
            (
                "no version line",
                Ok("skip boss-assets-api module equipment\nend 1\n"),
            ),
            (
                "truncated before its end line",
                Ok(
                    "boss-launch-record v1\nstart boss-policy-api\nskip boss-assets-api module equipment\n",
                ),
            ),
            (
                "miscounted",
                Ok(
                    "boss-launch-record v1\nstart boss-policy-api\nskip boss-assets-api module equipment\nend 3\n",
                ),
            ),
            (
                "a skip with no reason",
                Ok("boss-launch-record v1\nstart boss-policy-api\nskip boss-assets-api\nend 2\n"),
            ),
            (
                "an unknown verb",
                Ok(
                    "boss-launch-record v1\nstart boss-policy-api\nabsent boss-assets-api because\nend 2\n",
                ),
            ),
            (
                "one binary decided twice",
                Ok(
                    "boss-launch-record v1\nskip boss-assets-api module equipment\nstart boss-assets-api\nend 2\n",
                ),
            ),
        ];
        for (name, record) in cases {
            let roster = launch_roster(&gated(), record);
            assert_eq!(roster.required, all, "{name}: every service is required");
            assert!(roster.not_launched.is_empty(), "{name}: {roster:?}");
            assert_eq!(roster.errors.len(), 1, "{name}: {roster:?}");
        }
        // A service the record does not name at all stays required and
        // is named: a registry row newer than the launcher's roster
        // cannot be excused by silence.
        let silent = "boss-launch-record v1\nstart boss-policy-api\nskip boss-assets-api module equipment\nend 2\n";
        let roster = launch_roster(&gated(), Ok(silent));
        assert_eq!(roster.required, ["policy", "sim-control"]);
        assert!(roster.errors[0].contains("sim-control"), "{roster:?}");
    }

    const TWO: &str = "boss-launch-record v1\n\
        start boss-policy-api\n\
        skip boss-assets-api module equipment is not on in the tenant manifest\n\
        end 2\n";

    fn launched(
        probes: Vec<PortProbe>,
        assets: Option<Result<Value, String>>,
        extra: Vec<Event>,
    ) -> JoinedWindow {
        let gate = Gate::MachineGate;
        let roster = launch_roster(&gated()[..2], Ok(TWO));
        assert_eq!(roster.required, ["policy"]);
        let mut log = facts(gate);
        log.extend(extra);
        let mut reads = vec![read(snapshot(gate))];
        reads.extend(assets.map(|answer| LiveRead {
            service: "assets".into(),
            answer,
        }));
        join_launched_window(gate, &roster, &probes, at(3), at(75), Ok(log), reads)
    }
    fn probe(listening: Result<bool, String>) -> Vec<PortProbe> {
        vec![PortProbe {
            service: "assets".into(),
            listening,
        }]
    }
    fn refused() -> Option<Result<Value, String>> {
        Some(Err("GET http://127.0.0.1:7600: connection refused".into()))
    }

    #[test]
    fn a_service_the_launcher_did_not_start_is_not_a_gap() {
        let v = launched(probe(Ok(false)), refused(), vec![]);
        assert!(v.covers_requested_window, "{:?}", v.not_clean);
        assert_eq!(v.required_services, ["policy"]);
        assert_eq!(v.not_launched.len(), 1);
        assert_eq!(v.not_launched[0].service, "assets");
        assert_eq!(v.not_launched[0].listening, Some(false));
        assert!(v.not_launched[0].not_clean.is_empty());
        // The complete observation still carries the excused port's
        // read, and says which names were excused.
        let observation = v.observation.unwrap();
        assert_eq!(observation.not_launched, ["assets"]);
        assert_eq!(observation.reads.len(), 2);
        // Without the record the same service is the gap it always was.
        let all = join_window(
            Gate::MachineGate,
            &["policy".into(), "assets".into()],
            at(3),
            at(75),
            Ok(facts(Gate::MachineGate)),
            vec![
                read(snapshot(Gate::MachineGate)),
                LiveRead {
                    service: "assets".into(),
                    answer: Err("connection refused".into()),
                },
            ],
        );
        refuses(&all);
    }

    #[test]
    fn an_excused_service_that_runs_makes_the_window_not_clean() {
        let gate = Gate::MachineGate;
        // The control this car exists for: a record cannot drop a
        // running service. Each row is one way an excused service shows
        // a process, or one way its absence was left unobserved.
        let mut began = event(
            gate,
            Fact::RecordingBegan,
            10,
            json!({"service":"assets","mode":"report","since":at(10),"instance":"stray"}),
        );
        began.source = "assets".into();
        let mut named = event(gate, Fact::WouldRefuse, 11, json!({"service":"assets"}));
        named.source = "policy".into();
        let cases: Vec<(&str, JoinedWindow)> = vec![
            (
                "its port accepts a connection",
                launched(probe(Ok(true)), refused(), vec![]),
            ),
            (
                "its port could not be probed",
                launched(probe(Err("timed out".into())), refused(), vec![]),
            ),
            (
                "its port was not probed",
                launched(vec![], refused(), vec![]),
            ),
            (
                "its port was probed twice",
                launched(
                    [probe(Ok(false)), probe(Ok(false))].concat(),
                    refused(),
                    vec![],
                ),
            ),
            (
                "it answered its tally read",
                launched(probe(Ok(false)), Some(Ok(snapshot(gate))), vec![]),
            ),
            (
                "its tally read is missing",
                launched(probe(Ok(false)), None, vec![]),
            ),
            (
                "it recorded a gate fact inside the window",
                launched(probe(Ok(false)), refused(), vec![began]),
            ),
            (
                "a gate fact inside the window names it",
                launched(probe(Ok(false)), refused(), vec![named]),
            ),
        ];
        for (name, v) in cases {
            assert!(
                !v.covers_requested_window && v.clean_since.is_none(),
                "{name}: {v:?}"
            );
            assert!(
                v.not_clean.iter().any(|why| why.starts_with("assets: ")),
                "{name}: the reason names the service: {:?}",
                v.not_clean
            );
            assert!(!v.not_launched[0].not_clean.is_empty(), "{name}");
        }
        // A fact older than the window is the past, not a contradiction.
        let mut old = event(
            gate,
            Fact::RecordingBegan,
            1,
            json!({"service":"assets","mode":"report","since":at(1),"instance":"retired"}),
        );
        old.source = "assets".into();
        let v = launched(probe(Ok(false)), refused(), vec![old]);
        assert!(v.covers_requested_window, "{:?}", v.not_clean);
    }

    #[test]
    fn a_roster_that_could_not_be_derived_is_never_clean() {
        let gate = Gate::MachineGate;
        // Every required service answers cleanly and the window is still
        // refused, naming why the deployed set is unknown.
        let roster = launch_roster(
            &gated()[..1],
            Err("BOSS_LAUNCH_RECORD is unset: this process was not started by the launcher".into()),
        );
        let v = join_launched_window(
            gate,
            &roster,
            &[],
            at(3),
            at(75),
            Ok(facts(gate)),
            vec![read(snapshot(gate))],
        );
        refuses(&v);
        assert_eq!(v.roster_errors.len(), 1);
        assert!(
            v.not_clean
                .iter()
                .any(|why| why.starts_with("launch record: BOSS_LAUNCH_RECORD is unset")),
            "{:?}",
            v.not_clean
        );
        // A roster that names one service on both sides is ambiguous.
        let both = LaunchRoster {
            required: vec!["policy".into()],
            not_launched: vec![NotLaunched {
                service: "policy".into(),
                binary: "boss-policy-api".into(),
                reason: "fixture".into(),
            }],
            errors: vec![],
        };
        refuses(&join_launched_window(
            gate,
            &both,
            &[PortProbe {
                service: "policy".into(),
                listening: Ok(false),
            }],
            at(3),
            at(75),
            Ok(facts(gate)),
            vec![read(snapshot(gate))],
        ));
    }
}
