//! The two halves of design 21946380's gate window, read together.
//! This reports evidence; neither a clean answer nor a caller-chosen
//! lookback authorizes a mode change. The log arithmetic stays in
//! `gate_evidence::window`; here a live tally must match its latest
//! sourced recording start, and every required service must answer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;

use crate::event::Event;
pub use crate::gate_evidence::DrainedSlot;
use crate::gate_evidence::{
    EvidenceHealth, Fact, Gate, Window, actor_role_tally_reasons, actor_role_would_refuse,
    drain_window, policy_tally_reasons, policy_watch_window, window,
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
    /// EVERY launch roster generation the window was judged under,
    /// oldest first (design 3cc6152a; backlog 14fe115c): the stretches
    /// its hours fall in, and whatever else a required service's own
    /// judged start stated (`judged_generations`). A clean window names
    /// exactly one, and it is the one this reader's own launch record
    /// digests to. Empty for a window joined without a launch record.
    #[serde(default)]
    pub roster_generations: Vec<RosterGeneration>,
}

/// One stretch of process starts that stated the same launch roster
/// generation — or, with `generation` absent, stated none — or the one
/// start of a required service's process that stated something else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterGeneration {
    /// `sha256:<hex>` over the selection (`roster_generation`); `None`
    /// when the starts in this stretch stated none.
    pub generation: Option<String>,
    /// The first process start of the stretch: the instant the selection
    /// is on the record from.
    pub since: DateTime<Utc>,
    /// That start's immutable record and the service that stated it.
    pub stated_by: Uuid,
    pub service: String,
    /// Why no generation was stated, in the starting process's own words
    /// when it gave any.
    pub unstated: Option<String>,
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
    /// The record's generation (`roster_generation`) when it was read
    /// whole. `None` beside `errors` is a record that could not be read;
    /// `None` with no error is a roster that consulted no record, and
    /// such a window is judged under no generation rule at all (every
    /// service is required, so there is no excuse to inherit).
    pub generation: Option<String>,
}

impl LaunchRoster {
    /// No record consulted: every named service is required.
    pub fn all_required(services: Vec<String>) -> Self {
        Self {
            required: services,
            not_launched: Vec::new(),
            errors: Vec::new(),
            generation: None,
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
    let seen = match record.and_then(launch_decisions) {
        Ok(seen) => seen,
        Err(error) => {
            return LaunchRoster {
                required: all(),
                not_launched: Vec::new(),
                errors: vec![error],
                generation: None,
            };
        }
    };
    let mut roster = LaunchRoster {
        generation: Some(generation_of(&seen)),
        ..LaunchRoster::default()
    };
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

/// The record's decisions, `(binary, Some(reason))` for a skip and
/// `(binary, None)` for a start — or why the record cannot be taken at
/// its word. The one parse `launch_roster` and `roster_generation` share.
fn launch_decisions(text: &str) -> Result<Vec<(&str, Option<&str>)>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
        return Err("the launch record is empty".into());
    };
    if *first != LAUNCH_RECORD_HEADER || lines.len() < 2 {
        return Err(format!(
            "the launch record does not begin with `{LAUNCH_RECORD_HEADER}`"
        ));
    }
    let decisions = &lines[1..lines.len() - 1];
    if last
        .strip_prefix("end ")
        .and_then(|n| n.parse::<usize>().ok())
        != Some(decisions.len())
    {
        return Err(format!(
            "the launch record is truncated or miscounted: {} decision line(s), last line `{last}`",
            decisions.len()
        ));
    }
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
            _ => return Err(format!("the launch record has a malformed line `{line}`")),
        };
        if seen.iter().any(|(binary, _)| *binary == entry.0) {
            return Err(format!(
                "the launch record names {} more than once",
                entry.0
            ));
        }
        seen.push(entry);
    }
    Ok(seen)
}

/// The variable the launcher exports with the path of the record it
/// wrote (`services-launcher.sh`).
pub const LAUNCH_RECORD_ENV: &str = "BOSS_LAUNCH_RECORD";

/// The first line of what a generation digests; never written to a file.
pub const ROSTER_GENERATION_HEADER: &str = "boss-launch-roster v1";

/// The GENERATION of a launch record: which selection it is (design
/// 3cc6152a, "the declaration must carry its generation and provenance
/// in the event record"; backlog 14fe115c, 2026-10-08).
///
/// WHAT IS HASHED, exactly: the text `boss-launch-roster v1\n` followed
/// by one line per decision in the record, `start <binary>\n` or
/// `skip <binary>\n`, the lines sorted by binary name (bytewise); the
/// answer is `sha256:` and the lowercase hex of that text's SHA-256.
/// A skip's REASON is not in it and neither is the record's order: the
/// selection is which binaries this launch starts, so a reworded reason
/// or a reordered launcher array is the same generation, while one
/// binary started that was skipped, skipped that was started, added or
/// removed is another. The image's commit is deliberately not in it
/// either — every train ships a new commit, and the same selection
/// relaunched must not start a new generation.
pub fn roster_generation(record: &str) -> Result<String, String> {
    launch_decisions(record).map(|seen| generation_of(&seen))
}

fn generation_of(seen: &[(&str, Option<&str>)]) -> String {
    use sha2::Digest;
    let mut lines: Vec<(&str, &str)> = seen
        .iter()
        .map(|(binary, skip)| (*binary, if skip.is_some() { "skip" } else { "start" }))
        .collect();
    lines.sort();
    let mut text = format!("{ROSTER_GENERATION_HEADER}\n");
    for (binary, verb) in lines {
        text.push_str(&format!("{verb} {binary}\n"));
    }
    format!(
        "sha256:{}",
        hex::encode(sha2::Sha256::digest(text.as_bytes()))
    )
}

/// The most a launch record is read. The launcher writes one short line
/// per binary (about thirty today, under 4 KiB); a file past this is
/// not a launch record.
pub const MAX_LAUNCH_RECORD_BYTES: u64 = 64 * 1024;

/// The record a process was launched under, read from the path the
/// launcher exported — or why there is none. An edge: called once, where
/// a gate is mounted.
pub fn read_launch_record(path: Option<String>) -> Result<String, String> {
    let path = path.filter(|path| !path.trim().is_empty()).ok_or_else(|| {
        format!(
            "{LAUNCH_RECORD_ENV} is unset: this process was not started by the launcher, so what is deployed is unknown"
        )
    })?;
    read_launch_record_file(std::path::Path::new(&path))
}

/// The record at `path`, BOUNDED (review 6858ef1d, N3): this runs at
/// every gated service's boot and on every window read, synchronously.
/// Anything that is not a regular file is refused before it is opened —
/// `open` on a FIFO blocks until a writer appears, which would hang the
/// boot — and no more than [`MAX_LAUNCH_RECORD_BYTES`] is ever read.
/// Every failure is a reason, never a refusal to start: the caller
/// states it as an error stamp, and the window then cannot read clean.
pub fn read_launch_record_file(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let unreadable =
        |why: String| format!("the launch record {} is unreadable: {why}", path.display());
    let meta = std::fs::metadata(path).map_err(|e| unreadable(e.to_string()))?;
    if !meta.is_file() {
        return Err(unreadable("it is not a regular file".into()));
    }
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_LAUNCH_RECORD_BYTES + 1).read_to_end(&mut raw))
        .map_err(|e| unreadable(e.to_string()))?;
    if raw.len() as u64 > MAX_LAUNCH_RECORD_BYTES {
        return Err(unreadable(format!(
            "it is larger than {MAX_LAUNCH_RECORD_BYTES} bytes"
        )));
    }
    String::from_utf8(raw).map_err(|_| unreadable("it is not UTF-8".into()))
}

/// What a process states about its launch roster on every
/// `machine_gate.recording_began` (`Evidence::state_roster`): the
/// generation of the record it was started under and the commit of its
/// image, or why it can name no generation. A process that cannot read
/// its record still starts and still states its start — the window then
/// cannot be read clean, and says this reason.
pub fn roster_stamp(record: Result<&str, String>, commit: Option<&str>) -> Value {
    match record.and_then(roster_generation) {
        Ok(generation) => serde_json::json!({"generation": generation, "commit": commit}),
        Err(error) => serde_json::json!({"error": error, "commit": commit}),
    }
}

/// The key a `recording_began` carries its roster stamp under.
pub const ROSTER_FIELD: &str = "roster";

/// Which generation(s) the hours of a window were judged under, read
/// from the process starts on the log, and why that is not the one
/// generation `declared` when it is not.
///
/// THE RULE HAS TWO HALVES, and a clean reading passes both.
///
/// PER PROCESS (review 6858ef1d, B1). A process does not end when
/// another service starts. So for every REQUIRED service, the start of
/// the process that was running when the window opened — its newest at
/// or before `from` — and each of its starts inside the window must
/// state exactly `declared`, the generation of the record this reader
/// judged the roster from. An error stamp, no stamp or another
/// generation on any of them is a reason naming the service, the
/// instant and what was stated, whatever any other service stated and
/// whenever. A required service with no start on the log is not judged
/// here: the coverage half of the window already reads it as no watch.
///
/// ACROSS THE POD. The starts of every service in time order fall into
/// stretches of one generation each. The window's hours belong to the
/// last stretch and to every stretch whose successor began after the
/// window opened; there must be exactly one, stating `declared`. This
/// half is the only judge of a start by a service this reader's record
/// does NOT require — the service an old selection ran and the new one
/// excuses, whose hours the new selection must not inherit — and it is
/// what lets such a service's last start become the past once the pod
/// has relaunched before the window opens. It is not a judge of
/// required services: there the per-process half decides.
///
/// NEITHER HALF READS THE ORDER OF A BOOT. Starts at one instant are
/// ordered by what they state, never by event id, with a start that
/// does not state `declared` taken last — the reading in which it is
/// still the newest word — so the same facts give the same verdict.
///
/// A relaunch of the same selection (a pod restart, every train) states
/// the same generation and extends the stretch: same digest, same
/// generation. A start that states none — an older build, an unreadable
/// record — is a stretch of its own, so the mechanism's own first boot
/// does not backdate itself over the hours before it (design 3cc6152a:
/// "do not backdate coverage when introducing this mechanism").
///
/// OPERATING RULE (review 6858ef1d, N2). The generation digests EVERY
/// decision line of the launch record, gated or not. Any change to the
/// launcher's SERVICES array or to the tenant's modules — a relay, a
/// non-gated daemon added, removed or re-selected — is therefore a new
/// generation, and the first boot on it restarts the clean clock of
/// every enforcement row then counting (26 or 72 hours), although the
/// required set did not change. Do not land one while a row's clock is
/// counting unless restarting it is intended.
pub fn judged_generations(
    gate: Gate,
    facts: &[Event],
    required: &[String],
    from: DateTime<Utc>,
    declared: &str,
) -> (Vec<RosterGeneration>, Vec<String>) {
    let began = gate.kind(Fact::RecordingBegan);
    let stated = |start: &Event| {
        let stamp = start.payload.get(ROSTER_FIELD);
        let generation = stamp
            .and_then(|stamp| stamp.get("generation"))
            .and_then(Value::as_str)
            .filter(|generation| !generation.is_empty())
            .map(str::to_string);
        let unstated = generation.is_none().then(|| {
            stamp
                .and_then(|stamp| stamp.get("error"))
                .and_then(Value::as_str)
                .map_or_else(
                    || "the process start carries no roster stamp".to_string(),
                    str::to_string,
                )
        });
        RosterGeneration {
            generation,
            since: start.timestamp,
            stated_by: start.id,
            service: start.source.clone(),
            unstated,
        }
    };
    let off = |g: &RosterGeneration| g.generation.as_deref() != Some(declared);
    let mut starts: Vec<(&Event, RosterGeneration)> = facts
        .iter()
        .filter(|e| e.kind == began)
        .map(|e| (e, stated(e)))
        .collect();
    // The event id orders only starts that state the same thing at the
    // same instant, where it cannot change what is read.
    starts.sort_by(|(_, a), (_, b)| {
        (a.since, off(a), &a.generation, &a.unstated, a.stated_by).cmp(&(
            b.since,
            off(b),
            &b.generation,
            &b.unstated,
            b.stated_by,
        ))
    });
    let mut stretches: Vec<RosterGeneration> = Vec::new();
    for (_, start) in &starts {
        if stretches
            .last()
            .is_some_and(|last| last.generation.is_some() && last.generation == start.generation)
        {
            continue;
        }
        // Two unstated starts in a row are one unstated stretch.
        if start.generation.is_none() && stretches.last().is_some_and(|l| l.generation.is_none()) {
            continue;
        }
        stretches.push(start.clone());
    }
    // Keep the last stretch and each one whose successor began inside
    // the window: those are the stretches the window's hours fall in.
    let keep: Vec<bool> = (0..stretches.len())
        .map(|i| stretches.get(i + 1).is_none_or(|next| next.since > from))
        .collect();
    let mut keep = keep.into_iter();
    stretches.retain(|_| keep.next().unwrap_or(true));
    let name = |g: &RosterGeneration| match (&g.generation, &g.unstated) {
        (Some(generation), _) => format!("generation {generation}"),
        (None, why) => format!(
            "no stated generation ({})",
            why.as_deref().unwrap_or("reason unknown")
        ),
    };
    let mut why = Vec::new();
    for pair in stretches.windows(2) {
        why.push(format!(
            "the window spans a change of launch roster at {}: {} before it (from {}), {} from it \
             (record {} from {}) — hours watched under one selection are not inherited by another",
            pair[1].since.to_rfc3339(),
            name(&pair[0]),
            pair[0].since.to_rfc3339(),
            name(&pair[1]),
            pair[1].stated_by,
            pair[1].service,
        ));
    }
    match stretches.last() {
        None => why.push(
            "no process start on the log states a launch roster generation, so the selection \
             these hours were watched under is not on the record"
                .into(),
        ),
        Some(last) if last.generation.is_none() => {
            if stretches.len() == 1 {
                why.push(format!(
                    "the process starts this window is judged under state {} since {} (record {} from {})",
                    name(last),
                    last.since.to_rfc3339(),
                    last.stated_by,
                    last.service,
                ));
            }
        }
        Some(last) if last.generation.as_deref() != Some(declared) => why.push(format!(
            "this reader's launch record is generation {declared}, while the newest stretch of \
             process starts on the log states {} since {} (record {} from {})",
            name(last),
            last.since.to_rfc3339(),
            last.stated_by,
            last.service,
        )),
        Some(_) => {}
    }
    // The per-process half. One line per required service — the first
    // start that is off and how many are — so a window reaching back
    // over many boots stays readable.
    let mut services: Vec<&String> = required.iter().collect();
    services.sort();
    services.dedup();
    for service in services {
        // The log port's own grouping (`payload.service`), and the
        // source beside it: a start is this service's by either name.
        let mine: Vec<&RosterGeneration> = starts
            .iter()
            .filter(|(event, _)| {
                &event.source == service
                    || event.payload.get("service").and_then(Value::as_str)
                        == Some(service.as_str())
            })
            .map(|(_, start)| start)
            .collect();
        let opened = mine.iter().map(|s| s.since).filter(|at| *at <= from).max();
        let judged: Vec<&RosterGeneration> = mine
            .into_iter()
            .filter(|s| s.since > from || Some(s.since) == opened)
            .collect();
        let wrong: Vec<&RosterGeneration> = judged.iter().copied().filter(|s| off(s)).collect();
        let Some(first) = wrong.first() else {
            continue;
        };
        why.push(format!(
            "{service}: {} of the {} process start(s) of it this window is judged by (the one \
             running when the window opened and each one inside it) do not state this reader's \
             generation {declared}; its start at {} (record {}) states {}",
            wrong.len(),
            judged.len(),
            first.since.to_rfc3339(),
            first.stated_by,
            name(first),
        ));
        // Every generation judged is named: one the stretches dropped
        // as the past is not the past for the process that stated it.
        for start in wrong {
            if !stretches
                .iter()
                .any(|kept| kept.generation == start.generation)
            {
                stretches.push(start.clone());
            }
        }
    }
    stretches.sort_by(|a, b| {
        (a.since, off(a), &a.generation, &a.unstated, a.stated_by).cmp(&(
            b.since,
            off(b),
            &b.generation,
            &b.unstated,
            b.stated_by,
        ))
    });
    (stretches, why)
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

/// One row of a service's actor-role report: the observation it keyed
/// on, and what the producer says of it. Every field the rule reads is
/// required; an older producer's row without them is malformed, never a
/// clean one.
#[derive(Deserialize)]
struct RoleRow {
    observation: RoleShape,
    count: u64,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    would_refuse: Option<String>,
}

#[derive(Deserialize)]
struct RoleShape {
    lookup_status: String,
    action: String,
    asserted_allowed: Option<bool>,
    recorded_allowed: Option<bool>,
    would_deny: Option<bool>,
    would_change_scope: Option<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Enforce,
    /// A drain of one slot set's `previous` — the estate token's, or the
    /// probe-reader credential's.
    Previous(DrainedSlot),
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
    // What a drain waits out: `previous`, or `reader.previous`.
    let drained = match purpose {
        Purpose::Previous(slot) => Some(slot.presented()),
        _ => None,
    };
    let previous = drained.is_some();
    // An actor-role answer is a service's report route, whose tally is
    // its `report` member; the other two gates answer the tally itself.
    let tally_body = match gate {
        Gate::ActorRole => body
            .get("report")
            .ok_or("actor-role answer has no report")?,
        Gate::MachineGate | Gate::PolicyCheck => body,
    };
    let tally: CommonTally = serde_json::from_value(tally_body.clone())
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
                    .any(|r| Some(r.key.presented) == drained && r.last_seen >= from)
                    || misses.source_overflow.iter().any(|r| {
                        Some(r.presented) == drained && r.last_seen.is_none_or(|at| at >= from)
                    })
                {
                    why.push(format!(
                        "live tally still holds a {} presentation",
                        drained.map_or("previous", Presented::name)
                    ));
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
        Gate::ActorRole => {
            if body.get("service").and_then(Value::as_str) != Some(service) {
                why.push(format!(
                    "actor-role answer names {}, requested {service}",
                    body.get("service").unwrap_or(&Value::Null)
                ));
            }
            // Every row is re-judged by the one rule the producer keys
            // its facts on: a tally is clean because of what its rows
            // ARE, never because of what it says of them.
            let (mut held, mut observations) = (0_usize, 0_u64);
            for row in &tally.rows {
                let row: RoleRow = serde_json::from_value(row.clone())
                    .map_err(|e| format!("actor-role tally row is malformed: {e}"))?;
                // No `last_seen > now` here, unlike the other gates'
                // rows: this tally also counts every MATCHING request,
                // so on a busy service a row is touched between the
                // observation's instant and the read of the tally, and
                // refusing that would refuse every read.
                if row.count == 0
                    || row.first_seen > row.last_seen
                    || row.first_seen < tally.recording_since
                {
                    why.push("actor-role tally has an invalid row count or time".into());
                }
                let shape = &row.observation;
                let judged = actor_role_would_refuse(
                    &shape.lookup_status,
                    &shape.action,
                    shape.asserted_allowed,
                    shape.recorded_allowed,
                    shape.would_deny,
                    shape.would_change_scope,
                );
                if judged != row.would_refuse.as_deref() {
                    why.push(format!(
                        "actor-role tally row says would_refuse {:?} and the rule says {judged:?}: \
                         the row disagrees with its shape",
                        row.would_refuse
                    ));
                }
                if judged.is_some() {
                    held += 1;
                    observations = observations.saturating_add(row.count);
                }
            }
            if held != 0 {
                why.push(format!(
                    "actor-role tally still holds {held} shape(s) that `enforce` answers differently"
                ));
            }
            let expected = actor_role_tally_reasons(
                tally.mode,
                held,
                observations,
                tally.overflow,
                &tally.evidence,
            );
            if expected != tally.not_clean {
                why.push("actor-role tally reasons disagree with its complete snapshot".into());
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
            generation: roster.generation.as_deref(),
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
    /// The generation of the record the roster was read from.
    generation: Option<&'a str>,
}

impl Excused<'_> {
    const NONE: Self = Excused {
        not_launched: &[],
        errors: &[],
        probes: &[],
        generation: None,
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
    join_drain_window(DrainedSlot::Estate, required, from, now, facts, reads)
}

/// [`join_previous_window`] for either of a machine gate's slot sets
/// (design b35c22b4; backlog d26515c5): the same complete observation,
/// the same health checks, the same retired-epoch rule — judged against
/// presentations of `drained`'s `previous` and of nothing else. The
/// probe-reader credential's rotation drains `reader.previous` on it, and
/// the estate token's `previous` neither holds that drain nor is held by
/// it: each value has its own holders, and a caller still on one says
/// nothing about the other.
pub fn join_drain_window(
    drained: DrainedSlot,
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
        Purpose::Previous(drained),
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
    // The actor-role window is held to both of the rules below under its
    // enforce reading as well: each of its facts is bound to the process
    // start that stated it, and a shape first seen before the window by
    // a process that ended inside it is never read as unused (backlog
    // e0bdba74). The machine gate's and the policy check's enforce
    // windows are judged as they were.
    let bound_to_a_process = purpose != Purpose::Enforce || gate == Gate::ActorRole;
    if bound_to_a_process {
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
    if bound_to_a_process {
        for event in facts.iter().filter(|event| event.timestamp < from) {
            let relevant = match purpose {
                // The one definition the log's window reads too
                // (`gate_evidence::drain_window`).
                Purpose::Previous(slot) => slot.dirtied_by(gate, event),
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
                Purpose::Enforce => gate.fact_of(&event.kind).is_some_and(Fact::dirties),
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
        Purpose::Previous(slot) => drain_window(slot, &services, from, now, &facts),
        Purpose::PolicyWatch => policy_watch_window(&services, from, now, &facts),
    });
    if let Some(log) = &log {
        why.extend(log.not_clean.iter().map(|e| format!("log: {e}")));
    }
    // The roster's excuses hold only for hours watched under the roster
    // that makes them (design 3cc6152a; `judged_generations`). Not an
    // input error: the inputs are whole, and the watch under this
    // selection began too recently — the same class as a young process.
    //
    // THE MACHINE GATE'S RULE ONLY. The stamp it reads rides
    // `machine_gate.recording_began` and no other gate's start
    // (`machine_gate::mount_launched` is its one writer). The actor-role
    // window takes its roster from the same record (backlog e0bdba74)
    // and its starts state no stamp, so judged here it would name every
    // start unstamped and never read clean. Its excuses are held to the
    // port, the read and the log, as they were; that an actor-role
    // excuse is NOT yet bound to the selection that made it is a gap
    // this line leaves named, not closed.
    let roster_generations = match excused.generation {
        Some(declared) if gate == Gate::MachineGate => {
            let (stretches, gaps) =
                judged_generations(gate, &facts, &required_services, from, declared);
            why.extend(gaps.into_iter().map(|gap| format!("launch roster: {gap}")));
            stretches
        }
        _ => Vec::new(),
    };
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
        roster_generations,
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
            // Its fixtures, and its tests, are
            // tests/the_actor_role_window_is_read_per_process.rs.
            Gate::ActorRole => unreachable!("no test here asks for an actor-role tally"),
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

    // ---- The probe-reader credential's drain (design b35c22b4) ----

    fn drain(drained: DrainedSlot, log: Vec<Event>, body: Value) -> JoinedWindow {
        let mut misses: Misses = serde_json::from_value(body).unwrap();
        misses.judge();
        join_drain_window(
            drained,
            &["policy".into()],
            at(51),
            at(75),
            Ok(log),
            vec![read(serde_json::to_value(misses).unwrap())],
        )
    }

    fn row(presented: &str, last: i64) -> Value {
        json!({"peer":"192.0.2.15","user":"automation:run-car-probe-reader","method":"GET",
            "route":"/api/jobs","presented":presented,"count":2,"first_seen":at(74),"last_seen":at(last)})
    }

    /// A caller still presenting the superseded reader value holds the
    /// READER's drain — as a keyed row, and as a count past its source's
    /// share, where a reader presentation lands (review 177b4976, B1) —
    /// and holds nothing of the estate's. Before this the one drain there
    /// was read `previous` alone, so a reader rotation would have blanked
    /// `reader.previous` under a probe still sending it.
    #[test]
    fn a_live_reader_previous_holds_the_readers_drain_and_not_the_estates() {
        let gate = Gate::MachineGate;
        let mut keyed = snapshot(gate);
        keyed["rows"] = json!([row("reader.previous", 75)]);
        let mut spilled = snapshot(gate);
        spilled["source_overflow"] = json!([{"source":"192.0.2.15","presented":"reader.previous",
            "count":3,"first_seen":at(74),"last_seen":at(75)}]);
        for body in [keyed, spilled] {
            let reader = drain(DrainedSlot::Reader, facts(gate), body.clone());
            assert!(!reader.covers_requested_window, "{reader:?}");
            assert!(
                reader
                    .not_clean
                    .iter()
                    .any(|why| why.contains("reader.previous")),
                "the hold names the slot: {:?}",
                reader.not_clean
            );
            let estate = drain(DrainedSlot::Estate, facts(gate), body);
            assert!(estate.covers_requested_window, "{estate:?}");
        }
    }

    /// The other direction: the estate's `previous`, and the reader's own
    /// `current` and `next` (every probe read, and the broker's own verify
    /// asks), say nothing about the superseded reader value.
    #[test]
    fn an_estate_previous_and_live_reader_reads_do_not_hold_the_readers_drain() {
        let gate = Gate::MachineGate;
        for presented in ["previous", "reader.current", "reader.next"] {
            let mut body = snapshot(gate);
            body["rows"] = json!([row(presented, 75)]);
            let reader = drain(DrainedSlot::Reader, facts(gate), body.clone());
            assert!(reader.covers_requested_window, "{presented}: {reader:?}");
            let estate = drain(DrainedSlot::Estate, facts(gate), body);
            assert_eq!(
                estate.covers_requested_window,
                presented != "previous",
                "{presented}: the estate's drain reads exactly as it did"
            );
        }
    }

    fn sighting(presented: Value, hour: i64, instance: &str, since: i64) -> Event {
        event(
            Gate::MachineGate,
            Fact::ReaderPresented,
            hour,
            json!({"service":"policy","instance":instance,"mode":"report",
                "recording_since":at(since),"key":{"presented":presented}}),
        )
    }

    /// The LOG half. A `reader_presented` naming `reader.previous` inside
    /// the window ends the reader's drain; one naming another reader slot
    /// does not; one whose slot cannot be read does, because a
    /// presentation nobody can place may be the old value. None of them
    /// touches the estate's drain.
    #[test]
    fn the_log_holds_the_readers_drain_on_a_reader_previous_fact_alone() {
        let gate = Gate::MachineGate;
        for (presented, holds) in [
            (json!("reader.previous"), true),
            (json!("reader.current"), false),
            (json!("reader.next"), false),
            (Value::Null, true),
            (json!("a slot nobody defined"), true),
        ] {
            let mut log = facts(gate);
            log.push(sighting(presented.clone(), 60, "old", 0));
            let reader = drain(DrainedSlot::Reader, log.clone(), snapshot(gate));
            assert_eq!(
                !reader.covers_requested_window, holds,
                "{presented}: {reader:?}"
            );
            let estate = drain(DrainedSlot::Estate, log, snapshot(gate));
            assert!(estate.covers_requested_window, "{presented}: {estate:?}");
        }
        // Past a source's share the fact carries the slot beside the
        // source, not in a key.
        let mut log = facts(gate);
        log.push(event(
            gate,
            Fact::ReaderPresented,
            60,
            json!({"service":"policy","instance":"old","mode":"report","recording_since":at(0),
                "scope":"source","source":"192.0.2.15","presented":"reader.previous"}),
        ));
        assert!(!drain(DrainedSlot::Reader, log, snapshot(gate)).covers_requested_window);
        // And an estate `previous_presented` is not the reader's.
        let mut log = facts(gate);
        log.push(event(
            gate,
            Fact::PreviousPresented,
            60,
            json!({"service":"policy","instance":"old","mode":"report","recording_since":at(0),
                "key":{"presented":"previous"}}),
        ));
        assert!(drain(DrainedSlot::Reader, log.clone(), snapshot(gate)).covers_requested_window);
        assert!(!drain(DrainedSlot::Estate, log, snapshot(gate)).covers_requested_window);
    }

    /// A first sighting says when a caller was first seen, never when it
    /// stopped: a `reader.previous` sighted before the window, by a
    /// process that lived into it, has an unknown final use — the rule
    /// `a_retired_previous_sighting_has_no_proven_final_usage_time` holds
    /// the estate to, held for the reader.
    #[test]
    fn a_retired_reader_previous_sighting_has_no_proven_final_usage_time() {
        let gate = Gate::MachineGate;
        let mut log = facts(gate);
        log.push(sighting(json!("reader.previous"), 1, "old", 0));
        let reader = drain(DrainedSlot::Reader, log.clone(), snapshot(gate));
        assert!(!reader.covers_requested_window, "{reader:?}");
        assert!(
            reader
                .input_errors
                .iter()
                .any(|reason| reason.contains("final usage")),
            "{:?}",
            reader.input_errors
        );
        assert!(drain(DrainedSlot::Estate, log, snapshot(gate)).covers_requested_window);
    }

    /// `join_previous_window` IS the estate's drain: one function reached
    /// by two names answers one thing, on a clean window and on each way
    /// the estate's drain is held.
    #[test]
    fn the_estate_drain_is_the_previous_window_it_always_was() {
        let gate = Gate::MachineGate;
        let mut held = snapshot(gate);
        held["rows"] = json!([row("previous", 75)]);
        let mut logged = facts(gate);
        logged.push(event(
            gate,
            Fact::PreviousPresented,
            70,
            json!({"service":"policy"}),
        ));
        for (log, body) in [
            (facts(gate), snapshot(gate)),
            (facts(gate), held),
            (logged, snapshot(gate)),
        ] {
            let mut misses: Misses = serde_json::from_value(body).unwrap();
            misses.judge();
            let reads = vec![read(serde_json::to_value(misses).unwrap())];
            let was = join_previous_window(
                &["policy".into()],
                at(51),
                at(75),
                Ok(log.clone()),
                reads.clone(),
            );
            let is = join_drain_window(
                DrainedSlot::Estate,
                &["policy".into()],
                at(51),
                at(75),
                Ok(log),
                reads,
            );
            assert_eq!(
                serde_json::to_value(&was).unwrap(),
                serde_json::to_value(&is).unwrap()
            );
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
        let mut log = starts(Some(&generation(TWO)), Some(&generation(TWO)));
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
            json!({"service":"assets","mode":"report","since":at(1),"instance":"retired",
                "roster":{"generation":generation(TWO),"commit":"fixture"}}),
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
            generation: None,
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

    // ── The roster's generation (design 3cc6152a; backlog 14fe115c) ───
    // The launch record excuses services, and until this existed nothing
    // on the log said WHICH selection a stretch of hours was watched
    // under: a service excused by a new roster kept the clean hours the
    // old one had earned while it still ran, unless it happened to leave
    // a fact inside the window.
    fn generation(record: &str) -> String {
        roster_generation(record).unwrap()
    }
    /// `facts(MachineGate)` — a process from hour 0 that ends cleanly at
    /// hour 74 and its successor — with each start stating a generation,
    /// or none.
    fn starts(old: Option<&str>, current: Option<&str>) -> Vec<Event> {
        let mut log = facts(Gate::MachineGate);
        for (start, stated) in [(0, old), (2, current)] {
            if let Some(stated) = stated {
                log[start].payload[ROSTER_FIELD] =
                    json!({"generation": stated, "commit": "fixture"});
            }
        }
        log
    }
    /// The enforce window over `log`, hours 3 to 75, read by a pod whose
    /// own launch record is `TWO`: policy started, assets skipped and
    /// silent.
    fn under_two(log: Vec<Event>) -> JoinedWindow {
        under_two_from(3, log)
    }
    fn under_two_from(from: i64, log: Vec<Event>) -> JoinedWindow {
        let gate = Gate::MachineGate;
        let roster = launch_roster(&gated()[..2], Ok(TWO));
        join_launched_window(
            gate,
            &roster,
            &probe(Ok(false)),
            at(from),
            at(75),
            Ok(log),
            vec![
                read(snapshot(gate)),
                LiveRead {
                    service: "assets".into(),
                    answer: Err("connection refused".into()),
                },
            ],
        )
    }

    #[test]
    fn a_generation_is_the_selection_and_nothing_else() {
        use sha2::Digest;
        // Exactly what is hashed: a header, then one line per decision,
        // verb and binary, sorted by binary.
        let canonical = "boss-launch-roster v1\n\
            skip boss-assets-api\n\
            start boss-policy-api\n\
            skip boss-sim-control\n";
        assert_eq!(
            generation(RECORD),
            format!(
                "sha256:{}",
                hex::encode(sha2::Sha256::digest(canonical.as_bytes()))
            )
        );
        // A reworded reason and a reordered launcher array are the same
        // selection: the same generation, so neither restarts a window.
        let reworded = "boss-launch-record v1\n\
            skip boss-sim-control parked\n\
            skip boss-assets-api the module is off\n\
            start boss-policy-api\n\
            end 3\n";
        assert_eq!(generation(reworded), generation(RECORD));
        // Any change of what is started is another generation.
        let skip = "skip boss-assets-api module equipment is not on in the tenant manifest";
        for (name, other) in [
            (
                "a skipped binary is started",
                RECORD.replace(skip, "start boss-assets-api"),
            ),
            (
                "a started binary is skipped",
                RECORD.replace("start boss-policy-api", "skip boss-policy-api off"),
            ),
            (
                "a binary is added",
                RECORD.replace("end 3", "start boss-new-api\nend 4"),
            ),
            ("a binary is removed", TWO.to_string()),
        ] {
            assert_ne!(generation(&other), generation(RECORD), "{name}");
        }
        // A record that cannot be read whole has no generation, and the
        // roster read from the same text carries the same answer.
        assert!(roster_generation("boss-launch-record v1\nstart boss-policy-api\n").is_err());
        assert_eq!(
            launch_roster(&gated(), Ok(RECORD)).generation,
            Some(generation(RECORD))
        );
        assert_eq!(launch_roster(&gated(), Ok("")).generation, None);
    }

    #[test]
    fn a_process_states_its_generation_or_why_it_has_none() {
        assert_eq!(
            roster_stamp(Ok(TWO), Some("abc123")),
            json!({"generation": generation(TWO), "commit": "abc123"})
        );
        // No record is not a refusal to start: the process states why,
        // and the window that reads it cannot be clean.
        let unset = read_launch_record(None).unwrap_err();
        assert!(unset.contains("BOSS_LAUNCH_RECORD is unset"), "{unset}");
        assert!(read_launch_record(Some("  ".into())).is_err());
        let absent = read_launch_record(Some("/nonexistent/boss-launch-record".into()));
        assert!(absent.unwrap_err().contains("is unreadable"));
        assert_eq!(
            roster_stamp(Err(unset.clone()), None),
            json!({"error": unset, "commit": null})
        );
        let cut = roster_stamp(Ok("boss-launch-record v1\nstart boss-policy-api\n"), None);
        assert!(cut["error"].as_str().unwrap().contains("truncated"));
        assert!(cut.get("generation").is_none(), "{cut}");
    }

    #[test]
    fn a_window_inside_one_generation_is_clean_and_names_it() {
        let two = generation(TWO);
        let log = starts(Some(&two), Some(&two));
        let first = log[0].id;
        let v = under_two(log);
        assert!(v.covers_requested_window, "{:?}", v.not_clean);
        // A relaunch of the same selection (hour 74) is the same
        // generation: one stretch, from its first start.
        assert_eq!(
            v.roster_generations,
            [RosterGeneration {
                generation: Some(two),
                since: at(0),
                stated_by: first,
                service: "policy".into(),
                unstated: None,
            }]
        );
        // A window that consults no launch record has no excuse to
        // inherit and no generation rule.
        let plain = judge(
            Gate::MachineGate,
            facts(Gate::MachineGate),
            snapshot(Gate::MachineGate),
        );
        assert!(plain.covers_requested_window, "{:?}", plain.not_clean);
        assert!(plain.roster_generations.is_empty());
    }

    #[test]
    fn a_window_that_spans_a_change_of_selection_is_not_clean_and_says_when() {
        let (two, three) = (generation(TWO), generation(RECORD));
        // The pod ran another selection until hour 74, inside the window.
        let v = under_two(starts(Some(&three), Some(&two)));
        refuses(&v);
        // This rule alone, in its two halves: when the selection
        // changed, and whose running process was under the other one.
        assert_eq!(v.not_clean.len(), 2, "this rule alone: {:?}", v.not_clean);
        assert!(
            v.not_clean[1].starts_with("launch roster: policy: 1 of the 2 process start(s)")
                && v.not_clean[1].contains(&at(0).to_rfc3339())
                && v.not_clean[1].contains(&three),
            "{:?}",
            v.not_clean
        );
        let why = &v.not_clean[0];
        assert!(
            why.contains("spans a change of launch roster")
                && why.contains(&three)
                && why.contains(&two)
                && why.contains(&at(74).to_rfc3339()),
            "{why}"
        );
        assert_eq!(
            v.roster_generations
                .iter()
                .map(|g| (g.generation.clone(), g.since))
                .collect::<Vec<_>>(),
            [(Some(three.clone()), at(0)), (Some(two.clone()), at(74))]
        );
        // The change is not an input error: the inputs are whole.
        assert!(v.input_errors.is_empty(), "{:?}", v.input_errors);
        // The other direction: the log moved on and this reader did not.
        let v = under_two(starts(Some(&two), Some(&three)));
        refuses(&v);
        assert!(
            v.not_clean
                .iter()
                .any(|why| why.contains("this reader's launch record is generation")),
            "{:?}",
            v.not_clean
        );
    }

    #[test]
    fn a_window_entirely_after_the_change_is_judged_under_the_new_one_alone() {
        let (two, three) = (generation(TWO), generation(RECORD));
        // The same change, read over a window that opens at it.
        let v = under_two_from(74, starts(Some(&three), Some(&two)));
        assert!(v.covers_requested_window, "{:?}", v.not_clean);
        assert_eq!(v.roster_generations.len(), 1);
        assert_eq!(v.roster_generations[0].generation, Some(two.clone()));
        assert_eq!(v.roster_generations[0].since, at(74));
        // A service the old selection ran and the new one excuses keeps
        // its last start on the log forever. It predates the selection
        // every later start states, so it is the past.
        let mut log = starts(Some(&two), Some(&two));
        let mut retired = event(
            Gate::MachineGate,
            Fact::RecordingBegan,
            -30,
            json!({"service":"assets","mode":"report","since":at(-30),"instance":"retired",
                "roster":{"generation":three,"commit":"fixture"}}),
        );
        retired.source = "assets".into();
        log.push(retired);
        let v = under_two(log);
        assert!(v.covers_requested_window, "{:?}", v.not_clean);
        assert_eq!(v.roster_generations.len(), 1);
        assert_eq!(v.roster_generations[0].generation, Some(two));
    }

    #[test]
    fn the_hours_before_the_first_stated_generation_are_not_backdated() {
        let two = generation(TWO);
        // The shape of this mechanism's own landing: every start before
        // it stated nothing, and the first boot on it is at hour 74.
        let v = under_two(starts(None, Some(&two)));
        refuses(&v);
        assert_eq!(v.not_clean.len(), 2, "{:?}", v.not_clean);
        assert!(
            v.not_clean[1].starts_with("launch roster: policy: 1 of the 2 process start(s)")
                && v.not_clean[1].contains("carries no roster stamp"),
            "{:?}",
            v.not_clean
        );
        assert!(
            v.not_clean[0].contains("no stated generation")
                && v.not_clean[0].contains(&at(74).to_rfc3339()),
            "{:?}",
            v.not_clean
        );
        assert_eq!(v.roster_generations[0].generation, None);
        // From that boot on, the window reads clean again.
        let v = under_two_from(74, starts(None, Some(&two)));
        assert!(v.covers_requested_window, "{:?}", v.not_clean);
    }

    #[test]
    fn a_start_that_states_no_generation_is_never_read_clean() {
        let two = generation(TWO);
        let unreadable = "the launch record /etc/boss-launch-record is unreadable: denied";
        let mut said_why = starts(Some(&two), None);
        said_why[2].payload[ROSTER_FIELD] = json!({"error": unreadable, "commit": null});
        for (name, log, names) in [
            (
                "the newest start states nothing",
                starts(Some(&two), None),
                "no roster stamp",
            ),
            (
                "the newest start says why it has none",
                said_why,
                unreadable,
            ),
            (
                "no start states anything",
                starts(None, None),
                "no roster stamp",
            ),
        ] {
            let v = under_two(log);
            refuses(&v);
            assert!(
                v.not_clean.iter().any(|why| why.contains(names)),
                "{name}: {:?}",
                v.not_clean
            );
        }
        // A log with no process start at all states no selection either.
        let (stretches, why) = judged_generations(Gate::MachineGate, &[], &required(), at(3), &two);
        assert!(stretches.is_empty());
        assert!(why[0].contains("no process start on the log"), "{why:?}");
    }

    #[test]
    fn a_start_under_another_roster_at_the_windows_opening_reaches_into_it() {
        let (two, three) = (generation(TWO), generation(RECORD));
        // The hole this rule closes. A service ran under the OLD
        // selection when the window opened (its start is hour 1, before
        // it), was killed with the pod and stated no end, and the new
        // selection excuses it: no fact of its own is inside the window,
        // its port is closed now, and its unstated misses would have
        // been forgiven.
        let mut log = starts(Some(&three), Some(&two));
        let mut ran = event(
            Gate::MachineGate,
            Fact::RecordingBegan,
            1,
            json!({"service":"assets","mode":"report","since":at(1),"instance":"killed",
                "roster":{"generation":three,"commit":"fixture"}}),
        );
        ran.source = "assets".into();
        log.push(ran);
        let v = under_two(log);
        refuses(&v);
        assert!(v.not_launched[0].not_clean.is_empty(), "{v:?}");
        assert!(
            v.not_clean[0].contains("spans a change of launch roster"),
            "{:?}",
            v.not_clean
        );
        // A start that states another selection BETWEEN two relaunches of
        // this one is not the past either: nothing relaunched after it
        // before the window opened.
        let mut log = starts(Some(&two), Some(&two));
        let mut stray = event(
            Gate::MachineGate,
            Fact::RecordingBegan,
            1,
            json!({"service":"assets","mode":"report","since":at(1),"instance":"stray",
                "roster":{"generation":three,"commit":"fixture"}}),
        );
        stray.source = "assets".into();
        log.push(stray);
        refuses(&under_two(log));
    }

    #[test]
    fn the_reader_and_the_log_must_name_the_same_generation() {
        let three = generation(RECORD);
        let v = under_two(starts(Some(&three), Some(&three)));
        refuses(&v);
        assert_eq!(v.not_clean.len(), 2, "{:?}", v.not_clean);
        assert!(
            v.not_clean[1].starts_with("launch roster: policy: 2 of the 2 process start(s)"),
            "{:?}",
            v.not_clean
        );
        assert!(
            v.not_clean[0].contains("this reader's launch record is generation")
                && v.not_clean[0].contains(&generation(TWO))
                && v.not_clean[0].contains(&three),
            "{:?}",
            v.not_clean
        );
    }

    // ── The per-process half (review 6858ef1d, B1) ────────────────────
    // A process does not end when another service starts. The first six
    // tests and the tie test are the reviewer's, brought in as written
    // (four of them red on head 623d59cc) and then moved to the rule's
    // new signature.
    fn start_at(source: &str, when: DateTime<Utc>, instance: &str, roster: Option<Value>) -> Event {
        let mut payload =
            json!({"service":source,"mode":"report","since":when,"instance":instance});
        if let Some(roster) = roster {
            payload[ROSTER_FIELD] = roster;
        }
        let mut e = event(Gate::MachineGate, Fact::RecordingBegan, 0, payload);
        e.timestamp = when;
        e.source = source.into();
        e
    }
    fn rv_start(source: &str, hour: i64, instance: &str, roster: Option<Value>) -> Event {
        start_at(source, at(hour), instance, roster)
    }
    fn rv_good(generation: &str) -> Option<Value> {
        Some(json!({"generation": generation, "commit": "fixture"}))
    }
    fn rv_error() -> Option<Value> {
        Some(
            json!({"error":"the launch record /etc/boss-launch-record is unreadable: EIO","commit":null}),
        )
    }
    /// The enforce window, hours 3 to 75, over `log` as given: policy is
    /// the one required service and its LIVE process began at hour 0.
    fn rv_window(log: Vec<Event>) -> JoinedWindow {
        let gate = Gate::MachineGate;
        let roster = launch_roster(&gated()[..2], Ok(TWO));
        let mut body = snapshot(gate);
        body["recording_since"] = json!(at(0));
        join_launched_window(
            gate,
            &roster,
            &probe(Ok(false)),
            at(3),
            at(75),
            Ok(log),
            vec![
                read(body),
                LiveRead {
                    service: "assets".into(),
                    answer: Err("connection refused".into()),
                },
            ],
        )
    }
    /// policy is the one required service; its LIVE process began at hour
    /// 0 (before the window opens at hour 3) and is still the one that
    /// answers at hour 75. `stamp` is what that start stated.
    fn rv_live_policy(stamp: Option<Value>, others: Vec<Event>) -> JoinedWindow {
        let mut log = vec![rv_start("policy", 0, "current", stamp)];
        log.extend(others);
        log.sort_by_key(|e| (e.timestamp, e.id));
        rv_window(log)
    }
    fn required() -> Vec<String> {
        vec!["policy".to_string()]
    }

    #[test]
    fn rv_control_the_live_fixture_is_clean_when_its_start_is_good() {
        let two = generation(TWO);
        let v = rv_live_policy(rv_good(&two), vec![]);
        assert!(v.covers_requested_window, "{:?}", v.not_clean);
    }

    #[test]
    fn rv_control_an_error_stamp_alone_is_not_clean() {
        let v = rv_live_policy(Some(json!({"error":"unreadable","commit":null})), vec![]);
        refuses(&v);
    }

    /// The live process of the REQUIRED service stated an error at hour 0
    /// and has watched every hour of the window since. Another service's
    /// good start at hour 1 (same boot, before the window opens) must not
    /// make that readable as clean.
    #[test]
    fn rv_a_later_good_start_of_another_service_does_not_mask_an_error_stamp() {
        let two = generation(TWO);
        let v = rv_live_policy(rv_error(), vec![rv_start("jobs", 1, "j", rv_good(&two))]);
        assert!(
            !v.covers_requested_window,
            "READS CLEAN: generations {:?}",
            v.roster_generations
        );
        // The line names the service, the start instant and what it said.
        let why: Vec<&String> = v
            .not_clean
            .iter()
            .filter(|why| why.starts_with("launch roster: policy:"))
            .collect();
        assert_eq!(why.len(), 1, "{:?}", v.not_clean);
        assert!(
            why[0].contains(&at(0).to_rfc3339())
                && why[0].contains("is unreadable: EIO")
                && why[0].contains(&two),
            "{why:?}"
        );
        // And the answer names every generation judged: the one the
        // other service stated and the none the required one did — so
        // the shell judge's "exactly one stated entry" refuses it too.
        assert_eq!(
            v.roster_generations
                .iter()
                .map(|g| (g.service.as_str(), g.generation.clone(), g.since))
                .collect::<Vec<_>>(),
            [("policy", None, at(0)), ("jobs", Some(two), at(1))]
        );
    }

    #[test]
    fn rv_a_later_good_start_of_another_service_does_not_mask_an_unstamped_start() {
        let two = generation(TWO);
        let v = rv_live_policy(None, vec![rv_start("jobs", 1, "j", rv_good(&two))]);
        assert!(
            !v.covers_requested_window,
            "READS CLEAN: generations {:?}",
            v.roster_generations
        );
        assert!(
            v.not_clean
                .iter()
                .any(|why| why.starts_with("launch roster: policy:")
                    && why.contains("carries no roster stamp")),
            "{:?}",
            v.not_clean
        );
        assert_eq!(v.roster_generations.len(), 2, "{:?}", v.roster_generations);
    }

    #[test]
    fn rv_a_later_good_start_of_another_service_does_not_mask_another_generation() {
        let (two, three) = (generation(TWO), generation(RECORD));
        let v = rv_live_policy(
            rv_good(&three),
            vec![rv_start("jobs", 1, "j", rv_good(&two))],
        );
        assert!(
            !v.covers_requested_window,
            "READS CLEAN: generations {:?}",
            v.roster_generations
        );
        assert!(
            v.not_clean
                .iter()
                .any(|why| why.starts_with("launch roster: policy:") && why.contains(&three)),
            "{:?}",
            v.not_clean
        );
        assert_eq!(
            v.roster_generations
                .iter()
                .map(|g| g.generation.clone())
                .collect::<Vec<_>>(),
            [Some(three), Some(two)]
        );
    }

    /// The same boot, the error-stating process the LAST to state its
    /// start instead of the first: what the verdict is then.
    #[test]
    fn rv_the_same_error_stated_last_in_the_boot_is_not_clean() {
        let two = generation(TWO);
        let v = rv_window(vec![
            rv_start("jobs", -1, "j", rv_good(&two)),
            rv_start(
                "policy",
                0,
                "current",
                Some(json!({"error":"unreadable","commit":null})),
            ),
        ]);
        refuses(&v);
    }

    /// One boot, two starts at the SAME instant, one an error: the
    /// verdict is a function of the facts, not of which uuid sorts first
    /// — for a required service (the per-process half) and for services
    /// this reader does not require (the stretches).
    #[test]
    fn rv_a_tie_does_not_decide_the_verdict() {
        let two = generation(TWO);
        let g = Gate::MachineGate;
        for required in [required(), Vec::new()] {
            let mut good = rv_start("jobs", 40, "a", rv_good(&two));
            let mut bad = rv_start("policy", 40, "b", Some(json!({"error":"x","commit":null})));
            good.id = Uuid::from_u128(1);
            bad.id = Uuid::from_u128(2);
            let first =
                judged_generations(g, &[good.clone(), bad.clone()], &required, at(49), &two);
            good.id = Uuid::from_u128(2);
            bad.id = Uuid::from_u128(1);
            // The order the facts are handed in does not matter either.
            let second = judged_generations(g, &[bad, good], &required, at(49), &two);
            assert!(
                !first.1.is_empty() && !second.1.is_empty(),
                "required {required:?}: the same two facts read clean or not by event id alone: \
                 {:?} vs {:?}",
                first.1,
                second.1
            );
            assert_eq!(first.0.len(), second.0.len(), "required {required:?}");
        }
        // Through the whole join, with the live process the one in the tie.
        for (low, high) in [(1, 2), (2, 1)] {
            let mut good = rv_start("jobs", 0, "j", rv_good(&two));
            let mut bad = rv_start("policy", 0, "current", rv_error());
            good.id = Uuid::from_u128(low);
            bad.id = Uuid::from_u128(high);
            refuses(&rv_window(vec![good, bad]));
        }
    }

    /// One boot of four services, the required one stating an error: in
    /// every position of the boot's order — first, between, last, and
    /// all four at one instant under either id order — the window is not
    /// clean; and the same boot with a good stamp is clean in every one.
    #[test]
    fn the_order_of_starts_within_a_boot_does_not_decide_the_verdict() {
        let two = generation(TWO);
        let others = ["jobs", "people", "ledger"];
        let minute = |m: i64| at(0) + Duration::minutes(m);
        for (name, stamp, clean) in [
            ("an error stamp", rv_error(), false),
            ("no stamp", None, false),
            ("another generation", rv_good(&generation(RECORD)), false),
            ("this generation", rv_good(&two), true),
        ] {
            let mut verdicts = Vec::new();
            // policy stays at hour 0 (its live process); `before` of the
            // others start ahead of it and the rest after.
            for before in 0..=others.len() {
                let mut log = vec![rv_start("policy", 0, "current", stamp.clone())];
                for (i, other) in others.iter().enumerate() {
                    let offset = if i < before {
                        i as i64 - before as i64
                    } else {
                        (i - before) as i64 + 1
                    };
                    log.push(start_at(other, minute(offset), other, rv_good(&two)));
                }
                // The facts in the order given, and reversed.
                for reversed in [false, true] {
                    let mut log = log.clone();
                    if reversed {
                        log.reverse();
                    }
                    verdicts.push((
                        format!("{before} before, reversed {reversed}"),
                        rv_window(log),
                    ));
                }
            }
            for (low, high) in [(1u128, 100u128), (100, 1)] {
                let mut log = vec![rv_start("policy", 0, "current", stamp.clone())];
                log[0].id = Uuid::from_u128(low);
                for (i, other) in others.iter().enumerate() {
                    let mut e = start_at(other, at(0), other, rv_good(&two));
                    e.id = Uuid::from_u128(high + i as u128);
                    log.push(e);
                }
                verdicts.push((format!("one instant, policy id {low}"), rv_window(log)));
            }
            for (order, v) in verdicts {
                assert_eq!(
                    v.covers_requested_window, clean,
                    "{name}, {order}: {:?}",
                    v.not_clean
                );
                // What the shell judge reads: exactly one stated entry.
                let one_stated = matches!(
                    v.roster_generations.as_slice(),
                    [only] if only.generation.as_deref() == Some(two.as_str())
                );
                assert_eq!(
                    one_stated, clean,
                    "{name}, {order}: {:?}",
                    v.roster_generations
                );
            }
        }
    }

    /// Which starts of a required service are judged: the newest at or
    /// before the opening and each one inside the window — not an older
    /// one the same service has since replaced, and not another
    /// service's.
    #[test]
    fn a_required_service_is_judged_by_its_own_running_process_and_each_start_inside() {
        let (two, three) = (generation(TWO), generation(RECORD));
        let g = Gate::MachineGate;
        let judged = |log: &[Event]| judged_generations(g, log, &required(), at(49), &two);
        // An older start of the SAME service under an error, replaced by
        // a good one before the window opened: that process is gone.
        let (stretches, why) = judged(&[
            rv_start("policy", 10, "a", rv_error()),
            rv_start("policy", 48, "b", rv_good(&two)),
        ]);
        assert!(why.is_empty(), "{why:?}");
        assert_eq!(stretches.len(), 1, "{stretches:?}");
        // A start AT the opening instant is the process the window opens
        // under; the one before it is not judged.
        let (_, why) = judged(&[
            rv_start("policy", 10, "a", rv_error()),
            rv_start("policy", 49, "b", rv_good(&two)),
        ]);
        assert!(why.is_empty(), "{why:?}");
        // The newest before the opening is judged however old it is, and
        // whatever any other service stated after it.
        let (stretches, why) = judged(&[
            rv_start("policy", -500, "a", rv_good(&three)),
            rv_start("jobs", -499, "j", rv_good(&two)),
            rv_start("people", 48, "p", rv_good(&two)),
        ]);
        assert_eq!(why.len(), 1, "{why:?}");
        assert!(
            why[0].starts_with("policy: 1 of the 1 process start(s)")
                && why[0].contains(&at(-500).to_rfc3339())
                && why[0].contains(&three),
            "{why:?}"
        );
        assert_eq!(stretches.len(), 2, "{stretches:?}");
        // Each start INSIDE the window is judged: a good one after an
        // error does not take the error's hours back, and the line counts
        // them and names the first.
        let (_, why) = judged(&[
            rv_start("policy", 48, "a", rv_good(&two)),
            rv_start("policy", 60, "b", rv_error()),
            rv_start("policy", 61, "c", None),
            rv_start("policy", 70, "d", rv_good(&two)),
        ]);
        let mine: Vec<&String> = why.iter().filter(|w| w.starts_with("policy:")).collect();
        assert_eq!(mine.len(), 1, "{why:?}");
        assert!(
            mine[0].starts_with("policy: 2 of the 4 process start(s)")
                && mine[0].contains(&at(60).to_rfc3339()),
            "{mine:?}"
        );
        // A start is the service's by its source or by the service it
        // names (the log port groups by the name).
        let mut named = rv_start("policy", 48, "a", rv_error());
        named.source = "elsewhere".into();
        let (_, why) = judged(&[named]);
        assert!(why.iter().any(|w| w.starts_with("policy:")), "{why:?}");
        // A service this reader does not require is not judged here, and
        // a required one with no start at all is the coverage half's to
        // refuse, not this one's.
        let (_, why) = judged(&[
            rv_start("jobs", 10, "j", rv_good(&two)),
            rv_start("people", 48, "p", rv_good(&two)),
        ]);
        assert!(why.is_empty(), "{why:?}");
    }

    /// Review 6858ef1d, section A, after the repair. now = hour 75, a
    /// 26-hour window opens at hour 49. Each case as a pod states it:
    /// every boot restarts the required service.
    #[test]
    fn rv_two_selections_by_when_the_newer_began() {
        let (two, three) = (generation(TWO), generation(RECORD));
        let g = Gate::MachineGate;
        let judged = |log: &[Event]| judged_generations(g, log, &required(), at(49), &two);
        // old at T-30h, new at T-2h: the window spans the change. The
        // stretches say when; the per-process half says whose process.
        let (stretches, why) = judged(&[
            rv_start("assets", 45, "o", rv_good(&three)),
            rv_start("policy", 45, "o", rv_good(&three)),
            rv_start("policy", 73, "n", rv_good(&two)),
        ]);
        assert_eq!(stretches.len(), 2, "{stretches:?}");
        assert_eq!(why.len(), 2, "{why:?}");
        assert!(
            why[0].contains("spans a change of launch roster"),
            "{why:?}"
        );
        assert!(why[1].starts_with("policy: 1 of the 2"), "{why:?}");
        // old at T-30h, new at T-27h: judged under the new one alone. The
        // retired service's last start (assets, never relaunched) is the
        // past, and so is the required service's old process.
        let (stretches, why) = judged(&[
            rv_start("assets", 45, "o", rv_good(&three)),
            rv_start("policy", 45, "o", rv_good(&three)),
            rv_start("policy", 48, "n", rv_good(&two)),
        ]);
        assert_eq!(stretches.len(), 1, "{stretches:?}");
        assert!(why.is_empty(), "{why:?}");
        // The stretches alone (no required service named), as the review
        // ran them: old/new by when, good-error-good, old-new-old.
        let alone = |log: &[Event]| judged_generations(g, log, &[], at(49), &two);
        let (stretches, why) = alone(&[
            rv_start("assets", 45, "o", rv_good(&three)),
            rv_start("policy", 73, "n", rv_good(&two)),
        ]);
        assert_eq!((stretches.len(), why.len()), (2, 1), "{why:?}");
        let (stretches, why) = alone(&[
            rv_start("assets", 45, "o", rv_good(&three)),
            rv_start("policy", 48, "n", rv_good(&two)),
        ]);
        assert_eq!((stretches.len(), why.len()), (1, 0), "{why:?}");
        // good, error, good, all inside the window: three stretches.
        let log = [
            rv_start("jobs", 40, "a", rv_good(&two)),
            rv_start("policy", 72, "b", Some(json!({"error":"x","commit":null}))),
            rv_start("jobs", 73, "c", rv_good(&two)),
        ];
        let (stretches, why) = alone(&log);
        assert_eq!((stretches.len(), why.len()), (3, 2), "{why:?}");
        let (stretches, why) = judged(&log);
        assert_eq!((stretches.len(), why.len()), (3, 3), "{why:?}");
        // old, new, old again: reverting does not rejoin the old hours.
        let log = [
            rv_start("policy", 40, "a", rv_good(&two)),
            rv_start("policy", 60, "b", rv_good(&three)),
            rv_start("policy", 70, "c", rv_good(&two)),
        ];
        let (stretches, why) = alone(&log);
        assert_eq!((stretches.len(), why.len()), (3, 2), "{why:?}");
        let (stretches, why) = judged(&log);
        assert_eq!((stretches.len(), why.len()), (3, 3), "{why:?}");
        // a stamp that is not an object, and an empty generation: unstated.
        for odd in [
            json!("sha256:x"),
            json!({"generation": ""}),
            json!({"generation": 7}),
            json!(null),
        ] {
            let log = [rv_start("policy", 40, "a", Some(odd.clone()))];
            let (stretches, why) = alone(&log);
            assert_eq!(stretches[0].generation, None, "{odd}");
            assert_eq!(why.len(), 1, "{odd}: {why:?}");
            let (stretches, why) = judged(&log);
            assert_eq!(stretches.len(), 1, "{odd}: {stretches:?}");
            assert_eq!(why.len(), 2, "{odd}: {why:?}");
        }
    }

    /// Review 6858ef1d, N3: the record is read at every gated boot, so
    /// the read is bounded and never opens what could block. Each
    /// failure is a reason — an error stamp — and never a refusal.
    #[test]
    fn the_launch_record_read_is_bounded_and_never_opens_what_is_not_a_file() {
        let dir = std::env::temp_dir().join(format!("boss-core-launch-record-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = |name: &str| dir.join(name).to_string_lossy().to_string();
        std::fs::write(dir.join("record"), TWO).unwrap();
        assert_eq!(read_launch_record(Some(path("record"))).unwrap(), TWO);
        // A directory, and a FIFO — which `open` would block on forever.
        let why = read_launch_record(Some(path(""))).unwrap_err();
        assert!(
            why.contains("is unreadable: it is not a regular file"),
            "{why}"
        );
        let fifo = std::process::Command::new("mkfifo")
            .arg(dir.join("fifo"))
            .status()
            .unwrap();
        assert!(fifo.success(), "mkfifo");
        let (done, waited) = std::sync::mpsc::channel();
        let at_fifo = path("fifo");
        std::thread::spawn(move || done.send(read_launch_record(Some(at_fifo))));
        let why = waited
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the read of a FIFO returns instead of waiting for a writer")
            .unwrap_err();
        assert!(
            why.contains("is unreadable: it is not a regular file"),
            "{why}"
        );
        // One byte past the bound is refused; the bound itself is read.
        let most = MAX_LAUNCH_RECORD_BYTES as usize;
        std::fs::write(dir.join("most"), "x".repeat(most)).unwrap();
        assert_eq!(read_launch_record(Some(path("most"))).unwrap().len(), most);
        std::fs::write(dir.join("over"), "x".repeat(most + 1)).unwrap();
        let why = read_launch_record(Some(path("over"))).unwrap_err();
        assert!(why.contains("is unreadable: it is larger than"), "{why}");
        std::fs::write(dir.join("binary"), [0xff, 0xfe]).unwrap();
        let why = read_launch_record(Some(path("binary"))).unwrap_err();
        assert!(why.contains("is unreadable: it is not UTF-8"), "{why}");
        // Whatever the reason, what the process states is an error stamp.
        let stamp = roster_stamp(
            read_launch_record(Some(path("over")))
                .as_deref()
                .map_err(Clone::clone),
            None,
        );
        assert!(stamp.get("generation").is_none(), "{stamp}");
        assert!(stamp["error"].as_str().unwrap().contains("larger than"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
