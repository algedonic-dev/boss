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
use std::collections::{BTreeMap, BTreeSet};

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
    /// Current authorized identities for a narrow read, observed in
    /// the same native snapshot. None is valid only for global scope.
    pub currently_authorized: Option<BTreeSet<String>>,
    pub clock: ClockTimeline,
    pub registry_prefix_complete: bool,
    /// A native identity/Step roster may disprove a complete original
    /// prefix. It never supplies missing historical state.
    #[serde(default)]
    pub packet_prefix_complete: bool,
    #[serde(default)]
    pub source_unavailable: Vec<Unavailable>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub covered: Vec<Coverage>,
    pub versions: Vec<VersionBasis>,
    pub caller_binding: Option<String>,
    pub scope: JobScope,
}
impl MembershipFlow {
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
            covered: vec![],
            versions: vec![],
            caller_binding: None,
            scope: scope.clone(),
        }
    }
    fn unknown(&mut self, input: &ReplayInput, reason: impl Into<String>) {
        let entry = Unavailable {
            since: input.since,
            until: input.until,
            reason: reason.into(),
        };
        if !self.unknown.contains(&entry) {
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
            if !self.unknown.contains(&entry) {
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
#[derive(Default)]
struct Packet {
    job: Option<Job>,
    steps: BTreeMap<String, Step>,
    complete: bool,
    reason: Option<String>,
}
#[derive(Default)]
struct State {
    packets: BTreeMap<String, Packet>,
    authored: BTreeMap<String, (StationSpec, i64)>,
    workflows: BTreeMap<String, (WorkflowSpec, i64)>,
    registry_at: Option<DateTime<Utc>>,
    station_cache: BTreeMap<String, (StationSpec, VersionBasis)>,
}
impl State {
    fn packets_for(&self, only: Option<&BTreeSet<String>>) -> Vec<(&String, &Packet)> {
        match only {
            Some(ids) => ids
                .iter()
                .filter_map(|id| self.packets.get_key_value(id))
                .collect(),
            None => self.packets.iter().collect(),
        }
    }
    fn refresh_stations(&mut self) {
        let mut out: BTreeMap<_, _> = self
            .authored
            .iter()
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
        self.station_cache.clone()
    }
    fn members_for(
        &self,
        spec: Option<&StationSpec>,
        at: DateTime<Utc>,
        input: &ReplayInput,
        only: Option<&BTreeSet<String>>,
    ) -> Result<BTreeMap<String, bool>, String> {
        let Some(spec) = spec else {
            return Ok(BTreeMap::new());
        };
        let bound = spec
            .bind_self(input.actor.as_deref())
            .ok_or("actual caller binding unavailable")?;
        let mut out = BTreeMap::new();
        let candidates = self.packets_for(only);
        for (id, packet) in candidates {
            if input.scope != JobScope::All
                && !input
                    .currently_authorized
                    .as_ref()
                    .is_some_and(|ids| ids.contains(id))
            {
                continue;
            }
            let Some(job) = &packet.job else {
                return Err("original first Job snapshot missing".into());
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
            let steps = packet.steps.values().cloned().collect::<Vec<_>>();
            if bound.predicate.needs_steps() && !self.workflows.contains_key(&job.kind) {
                return Err("original applicable workflow publication missing".into());
            }
            let steps = crate::agent_spec::resolved_steps(
                &steps,
                self.workflows.get(&job.kind).map(|(w, _)| w),
            );
            if !bound.predicate.matches(job, &steps) {
                out.insert(id.clone(), false);
                continue;
            }
            let today = if matches!(job.status, JobStatus::Closed | JobStatus::Cancelled)
                && bound.terminal_window_days.is_some()
            {
                input.clock.date_at(at)?
            } else {
                job.opened_on
            }; // in_window does not inspect this date for live/no-retention packets.
            out.insert(
                id.clone(),
                crate::station_queue::in_window(job, today, bound.terminal_window_days)
                    && bound.predicate.matches(job, &steps),
            );
        }
        Ok(out)
    }
    fn apply(&mut self, row: &HistoricalEvent, complete: bool) -> Result<(), String> {
        let e = &row.event;
        match e.kind.as_str() {
            crate::events::JOB_CREATED | crate::events::JOB_UPDATED => {
                let job: Job =
                    serde_json::from_value(e.payload.clone()).map_err(|e| e.to_string())?;
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
            crate::events::STEP_CREATED | crate::events::STEP_UPDATED => {
                let step: Step =
                    serde_json::from_value(e.payload.clone()).map_err(|e| e.to_string())?;
                let packet = self.packets.entry(step.job_id.to_string()).or_default();
                let exists = packet.steps.contains_key(&step.id.to_string());
                if (e.kind == crate::events::STEP_CREATED && exists)
                    || (e.kind == crate::events::STEP_UPDATED && !exists)
                {
                    packet.complete = false;
                    packet.reason = Some("duplicate or missing original Step creation".into());
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
            crate::events::STEP_FIRST_RECORDED => {
                let ev = boss_events::replay::ReplayEvent {
                    audit_id: row.seq,
                    event_id: e.id,
                    kind: e.kind.clone(),
                    ts: e.timestamp,
                    payload: e.payload.clone(),
                };
                let (step, _) = crate::state_receipt::first_record(&ev)?;
                let packet = self.packets.entry(step.job_id.to_string()).or_default();
                if !packet.steps.contains_key(&step.id.to_string()) {
                    packet.complete = false;
                    packet.reason =
                        Some("original Step creation missing before first record".into());
                }
                packet.complete &= complete;
                packet.steps.insert(step.id.to_string(), step);
            }
            crate::events::STATION_CLAIMED => {
                let ev = boss_events::replay::ReplayEvent {
                    audit_id: row.seq,
                    event_id: e.id,
                    kind: e.kind.clone(),
                    ts: e.timestamp,
                    payload: e.payload.clone(),
                };
                let text = e
                    .payload
                    .get("receipt_json")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("original exact claim JSON missing")?;
                let (step, _) = crate::state_receipt::station_claim(&ev, text)?;
                let packet = self.packets.entry(step.job_id.to_string()).or_default();
                if !packet.steps.contains_key(&step.id.to_string()) {
                    packet.complete = false;
                    packet.reason =
                        Some("original Step creation missing before station claim".into());
                }
                packet.complete &= complete;
                packet.steps.insert(step.id.to_string(), step);
            }
            _ => {}
        }
        Ok(())
    }
}

fn packet_identity(row: &HistoricalEvent) -> Option<String> {
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
    if input.scope == JobScope::All {
        return true;
    }
    let id = packet_identity(row);
    id.is_some_and(|id| {
        input
            .currently_authorized
            .as_ref()
            .is_some_and(|ids| ids.contains(&id))
    })
}

fn accrue_expiry(
    state: &State,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    input: &ReplayInput,
    flows: &mut BTreeMap<String, MembershipFlow>,
    counted: &mut BTreeSet<(String, String, i64)>,
) {
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
    for (name, (spec, _)) in state.stations() {
        let Some(days) = spec.terminal_window_days else {
            continue;
        };
        let Some(bound) = spec.bind_self(input.actor.as_deref()) else {
            continue;
        };
        let flow = flows
            .entry(name.clone())
            .or_insert_with(|| MembershipFlow::new(name.clone(), &input.scope));
        for (id, packet) in &state.packets {
            if input.scope != JobScope::All
                && !input
                    .currently_authorized
                    .as_ref()
                    .is_some_and(|ids| ids.contains(id))
            {
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
                flow.unknown_span(from.max(gap.since), to.min(gap.until), gap.reason.clone());
            }
            for span in &input.clock.known {
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
                    if before
                        .and_then(|wall| input.clock.date_at(wall).ok())
                        .is_none_or(|date| date >= target)
                    {
                        continue;
                    }
                }
                let mut left = low.timestamp_micros();
                let mut right = high.timestamp_micros();
                while left < right {
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
        if let Err(reason) = state.members_for(Some(&spec), input.since, input, None) {
            flows
                .entry(name.clone())
                .or_insert_with(|| MembershipFlow::new(name, &input.scope))
                .unknown(input, reason);
        }
    }
}

pub fn replay(input: &ReplayInput) -> MembershipReading {
    let mut state = State::default();
    let mut flows = BTreeMap::<String, MembershipFlow>::new();
    let denied = input.scope == JobScope::None;
    if denied {
        return MembershipReading {
            stations: vec![],
            since: input.since,
            until: input.until,
            audit_cutoff: input.audit_cutoff,
            denied,
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
    let mut prior_ranges = Vec::<(i64, i64, BTreeSet<String>, bool)>::new();
    for original_group in &input.groups {
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
        let possible = prior_ranges.partition_point(|(_, high, _, _)| *high < first);
        let interleaved = prior_ranges[possible..]
            .iter()
            .any(|(low, high, ids, registry)| {
                first <= *high
                    && *low <= end
                    && (registry_group || *registry || !affected.is_disjoint(ids))
            });
        prior_ranges.push((first, end, affected.clone(), registry_group));
        let only = if registry_group {
            None
        } else {
            Some(&affected)
        };
        let before_members: BTreeMap<_, _> = if at >= input.since && at < input.until {
            before
                .iter()
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
            .filter(|s| s.status == StepStatus::Completed)
            .map(|s| s.id.to_string())
            .collect();
        let previous_assignees: BTreeMap<_, _> = state
            .packets_for(only)
            .into_iter()
            .flat_map(|(_, p)| p.steps.values())
            .map(|s| (s.id.to_string(), s.assignee_id.clone()))
            .collect();
        let eligible_before: BTreeMap<String, BTreeSet<String>> = before
            .iter()
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
                            .filter(|s| predicate.matches(s))
                            .map(|s| s.id.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                (name.clone(), ids)
            })
            .collect();
        let mut registry_changed = false;
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
        if interleaved {
            for id in &affected {
                if let Some(packet) = state.packets.get_mut(id) {
                    packet.complete = false;
                    packet.reason=Some("interleaved original transaction groups do not establish packet state order".into());
                }
            }
        }
        for row in ordered {
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
                let was = old.get(id).copied().unwrap_or(false);
                let now = new.get(id).copied().unwrap_or(false);
                if !was && now {
                    flow.known_arrived += 1;
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
                    if cause == "work-completed" {
                        flow.known_served += 1;
                    }
                    *flow.departure_causes.entry(cause.into()).or_default() += 1;
                }
            }
        }
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
    );
    for flow in flows.values_mut() {
        for gap in &input.source_unavailable {
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
    }
    MembershipReading {
        stations: flows.into_values().collect(),
        since: input.since,
        until: input.until,
        audit_cutoff: input.audit_cutoff,
        denied,
    }
}
