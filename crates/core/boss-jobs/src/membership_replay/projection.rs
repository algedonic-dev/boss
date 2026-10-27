//! An incremental, caller-bound domain projection, not an endpoint cache.
//!
//! A rebuild uses the original replay fold once and retains only its latest
//! packet/registry state, original pruned census and compact crossing facts.
//! An advance folds only new complete groups. Reads touch the bounded fact ledger, never packet
//! payloads. The PostgreSQL updater, persistence and live latency proof are
//! deliberately separate from this domain primitive (partial c7870939).
//!
//! This first component advances a KNOWN generation. Changed authorization,
//! registry/clock history, backdated or cross-cut groups require a witnessed
//! rebuild. An unknown fold is returned as unread, never promoted to ready.

use super::*;
use boss_events::original_group::GroupReference;

const SCHEMA: u32 = 2;
const MAX_FACTS: usize = 250_000;
const MAX_SOURCE_IDENTITIES: usize = 250_000;
pub const MAX_CHECKPOINT_BYTES: usize = 128 * 1024 * 1024;
const MAX_WINDOW_HOURS: i64 = 48;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionBinding {
    pub actor: Option<String>,
    pub scope: JobScope,
    pub currently_authorized: Option<BTreeSet<String>>,
}

impl ProjectionBinding {
    pub fn from_input(input: &ReplayInput) -> Self {
        Self {
            actor: input.actor.clone(),
            scope: input.scope.clone(),
            currently_authorized: input.currently_authorized.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FactSource {
    OriginalGroup {
        reference: GroupReference,
        audit_cutoff: i64,
        audit_end: i64,
    },
    ClockExpiry {
        revision: String,
        audit_cutoff: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Crossing {
    Arrival,
    Departure { cause: String, served: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembershipFact {
    pub at: DateTime<Utc>,
    pub station: String,
    pub packet: String,
    pub crossing: Crossing,
    pub source: FactSource,
}

#[derive(Debug)]
pub enum ProjectionRefusal {
    RebuildRequired(String),
    /// The exact oracle refusal, including every original unknown span.
    Unread(Box<MembershipReading>),
}

impl std::fmt::Display for ProjectionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RebuildRequired(why) => {
                write!(f, "membership projection rebuild required: {why}")
            }
            Self::Unread(reading) => write!(
                f,
                "membership projection unread at audit cut {}",
                reading.audit_cutoff
            ),
        }
    }
}
impl std::error::Error for ProjectionRefusal {}

fn rebuild(why: impl Into<String>) -> ProjectionRefusal {
    ProjectionRefusal::RebuildRequired(why.into())
}

// No old ReplayInput or derived station-cache copy survives here. The
// separately retained pruned census keeps its own original source context.
// The station cache is derived from these registry facts.
#[derive(Clone, Serialize, Deserialize)]
struct CurrentState {
    packets: BTreeMap<String, Packet>,
    authored: BTreeMap<String, (StationSpec, i64)>,
    workflows: BTreeMap<String, (WorkflowSpec, i64)>,
    registry_at: Option<DateTime<Utc>>,
}

impl CurrentState {
    fn take(state: State<'_>) -> Self {
        Self {
            packets: state.packets,
            authored: state.authored,
            workflows: state.workflows,
            registry_at: state.registry_at,
        }
    }

    fn restore<'a>(&self, budget: Option<&'a ReplayBudget>) -> State<'a> {
        let mut state = State {
            budget,
            packets: self.packets.clone(),
            authored: self.authored.clone(),
            workflows: self.workflows.clone(),
            registry_at: self.registry_at,
            station_cache: BTreeMap::new(),
        };
        state.refresh_stations();
        state
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MembershipProjection {
    schema: u32,
    binding: ProjectionBinding,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    audit_cutoff: i64,
    state: CurrentState,
    clock: ClockTimeline,
    prototypes: Vec<MembershipFlow>,
    facts: Vec<MembershipFact>,
    seen: BTreeSet<uuid::Uuid>,
    group_spans: Vec<(DateTime<Utc>, DateTime<Utc>)>,
    #[serde(default)]
    pruned_census: Option<pruned::PrunedCensus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvanceStats {
    pub source_rows: usize,
    pub crossing_facts: usize,
}

pub(super) struct FactRecorder {
    facts: Vec<MembershipFact>,
    refused: Option<String>,
}

impl FactRecorder {
    fn new() -> Self {
        Self {
            facts: vec![],
            refused: None,
        }
    }

    fn push(&mut self, fact: MembershipFact) {
        if self.facts.len() == MAX_FACTS {
            self.refused = Some("crossing fact bound exceeded".into());
        } else {
            self.facts.push(fact);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn group(
        &mut self,
        station: &str,
        packet: &str,
        at: DateTime<Utc>,
        group: Option<&OriginalGroup>,
        cutoff: i64,
        end: i64,
        departure: Option<(&str, bool)>,
    ) {
        let Some(group) = group else {
            self.refused = Some("a crossing lacks its complete original group".into());
            return;
        };
        self.push(MembershipFact {
            at,
            station: station.into(),
            packet: packet.into(),
            crossing: departure.map_or(Crossing::Arrival, |(cause, served)| Crossing::Departure {
                cause: cause.into(),
                served,
            }),
            source: FactSource::OriginalGroup {
                reference: group.reference.clone(),
                audit_cutoff: cutoff,
                audit_end: end,
            },
        });
    }

    pub(super) fn expiry(
        &mut self,
        station: &str,
        packet: &str,
        micros: i64,
        revision: &str,
        cutoff: i64,
    ) {
        let Some(at) = DateTime::<Utc>::from_timestamp_micros(micros) else {
            self.refused = Some("clock crossing instant is unavailable".into());
            return;
        };
        self.push(MembershipFact {
            at,
            station: station.into(),
            packet: packet.into(),
            crossing: Crossing::Departure {
                cause: "terminal-expiry".into(),
                served: false,
            },
            source: FactSource::ClockExpiry {
                revision: revision.into(),
                audit_cutoff: cutoff,
            },
        });
    }
}

impl MembershipProjection {
    pub fn binding(&self) -> &ProjectionBinding {
        &self.binding
    }
    pub fn since(&self) -> DateTime<Utc> {
        self.since
    }
    pub fn until(&self) -> DateTime<Utc> {
        self.until
    }
    pub fn audit_cutoff(&self) -> i64 {
        self.audit_cutoff
    }
    /// Replace only the named packets from a complete retained-history
    /// witness at the new cut. Unchanged packet history is not reread.
    /// Caller/scope and registry/clock changes still require a rebuild.
    /// Every check prepares an owned candidate; refusal preserves self.
    pub fn reconcile(
        &mut self,
        delta: &ReplayInput,
        replacement_history: Option<&ReplayInput>,
        replacement_ids: &BTreeSet<String>,
    ) -> Result<AdvanceStats, ProjectionRefusal> {
        self.reconcile_inner(delta, replacement_history, replacement_ids, None)
    }

    pub fn reconcile_checked(
        &mut self,
        delta: &ReplayInput,
        replacement_history: Option<&ReplayInput>,
        replacement_ids: &BTreeSet<String>,
        budget: &ReplayBudget,
    ) -> Result<AdvanceStats, ProjectionRefusal> {
        self.reconcile_inner(delta, replacement_history, replacement_ids, Some(budget))
    }

    fn reconcile_inner(
        &mut self,
        delta: &ReplayInput,
        replacement_history: Option<&ReplayInput>,
        replacement_ids: &BTreeSet<String>,
        budget: Option<&ReplayBudget>,
    ) -> Result<AdvanceStats, ProjectionRefusal> {
        check(budget)?;
        if delta.actor != self.binding.actor || delta.scope != self.binding.scope {
            return Err(rebuild("caller or historical scope changed"));
        }
        let target = ProjectionBinding::from_input(delta);
        if self.pruned_census.is_some() && target != self.binding {
            return Err(rebuild("original pruned census authorization changed"));
        }
        if self.pruned_census.as_ref().is_some_and(|witness| {
            witness
                .packets
                .iter()
                .any(|packet| replacement_ids.contains(&packet.id))
        }) {
            return Err(rebuild("replacement touches an originally pruned packet"));
        }
        if self.binding.currently_authorized.is_some() && target.currently_authorized.is_none() {
            return Err(rebuild(
                "unrestricted authorization requires a complete retained-history rebuild",
            ));
        }
        if replacement_ids
            .iter()
            .any(|id| !authorized_packet(delta, id))
        {
            return Err(rebuild("replacement packet is not currently authorized"));
        }
        if let Some(ids) = &target.currently_authorized {
            let old = self.binding.currently_authorized.as_ref();
            if ids
                .iter()
                .any(|id| old.is_some_and(|old| !old.contains(id)) && !replacement_ids.contains(id))
            {
                return Err(rebuild("newly authorized packet lacks retained history"));
            }
        }
        let mut continuation = delta.clone();
        let mut replaced_source = BTreeSet::new();
        continuation.groups.retain(|group| {
            if check(budget).is_err() {
                return false;
            }
            let selected = group
                .rows
                .iter()
                .filter_map(packet_identity)
                .any(|id| replacement_ids.contains(&id));
            if selected {
                replaced_source.extend(group.rows.iter().map(|row| row.event.id));
            }
            !selected
        });
        for group in &delta.groups {
            check(budget)?;
            if group
                .rows
                .iter()
                .any(|row| replaced_source.contains(&row.event.id))
            {
                for row in &group.rows {
                    check(budget)?;
                    if let Some(id) = packet_identity(row) {
                        if authorized_packet(delta, &id) && !replacement_ids.contains(&id) {
                            return Err(rebuild(
                                "replacement must retain the whole visible original group",
                            ));
                        }
                    } else if matches!(
                        row.event.kind.as_str(),
                        crate::events::STATION_PUBLISHED
                            | crate::events::STATION_RETIRED
                            | crate::events::WORKFLOW_PUBLISHED
                            | crate::events::WORKFLOW_RETIRED
                            | "clock.mapping.recorded"
                    ) {
                        return Err(rebuild("registry or clock generation changed"));
                    }
                }
            }
        }
        let mut candidate = self.clone();
        check(budget)?;
        candidate
            .state
            .packets
            .retain(|id, _| authorized_packet(delta, id) && !replacement_ids.contains(id));
        candidate.facts.retain(|fact| {
            authorized_packet(delta, &fact.packet) && !replacement_ids.contains(&fact.packet)
        });
        // A census retains its original full caller authorization. State
        // and continuation groups already exclude selected replacements.
        if self.pruned_census.is_none() {
            let remaining: BTreeSet<_> = match &target.currently_authorized {
                Some(ids) => ids
                    .iter()
                    .filter(|id| !replacement_ids.contains(*id))
                    .cloned()
                    .collect(),
                None => candidate
                    .state
                    .packets
                    .keys()
                    .cloned()
                    .chain(
                        continuation
                            .groups
                            .iter()
                            .flat_map(|group| &group.rows)
                            .filter_map(packet_identity),
                    )
                    .filter(|id| !replacement_ids.contains(id))
                    .collect(),
            };
            candidate.binding.currently_authorized = Some(remaining.clone());
            continuation.currently_authorized = Some(remaining);
        }
        let mut stats = candidate.advance_inner(&continuation, budget)?;
        if !replacement_ids.is_empty() {
            let mut history = replacement_history
                .ok_or_else(|| rebuild("replacement retained history unavailable"))?
                .clone();
            if history.actor != delta.actor
                || history.scope != delta.scope
                || history.since != self.since
                || history.until != delta.until
                || history.audit_cutoff != delta.audit_cutoff
            {
                return Err(rebuild(
                    "replacement history does not witness the same caller and native cut",
                ));
            }
            history.currently_authorized = Some(replacement_ids.clone());
            let donor = Self::rebuild_inner(&history, budget)?;
            if donor.pruned_census.is_some() {
                return Err(rebuild(
                    "replacement introduces a different original pruned census",
                ));
            }
            if serde_json::to_value(&donor.state.authored)
                .map_err(|why| rebuild(why.to_string()))?
                != serde_json::to_value(&candidate.state.authored)
                    .map_err(|why| rebuild(why.to_string()))?
                || serde_json::to_value(&donor.state.workflows)
                    .map_err(|why| rebuild(why.to_string()))?
                    != serde_json::to_value(&candidate.state.workflows)
                        .map_err(|why| rebuild(why.to_string()))?
                || clock_prefix(&donor.clock, self.since, delta.until)?
                    != clock_prefix(&candidate.clock, self.since, delta.until)?
            {
                return Err(rebuild("replacement registry or clock basis differs"));
            }
            if !replaced_source.is_subset(&donor.seen) {
                return Err(rebuild("replacement history lacks original delta members"));
            }
            stats.source_rows += history
                .groups
                .iter()
                .map(|group| group.rows.len())
                .sum::<usize>();
            stats.crossing_facts += donor.facts.len();
            candidate.state.packets.extend(donor.state.packets);
            candidate.facts.extend(donor.facts);
            candidate.facts.sort_by_key(|fact| fact.at);
            candidate.seen.extend(donor.seen);
            candidate.group_spans.extend(donor.group_spans);
        } else if replacement_history.is_some() {
            return Err(rebuild(
                "unexpected replacement history without selected packets",
            ));
        }
        candidate.group_spans.sort_unstable();
        candidate.group_spans.dedup();
        candidate.binding = target;
        candidate.check_bound()?;
        check(budget)?;
        *self = candidate;
        Ok(stats)
    }

    pub fn rebuild(input: &ReplayInput) -> Result<Self, ProjectionRefusal> {
        Self::rebuild_inner(input, None)
    }

    pub fn rebuild_checked(
        input: &ReplayInput,
        budget: &ReplayBudget,
    ) -> Result<Self, ProjectionRefusal> {
        Self::rebuild_inner(input, Some(budget))
    }

    fn rebuild_inner(
        input: &ReplayInput,
        budget: Option<&ReplayBudget>,
    ) -> Result<Self, ProjectionRefusal> {
        check(budget)?;
        if input.since >= input.until || input.audit_cutoff < 0 || input.scope == JobScope::None {
            return Err(rebuild("invalid or denied native projection cut"));
        }
        let seen = source_identities(input)?;
        let group_spans = group_spans(input)?;
        let mut recorder = FactRecorder::new();
        let folded = replay_fold(input, budget, None, Some(&mut recorder));
        check(budget)?;
        verify_fold(&folded.reading, &recorder)?;
        require_source_evidence(input, input.since, budget)?;
        let mut facts = recorder.facts;
        facts.sort_by_key(|f| f.at);
        let projection = Self {
            schema: SCHEMA,
            binding: ProjectionBinding::from_input(input),
            since: input.since,
            until: input.until,
            audit_cutoff: input.audit_cutoff,
            state: CurrentState::take(folded.state),
            clock: input.clock.clone(),
            prototypes: folded.reading.stations,
            facts,
            seen,
            group_spans,
            pruned_census: input.initial.pruned_census.clone(),
        };
        projection.check_bound()?;
        Ok(projection)
    }

    pub fn advance(&mut self, delta: &ReplayInput) -> Result<AdvanceStats, ProjectionRefusal> {
        self.advance_inner(delta, None)
    }

    pub fn advance_checked(
        &mut self,
        delta: &ReplayInput,
        budget: &ReplayBudget,
    ) -> Result<AdvanceStats, ProjectionRefusal> {
        self.advance_inner(delta, Some(budget))
    }

    fn advance_inner(
        &mut self,
        delta: &ReplayInput,
        budget: Option<&ReplayBudget>,
    ) -> Result<AdvanceStats, ProjectionRefusal> {
        check(budget)?;
        self.validate_pruned_source()?;
        if self.schema != SCHEMA || ProjectionBinding::from_input(delta) != self.binding {
            return Err(rebuild(
                "caller, scope or current authorized census changed",
            ));
        }
        if delta.since != self.until
            || delta.until <= self.until
            || delta.audit_cutoff < self.audit_cutoff
        {
            return Err(rebuild("delta does not continue the witnessed native cut"));
        }
        if delta.initial.cut != 0
            || !delta.initial.rows.is_empty()
            || !delta.initial.unknown.is_empty()
            || delta.initial.pruned_terminal != 0
            || delta.initial.pruned_census.is_some()
        {
            return Err(rebuild(
                "an advance cannot replace its original starting state",
            ));
        }
        if clock_prefix(&self.clock, self.since, self.until)?
            != clock_prefix(&delta.clock, self.since, self.until)?
        {
            return Err(rebuild("the original clock prefix changed"));
        }
        let identities = source_identities(delta)?;
        if identities.iter().any(|id| self.seen.contains(id)) {
            return Err(rebuild("delta repeats an original event identity"));
        }
        for row in delta.groups.iter().flat_map(|g| &g.rows) {
            check(budget)?;
            if self.pruned_census.as_ref().is_some_and(|witness| {
                packet_identity(row)
                    .is_some_and(|id| witness.packets.iter().any(|packet| packet.id == id))
            }) {
                return Err(rebuild("delta touches an originally pruned packet"));
            }
            if row.seq <= self.audit_cutoff || row.seq > delta.audit_cutoff {
                return Err(rebuild(
                    "original transaction group crosses the checkpoint cut",
                ));
            }
            if row
                .created_at
                .is_none_or(|at| at < self.until || at >= delta.until)
            {
                return Err(rebuild("backdated or future original insertion evidence"));
            }
            if matches!(
                row.event.kind.as_str(),
                crate::events::STATION_PUBLISHED
                    | crate::events::STATION_RETIRED
                    | crate::events::WORKFLOW_PUBLISHED
                    | crate::events::WORKFLOW_RETIRED
                    | "clock.mapping.recorded"
            ) {
                return Err(rebuild("registry or clock generation changed"));
            }
        }
        let spans = group_spans(delta)?;
        let mut recorder = FactRecorder::new();
        let folded = replay_fold(
            delta,
            budget,
            Some(self.state.restore(budget)),
            Some(&mut recorder),
        );
        check(budget)?;
        verify_fold(&folded.reading, &recorder)?;
        require_source_evidence(delta, self.since, budget)?;
        let stats = AdvanceStats {
            source_rows: delta.groups.iter().map(|g| g.rows.len()).sum(),
            crossing_facts: recorder.facts.len(),
        };
        // Prepare a separate owned generation: a refusal never mutates the
        // last good checkpoint or claims the new watermark was processed.
        let mut candidate = Self {
            schema: SCHEMA,
            binding: self.binding.clone(),
            since: self.since,
            until: delta.until,
            audit_cutoff: delta.audit_cutoff,
            state: CurrentState::take(folded.state),
            clock: delta.clock.clone(),
            prototypes: self.prototypes.clone(),
            facts: self.facts.clone(),
            seen: self.seen.clone(),
            group_spans: self.group_spans.clone(),
            pruned_census: self.pruned_census.clone(),
        };
        candidate.facts.extend(recorder.facts);
        candidate.facts.sort_by_key(|f| f.at);
        candidate.seen.extend(identities);
        candidate.group_spans.extend(spans);
        candidate.check_bound()?;
        check(budget)?;
        *self = candidate;
        Ok(stats)
    }

    /// Validate a generation header and requested window without reading facts.
    pub fn validate_window(
        &self,
        since: DateTime<Utc>,
        binding: &ProjectionBinding,
    ) -> Result<(), ProjectionRefusal> {
        self.validate_pruned_source()?;
        if self.schema != SCHEMA || binding != &self.binding {
            return Err(rebuild(
                "caller, scope or current authorized census changed",
            ));
        }
        if since < self.since
            || since >= self.until
            || self.until - since > chrono::Duration::hours(MAX_WINDOW_HOURS)
        {
            return Err(rebuild(
                "requested window is outside bounded witnessed projection history",
            ));
        }
        Ok(())
    }

    /// Read a window ending at this generation's OWN watermark, at most
    /// 48 hours and wholly inside its witnessed history. A current binding
    /// is required even when reading a previously known generation.
    pub fn read(
        &self,
        since: DateTime<Utc>,
        binding: &ProjectionBinding,
    ) -> Result<MembershipReading, ProjectionRefusal> {
        self.validate_window(since, binding)?;
        if self
            .group_spans
            .iter()
            .any(|(start, end)| *start < since && *end >= since)
        {
            return Err(rebuild(
                "original atomic group crosses the requested window boundary",
            ));
        }
        let start = self.facts.partition_point(|fact| fact.at < since);
        let mut stations: BTreeMap<_, _> = self
            .prototypes
            .iter()
            .cloned()
            .map(|mut flow| {
                flow.known_arrived = 0;
                flow.known_departed = 0;
                flow.known_served = 0;
                flow.arrived = Some(0);
                flow.departed = Some(0);
                flow.served = Some(0);
                flow.departure_causes.clear();
                flow.unknown.clear();
                flow.covered = vec![Coverage {
                    since,
                    until: self.until,
                }];
                (flow.station.clone(), flow)
            })
            .collect();
        for fact in &self.facts[start..] {
            let flow = stations
                .get_mut(&fact.station)
                .ok_or_else(|| rebuild("crossing station is absent from its registry basis"))?;
            count(flow, fact)?;
        }
        for flow in stations.values_mut() {
            flow.arrived = Some(flow.known_arrived);
            flow.departed = Some(flow.known_departed);
            flow.served = Some(flow.known_served);
        }
        Ok(MembershipReading {
            stations: stations.into_values().collect(),
            since,
            until: self.until,
            audit_cutoff: self.audit_cutoff,
            denied: false,
        })
    }

    /// Compact provenance facts for a persistence adapter, not event payloads.
    pub fn facts(&self) -> &[MembershipFact] {
        &self.facts
    }

    fn check_bound(&self) -> Result<(), ProjectionRefusal> {
        self.validate_pruned_source()?;
        if self.facts.len() > MAX_FACTS
            || self.seen.len() > MAX_SOURCE_IDENTITIES
            || self.group_spans.len() > MAX_SOURCE_IDENTITIES
        {
            return Err(rebuild("projection fact/identity bound exceeded"));
        }
        check_serialized_bound(self, MAX_CHECKPOINT_BYTES)
    }

    fn validate_pruned_source(&self) -> Result<(), ProjectionRefusal> {
        let mut certificates = self
            .prototypes
            .iter()
            .flat_map(|flow| &flow.pruned_exclusions);
        let Some(witness) = &self.pruned_census else {
            return if certificates.count() == 0 {
                Ok(())
            } else {
                Err(rebuild("pruned certificate lacks its original census"))
            };
        };
        if witness.schema != 1
            || witness.binding != self.binding
            || witness.since != self.since
            || witness.until > self.until
            || witness.cut > witness.audit_cutoff
            || witness.audit_cutoff > self.audit_cutoff
            || certificates.any(|certificate| {
                certificate.source_cut != witness.cut
                    || certificate.packets != witness.packets.len() as u64
                    || certificate.since < witness.since
                    || certificate.until > witness.until
                    || certificate.since >= certificate.until
            })
        {
            return Err(rebuild(
                "pruned certificate original source binding changed",
            ));
        }
        Ok(())
    }
}

/// Count the exact serde JSON encoding without allocating a second full checkpoint.
#[derive(Default)]
struct SerializedBytes(usize);

impl std::io::Write for SerializedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("serialized checkpoint length overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn serialized_len(value: &impl serde::Serialize) -> Result<usize, serde_json::Error> {
    let mut bytes = SerializedBytes::default();
    serde_json::to_writer(&mut bytes, value)?;
    Ok(bytes.0)
}

fn check_serialized_bound(
    value: &impl serde::Serialize,
    limit: usize,
) -> Result<(), ProjectionRefusal> {
    let bytes = serialized_len(value).map_err(|error| rebuild(error.to_string()))?;
    if bytes > limit {
        return Err(rebuild(
            "serialized current-state checkpoint exceeds byte bound",
        ));
    }
    Ok(())
}

fn check(budget: Option<&ReplayBudget>) -> Result<(), ProjectionRefusal> {
    checkpoint(budget).map_err(rebuild)
}

// An empty registry has no per-station flow on which replay can place a
// source refusal. Certify its evidence explicitly, after preserving any
// original oracle unknowns. Later evidence must cover retained history too.
fn require_source_evidence(
    input: &ReplayInput,
    since: DateTime<Utc>,
    budget: Option<&ReplayBudget>,
) -> Result<(), ProjectionRefusal> {
    let mut relevant = Vec::new();
    for gap in &input.source_unavailable {
        check(budget)?;
        if gap.since.max(since) < gap.until.min(input.until) {
            relevant.push(gap);
        }
    }
    if !input.registry_prefix_complete || !input.packet_prefix_complete || !relevant.is_empty() {
        let details =
            serde_json::to_string(&relevant).map_err(|error| rebuild(error.to_string()))?;
        return Err(rebuild(format!(
            "original source evidence incomplete: registry_prefix_complete={}, packet_prefix_complete={}, unavailable={details}",
            input.registry_prefix_complete, input.packet_prefix_complete,
        )));
    }
    Ok(())
}

fn source_identities(input: &ReplayInput) -> Result<BTreeSet<uuid::Uuid>, ProjectionRefusal> {
    let mut ids = BTreeSet::new();
    for row in input
        .initial
        .rows
        .iter()
        .chain(input.groups.iter().flat_map(|g| &g.rows))
    {
        if !ids.insert(row.event.id) {
            return Err(rebuild("duplicate original source identity"));
        }
        if ids.len() > MAX_SOURCE_IDENTITIES {
            return Err(rebuild("original source identity bound exceeded"));
        }
    }
    Ok(ids)
}

fn group_spans(
    input: &ReplayInput,
) -> Result<Vec<(DateTime<Utc>, DateTime<Utc>)>, ProjectionRefusal> {
    input
        .groups
        .iter()
        .map(|group| {
            let instants = group
                .rows
                .iter()
                .map(|r| r.created_at)
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| rebuild("original insertion instant missing"))?;
            let first = instants
                .iter()
                .min()
                .copied()
                .ok_or_else(|| rebuild("empty original group"))?;
            let last = instants
                .iter()
                .max()
                .copied()
                .ok_or_else(|| rebuild("empty original group"))?;
            Ok((first, last))
        })
        .collect()
}

fn clock_prefix(
    clock: &ClockTimeline,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<serde_json::Value, ProjectionRefusal> {
    let since = since
        .checked_sub_signed(chrono::Duration::microseconds(1))
        .ok_or_else(|| rebuild("clock prefix bound exceeds supported time"))?;
    let outcome_originals = clock.outcome_evidence_prefix(until);
    let mut clock = clock.clone();
    for span in &mut clock.known {
        span.since = span.since.max(since);
        span.until = span.until.min(until);
    }
    clock.known.retain(|s| s.since < s.until);
    for span in &mut clock.unknown {
        span.since = span.since.max(since);
        span.until = span.until.min(until);
    }
    clock.unknown.retain(|s| s.since < s.until);
    serde_json::to_value((&clock.known, &clock.unknown, outcome_originals))
        .map_err(|e| rebuild(e.to_string()))
}

fn count(flow: &mut MembershipFlow, fact: &MembershipFact) -> Result<(), ProjectionRefusal> {
    let add = |value: &mut u64| -> Result<(), ProjectionRefusal> {
        *value = value
            .checked_add(1)
            .ok_or_else(|| rebuild("projection counter overflow"))?;
        Ok(())
    };
    match &fact.crossing {
        Crossing::Arrival => add(&mut flow.known_arrived)?,
        Crossing::Departure { cause, served } => {
            add(&mut flow.known_departed)?;
            if *served {
                add(&mut flow.known_served)?;
            }
            add(flow.departure_causes.entry(cause.clone()).or_default())?;
        }
    }
    Ok(())
}

fn verify_fold(
    reading: &MembershipReading,
    recorder: &FactRecorder,
) -> Result<(), ProjectionRefusal> {
    if reading.denied || reading.stations.iter().any(|flow| !flow.unknown.is_empty()) {
        return Err(ProjectionRefusal::Unread(Box::new(reading.clone())));
    }
    if let Some(why) = &recorder.refused {
        return Err(rebuild(why.clone()));
    }
    for flow in &reading.stations {
        let mut tallied = MembershipFlow::new(flow.station.clone(), &flow.scope);
        for fact in recorder
            .facts
            .iter()
            .filter(|fact| fact.station == flow.station)
        {
            count(&mut tallied, fact)?;
        }
        if (
            tallied.known_arrived,
            tallied.known_departed,
            tallied.known_served,
            &tallied.departure_causes,
        ) != (
            flow.known_arrived,
            flow.known_departed,
            flow.known_served,
            &flow.departure_causes,
        ) {
            return Err(rebuild(
                "compact crossing facts do not conserve the replay fold",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod byte_measurement_tests {
    #[test]
    fn refused_json_encoding_is_never_an_empty_accepted_checkpoint() {
        struct Refusing;
        impl serde::Serialize for Refusing {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("fixture encoding refused"))
            }
        }
        assert!(
            super::check_serialized_bound(&Refusing, usize::MAX)
                .unwrap_err()
                .to_string()
                .contains("fixture encoding refused")
        );
    }

    #[test]
    fn byte_counter_overflow_is_a_refusal_and_keeps_the_previous_count() {
        use std::io::Write;
        let mut count = super::SerializedBytes(usize::MAX);
        assert!(count.write(b"x").is_err());
        assert_eq!(count.0, usize::MAX);
    }

    #[test]
    fn checkpoint_byte_measurement_equals_the_original_json_encoding() {
        for value in [
            serde_json::json!(null),
            serde_json::json!({"quoted": "a\"b\\c\n\t", "unicode": "λ啤酒", "numbers": [-7, 0, 123.5], "empty": []}),
            serde_json::json!({"nested": [{"rows": ["a", "b", "c"]}, {"value": true}]}),
        ] {
            let original = serde_json::to_vec(&value).unwrap().len();
            assert_eq!(super::serialized_len(&value).unwrap(), original);
            assert!(super::check_serialized_bound(&value, original).is_ok());
            assert!(super::check_serialized_bound(&value, original + 1).is_ok());
            assert!(
                super::check_serialized_bound(&value, original - 1)
                    .unwrap_err()
                    .to_string()
                    .contains("exceeds byte bound")
            );
        }
    }
}
