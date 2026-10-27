//! Packet membership is a separate flow basis from distinct step
//! obligations. Inputs are original bounded histories and producer
//! groups; neither current packet metadata nor today's registry is
//! consulted while replaying a prior window.
use crate::{
    move_diagnosis::HistoricalEvent,
    port::JobScope,
    registry::{WorkflowSpec, WorkflowStatus},
    stations::StationSpec,
};
use boss_clock_client::mapping_history::ClockTimeline;
use boss_core::job::{Job, JobStatus, Step, StepStatus};
use boss_events::original_group::OriginalGroup;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};
use std::time::Instant;

pub mod projection;
pub mod pruned;
#[cfg(test)]
mod pruned_tests;

/// One explicitly owned replay's cooperative resource bound. A checkpoint
/// cannot preempt a single clone, sort or predicate evaluation. Callers must
/// retain ownership/resource permits until this synchronous work returns.
/// Fuel is deterministic; deadline/cancellation refuse a reading rather than
/// changing any successful replay fact. Both windows spend the same budget.
pub struct ReplayBudget {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    remaining: AtomicUsize,
    failure: AtomicU8,
    // Unit fixtures share their owning trace's clock across real blocking
    // work; scheduling delay must not manufacture the expiry under test.
    #[cfg(test)]
    test_now: Option<Arc<std::sync::Mutex<Option<Instant>>>>,
}

impl ReplayBudget {
    /// The immutable monotonic cutoff shared with native source adapters.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn new(deadline: Instant, cancelled: Arc<AtomicBool>, fuel: usize) -> Self {
        Self {
            deadline,
            cancelled,
            remaining: AtomicUsize::new(fuel),
            failure: AtomicU8::new(0),
            #[cfg(test)]
            test_now: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_clock(mut self, now: Arc<std::sync::Mutex<Option<Instant>>>) -> Self {
        self.test_now = Some(now);
        self
    }

    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(clock) = &self.test_now
            && let Some(now) = *clock.lock().unwrap()
        {
            return now;
        }
        Instant::now()
    }

    /// Remaining time in this same owned flight, never a new adapter deadline.
    pub fn remaining(&self) -> Result<std::time::Duration, String> {
        self.check()?;
        Ok(self.deadline.saturating_duration_since(self.now()))
    }

    /// Check this same flight's cancellation/deadline/fuel before another
    /// adapter phase. Refusal is sticky across snapshot fallback and replay.
    pub fn check(&self) -> Result<(), String> {
        let previous = self.failure.load(Ordering::Acquire);
        let failure = if previous != 0 {
            previous
        } else if self.cancelled.load(Ordering::Acquire) {
            1
        } else if self.now() >= self.deadline {
            2
        } else if self
            .remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |fuel| {
                fuel.checked_sub(1)
            })
            .is_err()
        {
            3
        } else {
            return Ok(());
        };
        let _ = self
            .failure
            .compare_exchange(0, failure, Ordering::AcqRel, Ordering::Acquire);
        Err(match self.failure.load(Ordering::Acquire) {
            1 => "native membership replay cancelled",
            2 => "native membership replay deadline exhausted",
            _ => "native membership replay work budget exhausted",
        }
        .into())
    }
}

fn checkpoint(budget: Option<&ReplayBudget>) -> Result<(), String> {
    match budget {
        Some(budget) => budget.check(),
        None => Ok(()),
    }
}

#[cfg(test)]
mod unknown_span_scaling_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn indexed_unknown_spans_keep_exact_first_occurrence_evidence() {
        let mut flow = MembershipFlow::new("fixture".into(), &JobScope::All);
        let start = Utc.timestamp_opt(1_760_000_000, 0).unwrap();
        let end = start + chrono::Duration::hours(24);
        let mut expected = Vec::new();
        for (from, until, reason) in [
            (start, end, "missing workflow"),
            (start, end, "missing workflow"),
            (
                start + chrono::Duration::seconds(1),
                end,
                "missing workflow",
            ),
            (start, end, "missing clock"),
            (end, start, "empty span"),
            (start, end, "missing workflow"),
        ] {
            let entry = Unavailable {
                since: from,
                until,
                reason: reason.into(),
            };
            if from < until && !expected.contains(&entry) {
                expected.push(entry);
            }
            flow.unknown_span(from, until, reason);
        }
        assert_eq!(flow.unknown, expected);
        let serialized = serde_json::to_value(&flow).unwrap();
        assert!(serialized.get("unknown_seen").is_none());
        let restored: MembershipFlow = serde_json::from_value(serialized.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), serialized);
    }

    #[test]
    fn a_completed_reading_releases_its_temporary_deduplication_index() {
        let start = Utc.timestamp_opt(1_760_000_000, 0).unwrap();
        let until = start + chrono::Duration::hours(24);
        let population: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/station-predicate-population-20261016.json"
        ))
        .unwrap();
        let input = ReplayInput {
            groups: vec![ReplayGroup {
                original: None,
                rows: vec![HistoricalEvent {
                    seq: 1,
                    created_at: Some(start - chrono::Duration::seconds(1)),
                    event: boss_core::event::Event::new(
                        "jobs",
                        crate::events::STATION_PUBLISHED,
                        population["data"][0].clone(),
                        start - chrono::Duration::seconds(1),
                    ),
                }],
            }],
            since: start,
            until,
            audit_cutoff: 1,
            actor: Some("alice".into()),
            scope: JobScope::All,
            currently_authorized: None,
            clock: ClockTimeline::build(&[], None, start, until),
            registry_prefix_complete: true,
            packet_prefix_complete: true,
            source_unavailable: vec![Unavailable {
                since: start,
                until,
                reason: "original source gap".into(),
            }],
            initial: InitialState::default(),
        };
        let reading = replay(&input);
        assert_eq!(reading.stations.len(), 1);
        assert!(!reading.stations[0].unknown.is_empty());
        assert!(
            reading.stations[0].unknown_seen.is_empty(),
            "the cached reading retains a second copy of its historical evidence"
        );
        assert_eq!(reading.stations[0].unknown_seen.capacity(), 0);
    }

    #[test]
    #[ignore = "explicit scalability regression; run on an optimized builder worker"]
    fn production_volume_unknown_spans_do_not_rescan_the_accumulated_history() {
        let mut flow = MembershipFlow::new("fixture".into(), &JobScope::All);
        let start = Utc.timestamp_opt(1_760_000_000, 0).unwrap();
        let until = start + chrono::Duration::days(3);
        let started = std::time::Instant::now();
        for n in 0..200_000 {
            flow.unknown_span(
                start + chrono::Duration::microseconds(n),
                until,
                "original applicable workflow publication missing",
            );
        }
        let elapsed = started.elapsed();
        eprintln!(
            "unknown-span insertion count={} elapsed_ms={}",
            flow.unknown.len(),
            elapsed.as_millis()
        );
        assert_eq!(flow.unknown.len(), 200_000);
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "deduplicating distinct spans rescanned their complete prefix: {elapsed:?}"
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayGroup {
    pub original: Option<OriginalGroup>,
    pub rows: Vec<HistoricalEvent>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayInput {
    pub groups: Vec<ReplayGroup>,
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub audit_cutoff: i64,
    pub actor: Option<String>,
    pub scope: JobScope,
    /// Current authorized identities observed in the same native snapshot.
    /// Some also restricts global scope for targeted packet reconstruction;
    /// None is valid only for an unrestricted global read.
    pub currently_authorized: Option<BTreeSet<String>>,
    pub clock: ClockTimeline,
    pub registry_prefix_complete: bool,
    /// A native identity/Step roster may disprove a complete original
    /// prefix. It never supplies missing historical state.
    #[serde(default)]
    pub packet_prefix_complete: bool,
    #[serde(default)]
    pub source_unavailable: Vec<Unavailable>,
    /// Where the fold starts. Empty is genesis: every group is replayed
    /// from the first row of the log, which is how this basis read until
    /// design c195e83e (2026-10-17) measured it refusing every station.
    #[serde(default)]
    pub initial: InitialState,
}
/// The starting state of the fold, witnessed at the window cut (design
/// c195e83e, Q1-Q3 approved 2026-10-17). The row and byte bounds stay;
/// what changes is that the fold no longer starts at genesis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InitialState {
    /// The highest audit sequence folded into this state. Every row
    /// above it, and every whole original group holding a row above it
    /// (Q2), is replayed in `ReplayInput::groups` instead.
    pub cut: i64,
    /// Each carried packet's latest original snapshot at the cut: its
    /// Job row, each Step's latest row (update, first record or claim
    /// receipt), and a manifest claim's chunks. Q3: this snapshot is the
    /// packet's creation witness; no creation row since genesis is read.
    pub rows: Vec<HistoricalEvent>,
    /// Packets the current census names that the originals cannot place
    /// at the cut, each with its specific reason. UNKNOWN, never absent.
    pub unknown: BTreeMap<String, String>,
    /// Terminal packets left out under Q1: no instant of the window's
    /// known clock can count them on any registered station.
    /// The count alone cannot rule membership out in an unknown clock
    /// span; an optional conserved original census is verified separately.
    pub pruned_terminal: u64,
    /// Original per-packet census, absent on legacy inputs. A scalar alone
    /// never supplies a station-specific outcome certificate.
    #[serde(default)]
    pub pruned_census: Option<pruned::PrunedCensus>,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Unavailable {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionBasis {
    pub version: i32,
    pub source: String,
    pub publication_sequences: Vec<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembershipFlow {
    pub station: String,
    pub basis: String,
    pub arrived: Option<u64>,
    pub departed: Option<u64>,
    pub served: Option<u64>,
    pub known_arrived: u64,
    pub known_departed: u64,
    pub known_served: u64,
    pub departure_causes: BTreeMap<String, u64>,
    pub unknown: Vec<Unavailable>,
    /// Replay-local membership index; the ordered original spans remain
    /// the serialized evidence. A linear scan here made distinct refusals
    /// quadratic in the number of original groups (c7870939).
    #[serde(skip)]
    unknown_seen: HashSet<Unavailable>,
    pub covered: Vec<Coverage>,
    pub versions: Vec<VersionBasis>,
    pub caller_binding: Option<String>,
    pub scope: JobScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pruned_exclusions: Vec<pruned::Exclusion>,
}
impl MembershipFlow {
    /// Why this flow has no count: every unknown span's reason, in order.
    /// `None` when nothing is unknown. One spelling for every surface that
    /// shows it — `GET /api/stations/flow` and the IT map (ab09f1af).
    pub fn unavailable_reason(&self) -> Option<String> {
        (!self.unknown.is_empty()).then(|| {
            self.unknown
                .iter()
                .map(|u| u.reason.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
    fn new(name: String, scope: &JobScope) -> Self {
        Self {
            station: name,
            basis: "packet-membership-v1".into(),
            arrived: Some(0),
            departed: Some(0),
            served: Some(0),
            known_arrived: 0,
            known_departed: 0,
            known_served: 0,
            departure_causes: BTreeMap::new(),
            unknown: vec![],
            unknown_seen: HashSet::new(),
            covered: vec![],
            versions: vec![],
            caller_binding: None,
            scope: scope.clone(),
            pruned_exclusions: vec![],
        }
    }
    fn unknown(&mut self, input: &ReplayInput, reason: impl Into<String>) {
        let entry = Unavailable {
            since: input.since,
            until: input.until,
            reason: reason.into(),
        };
        if self.unknown_seen.insert(entry.clone()) {
            self.unknown.push(entry);
        }
    }
    fn unknown_span(
        &mut self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        reason: impl Into<String>,
    ) {
        if since < until {
            let entry = Unavailable {
                since,
                until,
                reason: reason.into(),
            };
            if self.unknown_seen.insert(entry.clone()) {
                self.unknown.push(entry);
            }
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembershipReading {
    pub stations: Vec<MembershipFlow>,
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub audit_cutoff: i64,
    pub denied: bool,
}
#[derive(Default, Clone, Serialize, Deserialize)]
struct Packet {
    job: Option<Job>,
    steps: BTreeMap<String, Step>,
    complete: bool,
    reason: Option<String>,
}
#[derive(Default)]
struct State<'a> {
    budget: Option<&'a ReplayBudget>,
    packets: BTreeMap<String, Packet>,
    authored: BTreeMap<String, (StationSpec, i64)>,
    workflows: BTreeMap<String, (WorkflowSpec, i64)>,
    registry_at: Option<DateTime<Utc>>,
    station_cache: BTreeMap<String, (StationSpec, VersionBasis)>,
}
impl State<'_> {
    fn packets_for(&self, only: Option<&BTreeSet<String>>) -> Vec<(&String, &Packet)> {
        match only {
            Some(ids) => ids
                .iter()
                .take_while(|_| checkpoint(self.budget).is_ok())
                .filter_map(|id| self.packets.get_key_value(id))
                .collect(),
            None => self
                .packets
                .iter()
                .take_while(|_| checkpoint(self.budget).is_ok())
                .collect(),
        }
    }
    fn refresh_stations(&mut self) {
        if checkpoint(self.budget).is_err() {
            return;
        }
        let mut out: BTreeMap<_, _> = self
            .authored
            .iter()
            .take_while(|_| checkpoint(self.budget).is_ok())
            .map(|(name, (spec, seq))| {
                (
                    name.clone(),
                    (
                        spec.clone(),
                        VersionBasis {
                            version: spec.version,
                            source: "authored-publication".into(),
                            publication_sequences: vec![*seq],
                        },
                    ),
                )
            })
            .collect();
        if let Some(at) = self.registry_at {
            let workflows: Vec<_> = self.workflows.values().map(|(w, _)| w.clone()).collect();
            let names = self.authored.keys().cloned().collect::<Vec<_>>();
            let sequences = self
                .workflows
                .values()
                .map(|(_, seq)| *seq)
                .collect::<Vec<_>>();
            for spec in crate::station_projection::derived_stations(&workflows, &names, at) {
                if checkpoint(self.budget).is_err() {
                    return;
                }
                out.insert(
                    spec.name.clone(),
                    (
                        spec.clone(),
                        VersionBasis {
                            version: spec.version,
                            source: "original-workflow-publications".into(),
                            publication_sequences: sequences.clone(),
                        },
                    ),
                );
            }
        }
        self.station_cache = out;
    }
    fn stations(&self) -> BTreeMap<String, (StationSpec, VersionBasis)> {
        self.station_cache
            .iter()
            .take_while(|_| checkpoint(self.budget).is_ok())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
    fn members_for(
        &self,
        spec: Option<&StationSpec>,
        at: DateTime<Utc>,
        input: &ReplayInput,
        only: Option<&BTreeSet<String>>,
    ) -> Result<BTreeMap<String, bool>, String> {
        checkpoint(self.budget)?;
        let Some(spec) = spec else {
            return Ok(BTreeMap::new());
        };
        let bound = spec
            .bind_self(input.actor.as_deref())
            .ok_or("actual caller binding unavailable")?;
        let mut out = BTreeMap::new();
        let candidates = self.packets_for(only);
        checkpoint(self.budget)?;
        for (id, packet) in candidates {
            checkpoint(self.budget)?;
            if !authorized_packet(input, id) {
                continue;
            }
            let Some(job) = &packet.job else {
                // A census packet with no witness at the cut carries its
                // own reason (Q3); otherwise the first snapshot is missing.
                return Err(packet
                    .reason
                    .clone()
                    .unwrap_or("original first Job snapshot missing".into()));
            };
            if !input.scope.admits(job) {
                continue;
            }
            if bound
                .predicate
                .kind
                .as_ref()
                .is_some_and(|kind| kind != &job.kind)
            {
                continue;
            }
            if !packet.complete {
                return Err(packet
                    .reason
                    .clone()
                    .unwrap_or("original packet prefix is incomplete".into()));
            }
            let steps = packet
                .steps
                .values()
                .take_while(|_| checkpoint(self.budget).is_ok())
                .cloned()
                .collect::<Vec<_>>();
            checkpoint(self.budget)?;
            if bound.predicate.needs_steps() && !self.workflows.contains_key(&job.kind) {
                return Err("original applicable workflow publication missing".into());
            }
            let steps = crate::agent_spec::resolved_steps(
                &steps,
                self.workflows.get(&job.kind).map(|(w, _)| w),
            );
            checkpoint(self.budget)?;
            if !bound.predicate.matches(job, &steps) {
                out.insert(id.clone(), false);
                continue;
            }
            let in_window = if matches!(job.status, JobStatus::Closed | JobStatus::Cancelled)
                && bound.terminal_window_days.is_some()
            {
                match input.clock.date_at(at) {
                    Ok(today) => {
                        crate::station_queue::in_window(job, today, bound.terminal_window_days)
                    }
                    Err(reason) => at
                        .checked_add_signed(chrono::Duration::microseconds(1))
                        .and_then(|until| {
                            invariant_terminal_window(
                                job,
                                bound.terminal_window_days,
                                at,
                                until,
                                input,
                            )
                        })
                        .ok_or(reason)?,
                }
            } else {
                crate::station_queue::in_window(job, job.opened_on, bound.terminal_window_days)
            }; // in_window does not inspect this date for live/no-retention packets.
            out.insert(
                id.clone(),
                in_window && bound.predicate.matches(job, &steps),
            );
        }
        Ok(out)
    }
    /// Fold one starting-state snapshot row (c195e83e). Unlike `apply`,
    /// a snapshot is not a transition: an update without its creation
    /// is the witness Q3 accepts, so no creation order is checked. A
    /// packet first seen here is complete once its Job snapshot is held,
    /// unless the census already named it UNKNOWN; one a replayed group
    /// already placed keeps the completeness that group's evidence gave.
    fn seed(&mut self, row: &HistoricalEvent) -> Result<(), String> {
        checkpoint(self.budget)?;
        match packet_fact(row)? {
            Some(PacketFact::Job(job)) => {
                let packet = self.packets.entry(job.id.to_string()).or_default();
                if packet.job.is_none() {
                    packet.complete = packet.reason.is_none();
                }
                packet.job = Some(job);
            }
            Some(PacketFact::Step(step)) => {
                let packet = self.packets.entry(step.job_id.to_string()).or_default();
                packet.steps.insert(step.id.to_string(), step);
            }
            None => {}
        }
        Ok(())
    }
    fn apply(&mut self, row: &HistoricalEvent, complete: bool) -> Result<(), String> {
        checkpoint(self.budget)?;
        let e = &row.event;
        match e.kind.as_str() {
            crate::events::JOB_CREATED | crate::events::JOB_UPDATED => {
                let Some(PacketFact::Job(job)) = packet_fact(row)? else {
                    return Err("original Job snapshot is not a Job".into());
                };
                let packet = self.packets.entry(job.id.to_string()).or_default();
                if e.kind == crate::events::JOB_CREATED {
                    if packet.job.is_some() {
                        return Err("duplicate original packet admission".into());
                    }
                    packet.complete = complete;
                } else if packet.job.is_none() {
                    packet.reason = Some("original first Job snapshot missing".into());
                }
                packet.complete &= complete;
                packet.job = Some(job);
            }
            crate::events::STEP_CREATED
            | crate::events::STEP_UPDATED
            | crate::events::STEP_FIRST_RECORDED
            | crate::events::STATION_CLAIMED => {
                let Some(PacketFact::Step(step)) = packet_fact(row)? else {
                    return Err("original Step snapshot is not a Step".into());
                };
                let packet = self.packets.entry(step.job_id.to_string()).or_default();
                let exists = packet.steps.contains_key(&step.id.to_string());
                let broken = match e.kind.as_str() {
                    crate::events::STEP_CREATED if exists => {
                        Some("duplicate or missing original Step creation")
                    }
                    crate::events::STEP_UPDATED if !exists => {
                        Some("duplicate or missing original Step creation")
                    }
                    crate::events::STEP_FIRST_RECORDED if !exists => {
                        Some("original Step creation missing before first record")
                    }
                    crate::events::STATION_CLAIMED if !exists => {
                        Some("original Step creation missing before station claim")
                    }
                    _ => None,
                };
                if let Some(reason) = broken {
                    packet.complete = false;
                    packet.reason = Some(reason.into());
                }
                packet.complete &= complete;
                packet.steps.insert(step.id.to_string(), step);
            }
            crate::events::WORKFLOW_PUBLISHED | crate::events::WORKFLOW_RETIRED => {
                let spec: WorkflowSpec =
                    serde_json::from_value(e.payload.clone()).map_err(|e| e.to_string())?;
                if e.kind == crate::events::WORKFLOW_PUBLISHED
                    && spec.status == WorkflowStatus::Active
                {
                    self.workflows.insert(spec.kind.clone(), (spec, row.seq));
                } else if e.kind == crate::events::WORKFLOW_RETIRED
                    && spec.status == WorkflowStatus::Retired
                {
                    if self
                        .workflows
                        .get(&spec.kind)
                        .is_some_and(|(active, _)| active.version == spec.version)
                    {
                        self.workflows.remove(&spec.kind);
                    }
                } else {
                    return Err("original workflow publication kind/status mismatch".into());
                }
                self.registry_at = row.created_at;
            }
            crate::events::STATION_PUBLISHED | crate::events::STATION_RETIRED => {
                let spec: StationSpec =
                    serde_json::from_value(e.payload.clone()).map_err(|e| e.to_string())?;
                if e.kind == crate::events::STATION_PUBLISHED
                    && spec.status == WorkflowStatus::Active
                {
                    self.authored.insert(spec.name.clone(), (spec, row.seq));
                } else if e.kind == crate::events::STATION_RETIRED
                    && spec.status == WorkflowStatus::Retired
                {
                    if self
                        .authored
                        .get(&spec.name)
                        .is_some_and(|(active, _)| active.version == spec.version)
                    {
                        self.authored.remove(&spec.name);
                    }
                } else {
                    return Err("original station publication kind/status mismatch".into());
                }
                self.registry_at = row.created_at;
            }
            _ => {}
        }
        Ok(())
    }
}

/// The packet state one original row carries, read once for both the
/// starting state and the replay so the two cannot parse a row apart.
enum PacketFact {
    Job(Job),
    Step(Step),
}

fn packet_fact(row: &HistoricalEvent) -> Result<Option<PacketFact>, String> {
    let e = &row.event;
    let receipt_event = || boss_events::replay::ReplayEvent {
        audit_id: row.seq,
        event_id: e.id,
        kind: e.kind.clone(),
        ts: e.timestamp,
        payload: e.payload.clone(),
    };
    Ok(match e.kind.as_str() {
        crate::events::JOB_CREATED | crate::events::JOB_UPDATED => Some(PacketFact::Job(
            serde_json::from_value(e.payload.clone()).map_err(|e| e.to_string())?,
        )),
        crate::events::STEP_CREATED | crate::events::STEP_UPDATED => Some(PacketFact::Step(
            serde_json::from_value(e.payload.clone()).map_err(|e| e.to_string())?,
        )),
        crate::events::STEP_FIRST_RECORDED => Some(PacketFact::Step(
            crate::state_receipt::first_record(&receipt_event())?.0,
        )),
        crate::events::STATION_CLAIMED => {
            let text = e
                .payload
                .get("receipt_json")
                .and_then(serde_json::Value::as_str)
                .ok_or("original exact claim JSON missing")?;
            Some(PacketFact::Step(
                crate::state_receipt::station_claim(&receipt_event(), text)?.0,
            ))
        }
        _ => None,
    })
}

/// Prepare the starting state (c195e83e): place the census verdicts,
/// and restore each claim receipt kept as chunks from its own chunks,
/// exactly as a replayed group restores it. Answers the rows to seed in
/// audit order. A row that cannot be read makes its own packet UNKNOWN
/// when its packet is known, and the whole read UNKNOWN when not.
fn prepare_seed(state: &mut State, input: &ReplayInput) -> Result<Vec<HistoricalEvent>, String> {
    checkpoint(state.budget)?;
    let initial = &input.initial;
    let mut rows: Vec<&HistoricalEvent> = initial.rows.iter().collect();
    rows.sort_by_key(|r| r.seq);
    let mut prepared = Vec::with_capacity(rows.len());
    let mut chunks = BTreeMap::<String, Vec<boss_events::replay::ReplayEvent>>::new();
    let as_replay = |r: &HistoricalEvent| boss_events::replay::ReplayEvent {
        audit_id: r.seq,
        event_id: r.event.id,
        kind: r.event.kind.clone(),
        ts: r.event.timestamp,
        payload: r.event.payload.clone(),
    };
    // A census verdict is placed first: a later snapshot cannot make a
    // packet the census already found unplaceable look complete.
    for (id, reason) in &initial.unknown {
        checkpoint(state.budget)?;
        if !authorized_packet(input, id) {
            continue;
        }
        let packet = state.packets.entry(id.clone()).or_default();
        packet.complete = false;
        packet.reason = Some(reason.clone());
    }
    for row in rows {
        checkpoint(state.budget)?;
        if row.event.kind == crate::events::STATION_SELECTION_EVIDENCE {
            let claim = row
                .event
                .payload
                .get("claim_event_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            chunks.entry(claim).or_default().push(as_replay(row));
            continue;
        }
        let mut row = row.clone();
        if row.event.kind == crate::events::STATION_CLAIMED {
            let mut events = chunks.remove(&row.event.id.to_string()).unwrap_or_default();
            events.push(as_replay(&row));
            match crate::selection_evidence::reconstruct(&events) {
                Ok(restored) => {
                    if let Some(restored) = restored.iter().find(|r| r.event_id == row.event.id) {
                        row.event.payload = restored.payload.clone();
                    }
                }
                Err(error) => {
                    unreadable(state, input, &row, error.to_string())?;
                    continue;
                }
            }
        }
        if visible_row(&row, input) {
            prepared.push(row);
        }
    }
    Ok(prepared)
}

/// Seed the prepared rows below `bound`. The replay calls this before
/// each group, with that group's first audit sequence while the group
/// lies wholly at or below the cut, so a registry or clock group whose
/// transaction also wrote packet rows lands in audit order around the
/// snapshots; and with no bound before the first group reaching past
/// the cut, so the whole starting state precedes the window.
fn seed_through(
    state: &mut State,
    input: &ReplayInput,
    seeds: &mut std::iter::Peekable<std::vec::IntoIter<HistoricalEvent>>,
    bound: i64,
) -> Result<(), String> {
    while let Some(row) = seeds.next_if(|row| row.seq < bound) {
        checkpoint(state.budget)?;
        if let Err(error) = state.seed(&row) {
            unreadable(state, input, &row, error)?;
        }
    }
    Ok(())
}

fn unreadable(
    state: &mut State,
    input: &ReplayInput,
    row: &HistoricalEvent,
    reason: String,
) -> Result<(), String> {
    // As in a replayed group: a narrow read cannot be refused by a row it
    // could not see, or the refusal itself would disclose the row.
    if !visible_row(row, input) {
        return Ok(());
    }
    let Some(id) = packet_identity(row) else {
        return Err(reason);
    };
    let packet = state.packets.entry(id).or_default();
    packet.complete = false;
    packet.reason = Some(reason);
    Ok(())
}

pub(crate) fn packet_identity(row: &HistoricalEvent) -> Option<String> {
    if row.event.kind == crate::events::STATION_CLAIMED
        && let Some(value) = row
            .event
            .payload
            .get("receipt_json")
            .and_then(serde_json::Value::as_str)
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        && let Some(id) = value
            .pointer("/selected/job_id")
            .and_then(serde_json::Value::as_str)
    {
        return Some(id.to_owned());
    }
    row.event
        .payload
        .get("job_id")
        .or_else(|| row.event.payload.pointer("/step/job_id"))
        .or_else(|| row.event.payload.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// What one original group writes of packet state, per packet: each
/// entity it writes (`job`, or `step:<id>`) and the group's first audit
/// sequence for that entity. An entity the row does not let us name is
/// `None`, and it is never proven disjoint from anything. Marker rows
/// (`step.assigned.*`, `jobs.step.completed`, …) name a packet but carry
/// no state the replay folds, so they write nothing here.
type Writes = BTreeMap<String, BTreeMap<Option<String>, i64>>;

fn state_writes(
    rows: &[HistoricalEvent],
    input: &ReplayInput,
    budget: Option<&ReplayBudget>,
) -> Writes {
    let mut out = Writes::new();
    for row in rows.iter().filter(|row| visible_row(row, input)) {
        if checkpoint(budget).is_err() {
            break;
        }
        let payload = &row.event.payload;
        let entity = match row.event.kind.as_str() {
            crate::events::JOB_CREATED | crate::events::JOB_UPDATED => Some("job".to_owned()),
            crate::events::STEP_CREATED | crate::events::STEP_UPDATED => payload
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(|id| format!("step:{id}")),
            crate::events::STEP_FIRST_RECORDED | crate::events::STATION_CLAIMED => {
                match packet_fact(row) {
                    Ok(Some(PacketFact::Step(step))) => Some(format!("step:{}", step.id)),
                    _ => None,
                }
            }
            _ => continue,
        };
        // A state row naming no packet fails to fold at all (`apply`),
        // which refuses the whole read; it needs no order here.
        let Some(packet) = packet_identity(row) else {
            continue;
        };
        let first = out
            .entry(packet)
            .or_default()
            .entry(entity)
            .or_insert(row.seq);
        *first = (*first).min(row.seq);
    }
    out
}

/// An earlier original group whose audit span a later one may overlap.
struct Prior {
    low: i64,
    high: i64,
    registry: bool,
    writes: Writes,
    at: DateTime<Utc>,
    /// Each packet this group wrote, as it stood just before the group
    /// was folded. Kept for the most recent `PRIOR_STATE_DEPTH` groups
    /// only; an older group's is dropped, and an overlap that would need
    /// it is refused rather than answered.
    before: Option<BTreeMap<String, Option<Packet>>>,
    /// Packets this group's position relative to an earlier group was
    /// left unestablished for, because no order could change a count.
    unordered: BTreeSet<String>,
}

/// How many recent groups keep the packet state from before them. The
/// interleavings measured on 2026-10-22 span milliseconds — adjacent
/// groups — so a few hundred is generous; the bound only keeps memory
/// flat over a window of tens of thousands of groups.
const PRIOR_STATE_DEPTH: usize = 256;

/// How a later group stands to an earlier group whose audit span it
/// overlaps (backlog da9faa93).
enum Relation {
    /// They write no packet in common, and neither is a registry group.
    Unrelated,
    /// They write a common packet, and some entity both write is first
    /// written by the later group only after the earlier group's last
    /// row. A Job or Step row is updated under its row lock, which is
    /// held to commit, and its outbox row is inserted after the update
    /// (the `OUTBOX (phase 2)` convention in `postgres.rs`). So the later
    /// write waited on the earlier commit, and the relay, which copies
    /// committed rows in outbox order, could only have placed it after
    /// the earlier group's rows: the earlier group committed first, which
    /// is the order the fold already applies.
    Ordered(Vec<String>),
    /// They write common packets, but only different entities of them:
    /// nothing in the log says which committed first.
    Unordered(Vec<String>),
    /// A registry group, an entity both write with no lock order, or an
    /// entity that cannot be named.
    Refused,
}

fn relation(
    prior: &Prior,
    registry: bool,
    writes: &Writes,
    budget: Option<&ReplayBudget>,
) -> Relation {
    if prior.registry || registry {
        return Relation::Refused;
    }
    let shared: Vec<String> = writes
        .keys()
        .take_while(|_| checkpoint(budget).is_ok())
        .filter(|id| prior.writes.contains_key(*id))
        .cloned()
        .collect();
    if shared.is_empty() {
        return Relation::Unrelated;
    }
    let mut witnessed = false;
    for id in &shared {
        if checkpoint(budget).is_err() {
            return Relation::Refused;
        }
        let (Some(earlier), Some(later)) = (prior.writes.get(id), writes.get(id)) else {
            continue;
        };
        if earlier.contains_key(&None) || later.contains_key(&None) {
            return Relation::Refused;
        }
        for (entity, first) in later {
            if checkpoint(budget).is_err() {
                return Relation::Refused;
            }
            if earlier.contains_key(entity) {
                if *first > prior.high {
                    witnessed = true;
                } else {
                    return Relation::Refused;
                }
            }
        }
    }
    if witnessed {
        Relation::Ordered(shared)
    } else {
        Relation::Unordered(shared)
    }
}

/// `packet` with `rows` folded onto it, or None when a row does not fold.
fn folded(
    state: &mut State,
    id: &str,
    packet: Option<Packet>,
    rows: &[&HistoricalEvent],
) -> Option<Packet> {
    if let Some(packet) = packet {
        state.packets.insert(id.to_owned(), packet);
    }
    let applied = rows.iter().all(|row| state.apply(row, true).is_ok());
    let out = state.packets.remove(id);
    applied.then_some(out).flatten()
}

/// Whether every station holds packet `id` alike in each state two
/// unordered transactions can leave it in, at both of their instants:
/// before both (`before`), after the earlier alone (the fold's current
/// state), after the later alone, and after both. Then no order of the
/// two commits makes the packet cross any station, so the count is the
/// same whichever committed first and needs no order (backlog da9faa93).
/// Anything that cannot be evaluated answers false.
fn order_cannot_matter(
    state: &mut State,
    input: &ReplayInput,
    id: &str,
    before: Option<Packet>,
    rows: &[&HistoricalEvent],
    instants: [DateTime<Utc>; 2],
) -> bool {
    let current = state.packets.remove(id);
    let alone = folded(state, id, before.clone(), rows);
    let both = folded(state, id, current.clone(), rows);
    let mut verdict = alone.is_some() && both.is_some();
    if verdict {
        let only: BTreeSet<String> = [id.to_owned()].into_iter().collect();
        let specs: Vec<StationSpec> = state
            .station_cache
            .values()
            .map(|(spec, _)| spec.clone())
            .collect();
        let mut seen: Option<Vec<bool>> = None;
        'states: for candidate in [before, current.clone(), alone, both] {
            if checkpoint(state.budget).is_err() {
                verdict = false;
                break;
            }
            match candidate {
                Some(packet) => state.packets.insert(id.to_owned(), packet),
                None => state.packets.remove(id),
            };
            for at in instants {
                if checkpoint(state.budget).is_err() {
                    verdict = false;
                    break 'states;
                }
                let mut reading = Vec::with_capacity(specs.len());
                for spec in &specs {
                    if checkpoint(state.budget).is_err() {
                        verdict = false;
                        break 'states;
                    }
                    match state.members_for(Some(spec), at, input, Some(&only)) {
                        Ok(members) => reading.push(members.get(id).copied().unwrap_or(false)),
                        Err(_) => {
                            verdict = false;
                            break 'states;
                        }
                    }
                }
                if *seen.get_or_insert_with(|| reading.clone()) != reading {
                    verdict = false;
                    break 'states;
                }
            }
        }
    }
    match current {
        Some(packet) => state.packets.insert(id.to_owned(), packet),
        None => state.packets.remove(id),
    };
    verdict
}

/// The group being folded: its audit span, insertion instant, and
/// whether it publishes or retires registry rows.
struct Span {
    first: i64,
    end: i64,
    at: DateTime<Utc>,
    registry: bool,
}

/// Whether this group's place among the earlier groups its audit span
/// overlaps is unestablished for packet state it writes, so the replay
/// cannot fold it without guessing (backlog da9faa93).
///
/// Measured 2026-10-22: every window of the IT map was dark because two
/// concurrent commits on one packet — the dispatcher auto-assigning an
/// agent run's first Step while `boss dispatch --started` claims its
/// Building Step, or a rule completing a Step the dispatcher had just
/// assigned — reach the log interleaved, and any overlap sharing a packet
/// identity was refused. Overlap alone says nothing about order. What
/// can: a row lock (`Relation::Ordered`), or the fact that no order of
/// the two could move the packet across any station
/// (`order_cannot_matter`). One unordered earlier writer per packet is
/// judged; two, or one whose own place was left open, are refused.
/// Packets judged unordered are added to `unordered`.
fn interleaves(
    state: &mut State,
    input: &ReplayInput,
    priors: &[Prior],
    span: Span,
    writes: &Writes,
    rows: &[&HistoricalEvent],
    unordered: &mut BTreeSet<String>,
) -> bool {
    let start = priors.partition_point(|prior| prior.high < span.first);
    let mut writers = BTreeMap::<String, usize>::new();
    let mut open = BTreeMap::<String, usize>::new();
    for (index, prior) in priors.iter().enumerate().skip(start) {
        if checkpoint(state.budget).is_err() {
            return true;
        }
        if span.first > prior.high || prior.low > span.end {
            continue;
        }
        match relation(prior, span.registry, writes, state.budget) {
            Relation::Unrelated => {}
            Relation::Refused => return true,
            Relation::Ordered(shared) => {
                for id in shared {
                    if checkpoint(state.budget).is_err() {
                        return true;
                    }
                    *writers.entry(id).or_default() += 1;
                }
            }
            // A starting-state snapshot may land between an earlier group
            // at or below the cut and this one, so the state the fold holds
            // is not that group's after-state: judge only window groups.
            Relation::Unordered(_) if prior.low <= input.initial.cut => return true,
            Relation::Unordered(shared) => {
                for id in shared {
                    if checkpoint(state.budget).is_err() {
                        return true;
                    }
                    *writers.entry(id.clone()).or_default() += 1;
                    open.insert(id, index);
                }
            }
        }
    }
    for (id, index) in open {
        if checkpoint(state.budget).is_err() {
            return true;
        }
        let prior = &priors[index];
        if writers.get(&id) != Some(&1) || prior.unordered.contains(&id) {
            return true;
        }
        let Some(before) = prior
            .before
            .as_ref()
            .and_then(|before| before.get(&id))
            .cloned()
        else {
            return true;
        };
        let own: Vec<&HistoricalEvent> = rows
            .iter()
            .copied()
            .filter(|row| {
                visible_row(row, input)
                    && matches!(
                        row.event.kind.as_str(),
                        crate::events::JOB_CREATED
                            | crate::events::JOB_UPDATED
                            | crate::events::STEP_CREATED
                            | crate::events::STEP_UPDATED
                            | crate::events::STEP_FIRST_RECORDED
                            | crate::events::STATION_CLAIMED
                    )
                    && packet_identity(row).as_deref() == Some(id.as_str())
            })
            .collect();
        if !order_cannot_matter(state, input, &id, before, &own, [prior.at, span.at]) {
            return true;
        }
        unordered.insert(id);
    }
    false
}

fn visible_row(row: &HistoricalEvent, input: &ReplayInput) -> bool {
    if matches!(
        row.event.kind.as_str(),
        crate::events::WORKFLOW_PUBLISHED
            | crate::events::WORKFLOW_RETIRED
            | crate::events::STATION_PUBLISHED
            | crate::events::STATION_RETIRED
    ) {
        return true;
    }
    if input.scope == JobScope::All && input.currently_authorized.is_none() {
        return true;
    }
    packet_identity(row).is_some_and(|id| authorized_packet(input, &id))
}

fn authorized_packet(input: &ReplayInput, id: &str) -> bool {
    input
        .currently_authorized
        .as_ref()
        .map_or(input.scope == JobScope::All, |ids| ids.contains(id))
}

/// Additive outcome proof only (design16fad). The original model roster is
/// retained in the same checkpoint/cut as the packet and registry facts.
/// A scalar pruned count cannot witness the packets it discarded.
fn invariant_terminal_window(
    job: &Job,
    days: Option<u32>,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    input: &ReplayInput,
) -> Option<bool> {
    job.closed_on?;
    let (first, last) = input
        .clock
        .outcome_date_bounds(since, until, input.audit_cutoff)
        .ok()?;
    let initial = crate::station_queue::in_window(job, first, days);
    let final_value = crate::station_queue::in_window(job, last, days);
    (initial == final_value).then_some(initial)
}

// Expiry counts transitions, unlike point membership. Its inclusive start
// therefore needs the preceding original microsecond too (bd16fc12).
fn invariant_terminal_crossings(
    job: &Job,
    days: Option<u32>,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    input: &ReplayInput,
) -> Option<bool> {
    let before = since.checked_sub_signed(chrono::Duration::microseconds(1))?;
    invariant_terminal_window(job, days, before, until, input)
}

fn accrue_expiry(
    state: &State,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    input: &ReplayInput,
    flows: &mut BTreeMap<String, MembershipFlow>,
    counted: &mut BTreeSet<(String, String, i64)>,
    mut facts: Option<&mut projection::FactRecorder>,
) {
    if checkpoint(state.budget).is_err() {
        return;
    }
    let from = from.max(input.since);
    let to = to.min(input.until);
    if from >= to {
        return;
    }
    // In one original monotone mapping, equal business dates on the
    // instant before the inclusive start and before the exclusive end
    // prove no retention boundary crossed. Most audit rows are minutes
    // apart; they must not each rescan every historic terminal packet.
    if let (Some(before), Some(last)) = (
        from.checked_sub_signed(chrono::Duration::microseconds(1)),
        to.checked_sub_signed(chrono::Duration::microseconds(1)),
    ) && input
        .clock
        .known
        .iter()
        .any(|span| span.since <= before && span.until >= to)
        && input
            .clock
            .date_at(before)
            .ok()
            .zip(input.clock.date_at(last).ok())
            .is_some_and(|(a, b)| a == b)
    {
        return;
    }
    for (name, (spec, basis)) in state.stations() {
        if checkpoint(state.budget).is_err() {
            return;
        }
        let Some(days) = spec.terminal_window_days else {
            continue;
        };
        let Some(bound) = spec.bind_self(input.actor.as_deref()) else {
            continue;
        };
        let flow = flows
            .entry(name.clone())
            .or_insert_with(|| MembershipFlow::new(name.clone(), &input.scope));
        // Q1 (c195e83e) pruned these because no KNOWN instant of the
        // window can count them. An unknown span cannot rule that out,
        // so it stays unknown here as a carried terminal packet keeps it.
        if input.initial.pruned_terminal > 0 {
            for gap in &input.clock.unknown {
                if checkpoint(state.budget).is_err() {
                    return;
                }
                let since = from.max(gap.since);
                let until = to.min(gap.until);
                if since >= until {
                    continue;
                }
                match pruned::exclusion(input, &spec, &basis, since, until, state.budget) {
                    Some(certificate) => {
                        if !flow.pruned_exclusions.contains(&certificate) {
                            flow.pruned_exclusions.push(certificate);
                        }
                    }
                    None => flow.unknown_span(since, until, gap.reason.clone()),
                }
            }
        }
        for (id, packet) in &state.packets {
            if checkpoint(state.budget).is_err() {
                return;
            }
            if !authorized_packet(input, id) {
                continue;
            }
            let Some(job) = &packet.job else {
                continue;
            };
            if !input.scope.admits(job)
                || !matches!(job.status, JobStatus::Closed | JobStatus::Cancelled)
            {
                continue;
            }
            let steps = crate::agent_spec::resolved_steps(
                &packet.steps.values().cloned().collect::<Vec<_>>(),
                state.workflows.get(&job.kind).map(|(w, _)| w),
            );
            if !bound.predicate.matches(job, &steps) {
                continue;
            }
            if !packet.complete {
                flow.unknown_span(from, to, "original packet prefix is incomplete");
                continue;
            }
            let Some(closed) = job.closed_on else {
                continue;
            };
            let Some(target) = closed.checked_add_days(chrono::Days::new(u64::from(days) + 1))
            else {
                flow.unknown_span(from, to, "terminal retention date exceeds supported range");
                continue;
            };
            for gap in &input.clock.unknown {
                if checkpoint(state.budget).is_err() {
                    return;
                }
                let since = from.max(gap.since);
                let until = to.min(gap.until);
                if invariant_terminal_crossings(job, Some(days), since, until, input).is_none() {
                    flow.unknown_span(since, until, gap.reason.clone());
                }
            }
            for span in &input.clock.known {
                if checkpoint(state.budget).is_err() {
                    return;
                }
                let low = from.max(span.since);
                let high = to.min(span.until).min(input.until) - chrono::Duration::microseconds(1);
                if low > high {
                    continue;
                }
                let (Ok(start), Ok(end)) = (input.clock.date_at(low), input.clock.date_at(high))
                else {
                    continue;
                };
                if end < target {
                    continue;
                }
                if start >= target {
                    // The interval is half-open. A timer exactly at its
                    // inclusive start belongs here, but a packet already
                    // expired beforehand does not create another departure.
                    let before = low.checked_sub_signed(chrono::Duration::microseconds(1));
                    let previous = before.and_then(|wall| input.clock.date_at(wall).ok());
                    if previous.is_none() {
                        // A gap ending at this known span's inclusive start
                        // cannot silently erase a possible expiry at the edge.
                        // Outcome constancy may rule it out, but never makes
                        // the missing clock instant known.
                        let Some(after_start) =
                            low.checked_add_signed(chrono::Duration::microseconds(1))
                        else {
                            flow.unknown_span(
                                low,
                                to,
                                "terminal retention boundary exceeds supported time",
                            );
                            continue;
                        };
                        if invariant_terminal_crossings(job, Some(days), low, after_start, input)
                            .is_none()
                        {
                            flow.unknown_span(
                                low,
                                after_start,
                                "terminal retention boundary has no known preceding clock instant",
                            );
                        }
                        continue;
                    }
                    if previous.is_some_and(|date| date >= target) {
                        continue;
                    }
                }
                let mut left = low.timestamp_micros();
                let mut right = high.timestamp_micros();
                while left < right {
                    if checkpoint(state.budget).is_err() {
                        return;
                    }
                    let middle = left + (right - left) / 2;
                    let Some(wall) = DateTime::<Utc>::from_timestamp_micros(middle) else {
                        break;
                    };
                    match input.clock.date_at(wall) {
                        Ok(date) if date >= target => right = middle,
                        Ok(_) => left = middle + 1,
                        Err(_) => break,
                    }
                }
                if counted.insert((name.clone(), id.clone(), left)) {
                    flow.known_departed += 1;
                    *flow
                        .departure_causes
                        .entry("terminal-expiry".into())
                        .or_default() += 1;
                    if let Some(facts) = facts.as_deref_mut() {
                        facts.expiry(&name, id, left, &span.revision, input.audit_cutoff);
                    }
                }
            }
        }
    }
}

fn check_initial_state(
    state: &State,
    input: &ReplayInput,
    flows: &mut BTreeMap<String, MembershipFlow>,
) {
    for (name, (spec, _)) in state.stations() {
        if checkpoint(state.budget).is_err() {
            return;
        }
        if let Err(reason) = state.members_for(Some(&spec), input.since, input, None) {
            flows
                .entry(name.clone())
                .or_insert_with(|| MembershipFlow::new(name, &input.scope))
                .unknown(input, reason);
        }
    }
}

/// THIS WINDOW AND THE WIDER ONE, FROM ONE SNAPSHOT (item ab09f1af,
/// car 2): `[from narrow, from input.since]`, both ending at the
/// snapshot's own cut. The IT map draws a station's served count in this
/// window and the previous one, the wider minus the narrower; two
/// snapshots would be two cuts, and a crossing between them would land
/// in the wider count alone and read as the PREVIOUS window's — the
/// reason the step-events cube once counted its pair in one statement
/// (backlog 29f7e897). The narrower replay folds the groups between the
/// two openings as history before its window, as any replay folds what
/// precedes its window. A narrower window that does not lie inside the
/// snapshot's is refused, never answered short.
/// Each station's packet-membership flow, by name, in this window and in
/// the wider window ending with it (this plus the previous) — the IT
/// map's flow reading (`http/regions.rs`, `membership_flows`).
#[derive(Debug, Clone)]
pub struct FlowWindows {
    pub this: BTreeMap<String, MembershipFlow>,
    /// The wider window, or why it could not be read — the previous
    /// window is then unread, while this one still counts.
    pub both: Result<BTreeMap<String, MembershipFlow>, String>,
}

/// The map's flow reading as it is shared between map reads (backlog
/// b852ec37, `crate::reading_cache`): the windows, or why they could not
/// be read, and the wall instant the windows END at — which a served
/// reading states, because it is not the serving read's own now.
#[derive(Debug, Clone)]
pub struct MapFlowReading {
    pub as_of: DateTime<Utc>,
    pub windows: Result<FlowWindows, String>,
}

pub fn replay_windows(
    mut input: ReplayInput,
    narrow: DateTime<Utc>,
) -> Result<[MembershipReading; 2], String> {
    if narrow < input.since || narrow >= input.until {
        return Err(format!(
            "window opening {narrow} lies outside the snapshot's [{}, {})",
            input.since, input.until
        ));
    }
    let wide = replay(&input);
    input.since = narrow;
    Ok([replay(&input), wide])
}

/// Replay with cooperative refusal and no partial result publication.
pub fn replay_checked(
    input: &ReplayInput,
    budget: &ReplayBudget,
) -> Result<MembershipReading, String> {
    budget.check()?;
    let reading = replay_with_budget(input, Some(budget));
    // Helpers may stop with a transient partial internal state. Never expose
    // that state as a reading: the first exhaustion remains sticky.
    budget.check()?;
    Ok(reading)
}

pub fn replay_windows_checked(
    mut input: ReplayInput,
    narrow: DateTime<Utc>,
    budget: &ReplayBudget,
) -> Result<[MembershipReading; 2], String> {
    if narrow < input.since || narrow >= input.until {
        return Err(format!(
            "window opening {narrow} lies outside the snapshot's [{}, {})",
            input.since, input.until
        ));
    }
    let wide = replay_checked(&input, budget)?;
    input.since = narrow;
    let this = replay_checked(&input, budget)?;
    Ok([this, wide])
}

pub fn replay(input: &ReplayInput) -> MembershipReading {
    replay_with_budget(input, None)
}

fn replay_with_budget(input: &ReplayInput, budget: Option<&ReplayBudget>) -> MembershipReading {
    replay_fold(input, budget, None, None).reading
}

struct ReplayFold<'a> {
    reading: MembershipReading,
    state: State<'a>,
}

// Both the oracle and the incremental projection use THIS fold. The
// optional recorder retains crossing facts, never historical payloads.
fn replay_fold<'a>(
    input: &ReplayInput,
    budget: Option<&'a ReplayBudget>,
    restored: Option<State<'a>>,
    mut facts: Option<&mut projection::FactRecorder>,
) -> ReplayFold<'a> {
    let resumed = restored.is_some();
    let mut state = restored.unwrap_or_default();
    state.budget = budget;
    let mut flows = BTreeMap::<String, MembershipFlow>::new();
    if resumed {
        for (name, (_, basis)) in state.stations() {
            let mut flow = MembershipFlow::new(name.clone(), &input.scope);
            flow.versions.push(basis);
            flows.insert(name, flow);
        }
    }
    let denied = input.scope == JobScope::None;
    if denied {
        return ReplayFold {
            reading: MembershipReading {
                stations: vec![],
                since: input.since,
                until: input.until,
                audit_cutoff: input.audit_cutoff,
                denied,
            },
            state,
        };
    }
    let mut global_reason = if !input.packet_prefix_complete {
        Some("current authorized packet/Step roster lacks original admission facts".to_owned())
    } else if !input.registry_prefix_complete {
        Some("complete original registry prefix unavailable".to_owned())
    } else if input.scope != JobScope::All && input.currently_authorized.is_none() {
        Some("safe current authorized packet census unavailable".into())
    } else {
        None
    };
    let mut seeds = match prepare_seed(&mut state, input) {
        Ok(rows) => rows,
        Err(reason) => {
            global_reason = global_reason.or(Some(reason));
            vec![]
        }
    }
    .into_iter()
    .peekable();
    let mut seen = BTreeSet::new();
    let mut global_since = if global_reason.is_some() {
        input.since
    } else {
        input.until
    };
    let mut last_end = 0;
    let mut last_at = input.since;
    let mut expiries = BTreeSet::new();
    let mut initial_checked = false;
    let mut prior_ranges = Vec::<Prior>::new();
    for original_group in &input.groups {
        if checkpoint(budget).is_err() {
            break;
        }
        let span = original_group.rows.iter().map(|r| r.seq);
        let bound = if span.clone().max().unwrap_or(i64::MAX) > input.initial.cut {
            i64::MAX
        } else {
            span.min().unwrap_or(i64::MAX)
        };
        if let Err(reason) = seed_through(&mut state, input, &mut seeds, bound) {
            global_reason = Some(reason);
            global_since = input.since;
        }
        let mut reconstructed = original_group.clone();
        let mut rows: Vec<_> = original_group
            .rows
            .iter()
            .map(|r| boss_events::replay::ReplayEvent {
                audit_id: r.seq,
                event_id: r.event.id,
                kind: r.event.kind.clone(),
                ts: r.event.timestamp,
                payload: r.event.payload.clone(),
            })
            .collect();
        if let Some(original) = &original_group.original
            && original.validate(input.audit_cutoff) == Ok(true)
        {
            rows.sort_by_key(|row| original.event_ids.iter().position(|id| id == &row.event_id));
        }
        let mut reconstruction_error = None;
        match crate::selection_evidence::reconstruct(&rows) {
            Ok(restored) => {
                for row in &mut reconstructed.rows {
                    if checkpoint(budget).is_err() {
                        break;
                    }
                    if let Some(restored) = restored.iter().find(|r| r.event_id == row.event.id) {
                        row.event.payload = restored.payload.clone();
                    }
                }
            }
            Err(error) => {
                reconstruction_error = Some(error.to_string());
            }
        }
        let group = &reconstructed;
        if input.scope != JobScope::All && !group.rows.iter().any(|row| visible_row(row, input)) {
            continue;
        }
        let reason_before = global_reason.clone();
        if let Some(error) = reconstruction_error {
            global_reason = Some(error);
        }
        let Some(end) = group.rows.iter().map(|r| r.seq).max() else {
            global_reason = Some("empty original group".into());
            global_since = input.since;
            continue;
        };
        let at = group.rows.iter().filter_map(|r| r.created_at).max();
        let start_at = group.rows.iter().filter_map(|r| r.created_at).min();
        let legacy_before = group.original.is_none() && at.is_some_and(|at| at < input.since);
        let evidence: Result<bool, String> =
            group.original.as_ref().map_or(Ok(legacy_before), |o| {
                o.validate(input.audit_cutoff)?;
                if group.rows.iter().any(|row| {
                    o.event_ids
                        .iter()
                        .position(|id| id == &row.event.id)
                        .is_none_or(|index| o.audit_sequences[index] != Some(row.seq))
                }) {
                    return Err("original event identity/audit sequence binding mismatch".into());
                }
                Ok(o.complete)
            });
        let complete = evidence == Ok(true);
        if end <= last_end
            || end > input.audit_cutoff
            || group.rows.iter().any(|r| !seen.insert(r.event.id))
        {
            global_reason =
                Some("original groups have duplicate identity, order or cutoff mismatch".into());
        }
        last_end = end;
        let Some(at) = at else {
            global_reason = Some("original audit insertion instant missing".into());
            global_since = input.since;
            continue;
        };
        if !initial_checked && at >= input.since {
            check_initial_state(&state, input, &mut flows);
            initial_checked = true;
        }
        accrue_expiry(
            &state,
            last_at,
            start_at.unwrap_or(at),
            input,
            &mut flows,
            &mut expiries,
            facts.as_deref_mut(),
        );
        last_at = at;
        if group.rows.iter().any(|r| r.created_at.is_none()) {
            global_reason = Some("original audit insertion instant missing".into());
        }
        let before = state.stations();
        let registry_group = group.rows.iter().any(|row| {
            matches!(
                row.event.kind.as_str(),
                crate::events::STATION_PUBLISHED
                    | crate::events::STATION_RETIRED
                    | crate::events::WORKFLOW_PUBLISHED
                    | crate::events::WORKFLOW_RETIRED
            )
        });
        let affected: BTreeSet<_> = group
            .rows
            .iter()
            .filter(|row| visible_row(row, input))
            .filter_map(packet_identity)
            .collect();
        let first = group.rows.iter().map(|r| r.seq).min().unwrap_or(end);
        let mut ordered = group.rows.iter().collect::<Vec<_>>();
        if let Some(original) = &group.original {
            ordered.sort_by_key(|row| original.event_ids.iter().position(|id| id == &row.event.id));
            if original.complete
                && (ordered.len() != original.event_ids.len()
                    || ordered
                        .iter()
                        .any(|r| !original.event_ids.contains(&r.event.id)))
            {
                global_reason = Some("whole original group events are incomplete".into());
            }
        }
        let writes = state_writes(&group.rows, input, budget);
        let mut unordered = BTreeSet::new();
        let interleaved = interleaves(
            &mut state,
            input,
            &prior_ranges,
            Span {
                first,
                end,
                at,
                registry: registry_group,
            },
            &writes,
            &ordered,
            &mut unordered,
        );
        let before_group = writes
            .keys()
            .map(|id| (id.clone(), state.packets.get(id).cloned()))
            .collect();
        prior_ranges.push(Prior {
            low: first,
            high: end,
            registry: registry_group,
            writes,
            at,
            before: Some(before_group),
            unordered,
        });
        if let Some(index) = prior_ranges.len().checked_sub(PRIOR_STATE_DEPTH + 1) {
            prior_ranges[index].before = None;
        }
        let only = if registry_group {
            None
        } else {
            Some(&affected)
        };
        let before_members: BTreeMap<_, _> = if at >= input.since && at < input.until {
            before
                .iter()
                .take_while(|_| checkpoint(budget).is_ok())
                .map(|(name, (spec, _))| {
                    (name.clone(), state.members_for(Some(spec), at, input, only))
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let previous_completed: BTreeSet<_> = state
            .packets_for(only)
            .into_iter()
            .flat_map(|(_, p)| p.steps.values())
            .take_while(|_| checkpoint(budget).is_ok())
            .filter(|s| s.status == StepStatus::Completed)
            .map(|s| s.id.to_string())
            .collect();
        let previous_assignees: BTreeMap<_, _> = state
            .packets_for(only)
            .into_iter()
            .flat_map(|(_, p)| p.steps.values())
            .take_while(|_| checkpoint(budget).is_ok())
            .map(|s| (s.id.to_string(), s.assignee_id.clone()))
            .collect();
        let eligible_before: BTreeMap<String, BTreeSet<String>> = before
            .iter()
            .take_while(|_| checkpoint(budget).is_ok())
            .map(|(name, (spec, _))| {
                let ids = spec
                    .bind_self(input.actor.as_deref())
                    .and_then(|bound| bound.predicate.step)
                    .map(|predicate| {
                        state
                            .packets_for(only)
                            .into_iter()
                            .flat_map(|(_, p)| {
                                crate::agent_spec::resolved_steps(
                                    &p.steps.values().cloned().collect::<Vec<_>>(),
                                    p.job
                                        .as_ref()
                                        .and_then(|j| state.workflows.get(&j.kind).map(|(w, _)| w)),
                                )
                            })
                            .take_while(|_| checkpoint(budget).is_ok())
                            .filter(|s| predicate.matches(s))
                            .map(|s| s.id.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                (name.clone(), ids)
            })
            .collect();
        let mut registry_changed = false;
        if interleaved {
            for id in &affected {
                if checkpoint(budget).is_err() {
                    break;
                }
                if let Some(packet) = state.packets.get_mut(id) {
                    packet.complete = false;
                    packet.reason=Some("interleaved original transaction groups do not establish packet state order".into());
                }
            }
        }
        for row in ordered {
            if checkpoint(budget).is_err() {
                break;
            }
            if !visible_row(row, input) {
                continue;
            }
            registry_changed |= matches!(
                row.event.kind.as_str(),
                crate::events::STATION_PUBLISHED
                    | crate::events::STATION_RETIRED
                    | crate::events::WORKFLOW_PUBLISHED
                    | crate::events::WORKFLOW_RETIRED
            );
            if let Err(reason) = state.apply(row, complete && !interleaved) {
                global_reason = Some(reason);
            }
        }
        if registry_changed {
            state.refresh_stations();
        }
        if global_reason != reason_before && global_reason.is_some() {
            global_since = global_since.min(start_at.unwrap_or(at).max(input.since));
        }
        let after = state.stations();
        let names: BTreeSet<_> = before.keys().chain(after.keys()).cloned().collect();
        for name in names {
            if checkpoint(budget).is_err() {
                break;
            }
            let flow = flows
                .entry(name.clone())
                .or_insert_with(|| MembershipFlow::new(name.clone(), &input.scope));
            if let Some((spec, basis)) = after.get(&name).or_else(|| before.get(&name)) {
                if !flow.versions.contains(basis) {
                    flow.versions.push(basis.clone());
                }
                if spec.predicate.binds_self() {
                    flow.caller_binding = input.actor.clone();
                }
            }
            if let Some(reason) = &global_reason {
                flow.unknown_span(global_since, input.until, reason.clone());
            }
            if interleaved {
                flow.unknown_span(
                    start_at.unwrap_or(input.since).max(input.since),
                    (at + chrono::Duration::microseconds(1)).min(input.until),
                    "interleaved original transaction groups do not establish packet state order",
                );
            }
            if start_at.is_some_and(|start| start < input.until) && at >= input.until {
                flow.unknown_span(
                    start_at.unwrap_or(input.since).max(input.since),
                    input.until,
                    "original atomic group crosses the requested window boundary",
                );
                continue;
            }
            if at < input.since || at >= input.until {
                continue;
            }
            if !complete {
                if group.rows.iter().any(|row| visible_row(row, input)) {
                    flow.unknown_span(
                        start_at.unwrap_or(at).max(input.since),
                        input.until,
                        evidence.clone().err().unwrap_or_else(|| {
                            "original transaction group or its complete prefix unavailable".into()
                        }),
                    );
                }
                continue;
            }
            if start_at.is_some_and(|start| start < input.since) {
                flow.unknown_span(
                    input.since,
                    input.until,
                    "original atomic group crosses the requested window boundary",
                );
                continue;
            }
            let old = before_members
                .get(&name)
                .cloned()
                .unwrap_or(Ok(BTreeMap::new()));
            let new = state.members_for(after.get(&name).map(|(s, _)| s), at, input, only);
            let (Ok(old), Ok(new)) = (&old, &new) else {
                flow.unknown_span(
                    at.max(input.since),
                    input.until,
                    old.err()
                        .or_else(|| new.err())
                        .unwrap_or("membership evidence unavailable".into()),
                );
                continue;
            };
            for id in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
                if checkpoint(budget).is_err() {
                    break;
                }
                let was = old.get(id).copied().unwrap_or(false);
                let now = new.get(id).copied().unwrap_or(false);
                if !was && now {
                    flow.known_arrived += 1;
                    if let Some(facts) = facts.as_deref_mut() {
                        facts.group(
                            &name,
                            id,
                            at,
                            group.original.as_ref(),
                            input.audit_cutoff,
                            end,
                            None,
                        );
                    }
                }
                if was && !now {
                    flow.known_departed += 1;
                    let packet = state.packets.get(id);
                    let cancelled = packet
                        .and_then(|p| p.job.as_ref())
                        .is_some_and(|j| j.status == JobStatus::Cancelled);
                    let completed = packet.is_some_and(|p| {
                        p.steps.values().any(|s| {
                            s.status == StepStatus::Completed
                                && !previous_completed.contains(&s.id.to_string())
                                && eligible_before
                                    .get(&name)
                                    .is_some_and(|ids| ids.contains(&s.id.to_string()))
                        })
                    });
                    let reassigned = packet.is_some_and(|p| {
                        p.steps.values().any(|s| {
                            eligible_before
                                .get(&name)
                                .is_some_and(|ids| ids.contains(&s.id.to_string()))
                                && previous_assignees
                                    .get(&s.id.to_string())
                                    .is_some_and(|before| before != &s.assignee_id)
                        })
                    });
                    let cause = if registry_changed {
                        if group.rows.iter().any(|r| {
                            matches!(
                                r.event.kind.as_str(),
                                crate::events::STATION_PUBLISHED
                                    | crate::events::WORKFLOW_PUBLISHED
                            )
                        }) {
                            "registry-publication"
                        } else {
                            "registry-retirement"
                        }
                    } else if cancelled {
                        "cancellation"
                    } else if completed {
                        "work-completed"
                    } else if reassigned {
                        "reassignment"
                    } else {
                        "predicate-state-change"
                    };
                    let served = cause == "work-completed";
                    if served {
                        flow.known_served += 1;
                    }
                    *flow.departure_causes.entry(cause.into()).or_default() += 1;
                    if let Some(facts) = facts.as_deref_mut() {
                        facts.group(
                            &name,
                            id,
                            at,
                            group.original.as_ref(),
                            input.audit_cutoff,
                            end,
                            Some((cause, served)),
                        );
                    }
                }
            }
        }
    }
    if let Err(reason) = seed_through(&mut state, input, &mut seeds, i64::MAX) {
        global_reason = Some(reason);
        global_since = input.since;
    }
    if !initial_checked {
        check_initial_state(&state, input, &mut flows);
    }
    accrue_expiry(
        &state,
        last_at,
        input.until,
        input,
        &mut flows,
        &mut expiries,
        facts,
    );
    for flow in flows.values_mut() {
        if checkpoint(budget).is_err() {
            break;
        }
        for gap in &input.source_unavailable {
            if checkpoint(budget).is_err() {
                break;
            }
            flow.unknown_span(
                gap.since.max(input.since),
                gap.until.min(input.until),
                gap.reason.clone(),
            );
        }
        if let Some(reason) = &global_reason {
            flow.unknown_span(global_since, input.until, reason.clone());
        }
        let mut gaps: Vec<_> = flow
            .unknown
            .iter()
            .map(|u| (u.since.max(input.since), u.until.min(input.until)))
            .filter(|(a, b)| a < b)
            .collect();
        gaps.sort_unstable();
        let mut cursor = input.since;
        for (since, until) in gaps {
            if checkpoint(budget).is_err() {
                break;
            }
            if since > cursor {
                flow.covered.push(Coverage {
                    since: cursor,
                    until: since,
                });
            }
            cursor = cursor.max(until);
        }
        if cursor < input.until {
            flow.covered.push(Coverage {
                since: cursor,
                until: input.until,
            });
        }
        if flow.unknown.is_empty() {
            flow.arrived = Some(flow.known_arrived);
            flow.departed = Some(flow.known_departed);
            flow.served = Some(flow.known_served);
        } else {
            flow.arrived = None;
            flow.departed = None;
            flow.served = None;
        }
        // Serving a completed reading needs the original spans, not a
        // second owned copy of them in the construction-time index.
        flow.unknown_seen = HashSet::new();
    }
    ReplayFold {
        reading: MembershipReading {
            stations: flows.into_values().collect(),
            since: input.since,
            until: input.until,
            audit_cutoff: input.audit_cutoff,
            denied,
        },
        state,
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use boss_clock_client::ClockMode;
    use boss_clock_client::mapping_history::ClockMappingFact;
    use boss_core::{
        event::Event,
        job::{Priority, Subject},
    };
    use boss_events::original_group::GroupReference;
    use chrono::TimeZone;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_760_000_000 + seconds, 0).unwrap()
    }

    fn spec() -> StationSpec {
        let rows: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/station-predicate-population-20261016.json"
        ))
        .unwrap();
        let mut spec: StationSpec = serde_json::from_value(rows["data"][0].clone()).unwrap();
        spec.predicate = Default::default();
        spec
    }

    fn packet() -> Job {
        Job::new(
            "fixture",
            Subject::new("custom", "fixture"),
            "Original packet",
            "alice",
            Priority::Standard,
            at(0).date_naive(),
        )
    }

    fn input() -> ReplayInput {
        let fact = ClockMappingFact {
            schema_version: 1,
            revision: "clock".into(),
            previous_revision: None,
            epoch: Some("boot".into()),
            mode: Some(ClockMode::Wall),
            params: None,
            captured_at: at(-100),
            audit_created_at: at(-99),
            seq: 1,
            complete_group: true,
        };
        let events = [
            Event::new(
                "jobs",
                crate::events::STATION_PUBLISHED,
                serde_json::json!(spec()),
                at(1),
            ),
            Event::new(
                "jobs",
                crate::events::JOB_CREATED,
                serde_json::json!(packet()),
                at(1),
            ),
        ];
        let rows: Vec<_> = events
            .into_iter()
            .enumerate()
            .map(|(index, event)| HistoricalEvent {
                seq: index as i64 + 2,
                created_at: Some(at(1)),
                event,
            })
            .collect();
        let ids: Vec<_> = rows.iter().map(|row| row.event.id).collect();
        let original = OriginalGroup {
            reference: GroupReference {
                id: Uuid::new_v4(),
                schema_version: 1,
                event_count: ids.len() as i64,
                member_hash: OriginalGroup::roster_hash(&ids),
            },
            event_ids: ids,
            audit_sequences: rows.iter().map(|row| Some(row.seq)).collect(),
            complete: true,
        };
        ReplayInput {
            groups: vec![ReplayGroup {
                original: Some(original),
                rows,
            }],
            since: at(0),
            until: at(20),
            audit_cutoff: 100,
            actor: Some("alice".into()),
            scope: JobScope::All,
            currently_authorized: None,
            clock: ClockTimeline::build(&[fact], Some("clock"), at(-100), at(20)),
            registry_prefix_complete: true,
            packet_prefix_complete: true,
            source_unavailable: vec![],
            initial: InitialState::default(),
        }
    }

    fn budget(fuel: usize) -> ReplayBudget {
        ReplayBudget::new(
            Instant::now() + Duration::from_secs(60),
            Arc::new(AtomicBool::new(false)),
            fuel,
        )
    }

    #[test]
    fn ample_checked_replay_preserves_the_entire_original_reading() {
        let input = input();
        let expected = replay_windows(input.clone(), at(10)).unwrap();
        assert!(!expected[1].stations.is_empty());
        let actual = replay_windows_checked(input, at(10), &budget(1_000_000)).unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }

    #[test]
    fn a_membership_scan_spends_budget_inside_one_group() {
        let budget = budget(8);
        let mut state = State {
            budget: Some(&budget),
            ..State::default()
        };
        for _ in 0..64 {
            let job = packet();
            state.packets.insert(
                job.id.to_string(),
                Packet {
                    job: Some(job),
                    complete: true,
                    ..Packet::default()
                },
            );
        }
        let result = state.members_for(Some(&spec()), at(1), &input(), None);
        assert!(
            result.is_err(),
            "one membership scan must stop inside its packet loop"
        );
        assert!(
            replay_checked(&input(), &budget).is_err(),
            "exhaustion is sticky, never a partial result"
        );
    }

    #[test]
    fn both_windows_share_one_budget_instead_of_refilling_it() {
        let input = input();
        let mut exact = None;
        for fuel in 1..1_000 {
            if replay_checked(&input, &budget(fuel)).is_ok() {
                exact = Some(fuel);
                break;
            }
        }
        let one_window = exact.expect("fixture must finish within the bounded fuel search");
        assert!(replay_windows_checked(input, at(10), &budget(one_window)).is_err());
    }

    #[test]
    fn cancellation_and_an_elapsed_deadline_refuse_before_replay() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let budget = ReplayBudget::new(
            Instant::now() + Duration::from_secs(60),
            cancelled.clone(),
            1_000_000,
        );
        cancelled.store(true, Ordering::Release);
        assert!(replay_checked(&input(), &budget).is_err());
        let expired = ReplayBudget::new(
            Instant::now() - Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
            1_000_000,
        );
        assert!(replay_windows_checked(input(), at(10), &expired).is_err());
    }
}
