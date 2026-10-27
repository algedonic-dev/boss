//! Original Q3 census facts, conserved before pruning (approved2eaa).
//! These are adapter evidence, not an exclusion verdict or a replacement
//! for original registry/clock transaction validation.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrunedPacket {
    pub id: String,
    pub job_seq: Option<i64>,
    pub last_seq: Option<i64>,
    pub last_at: Option<DateTime<Utc>>,
    pub status: Option<String>,
    pub closed_on: Option<String>,
    pub kind: Option<String>,
    pub tags: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrunedCensus {
    pub schema: u32,
    pub cut: i64,
    pub audit_cutoff: i64,
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub binding: projection::ProjectionBinding,
    pub metadata_keys: Vec<String>,
    pub packets: Vec<PrunedPacket>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exclusion {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub source_cut: i64,
    pub station_version: i32,
    pub publication_sequences: Vec<i64>,
    pub packets: u64,
}

pub(crate) fn registry_kind(kind: &str) -> bool {
    matches!(
        kind,
        crate::events::STATION_PUBLISHED
            | crate::events::STATION_RETIRED
            | crate::events::WORKFLOW_PUBLISHED
            | crate::events::WORKFLOW_RETIRED
    )
}

/// Returns a separate certificate only for complete original Job evidence
/// and a constant *absent* outcome over the exact half-open partition.
/// The scalar remains a refusal when any ingredient is missing.
pub(super) fn exclusion(
    input: &ReplayInput,
    spec: &StationSpec,
    basis: &VersionBasis,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    budget: Option<&ReplayBudget>,
) -> Option<Exclusion> {
    checkpoint(budget).ok()?;
    let witness = input.initial.pruned_census.as_ref()?;
    if witness.schema != 1
        || witness.cut != input.initial.cut
        || witness.audit_cutoff != input.audit_cutoff
        || witness.since != input.since
        || witness.until != input.until
        || witness.binding != projection::ProjectionBinding::from_input(input)
        || witness.packets.len() as u64 != input.initial.pruned_terminal
        || !input.registry_prefix_complete
        || !input.packet_prefix_complete
        || input
            .source_unavailable
            .iter()
            .any(|g| g.since < until && since < g.until)
        || since >= until
        || since < input.since
        || until > input.until
        || basis.source != "authored-publication"
        || basis.version != spec.version
        || basis.publication_sequences.len() != 1
    {
        return None;
    }
    // Registry originals already belong to this same-cut replay input.
    // Validate and borrow them, rather than retain a second copy merely
    // to compare that copy to its source.
    let mut publication = None;
    for source in &input.groups {
        for row in source.rows.iter().filter(|r| registry_kind(&r.event.kind)) {
            checkpoint(budget).ok()?;
            let group = source.original.as_ref()?;
            if !group.validate(input.audit_cutoff).ok()? {
                return None;
            }
            let index = group.event_ids.iter().position(|id| *id == row.event.id)?;
            if group.audit_sequences.get(index).copied().flatten() != Some(row.seq) {
                return None;
            }
            if row.seq == basis.publication_sequences[0]
                && row.event.kind == crate::events::STATION_PUBLISHED
            {
                publication = Some(row);
            }
        }
    }
    let publication = publication?;
    // This version must already govern the pre-edge instant. A newly
    // activated version cannot certify the prior version's transition.
    let before = since.checked_sub_signed(chrono::Duration::microseconds(1))?;
    if publication.created_at? > before {
        return None;
    }
    let original_spec = StationSpec::deserialize(&publication.event.payload).ok()?;
    if original_spec != *spec {
        return None;
    }
    let bound = spec.bind_self(input.actor.as_deref())?;
    if bound.predicate.needs_steps()
        || bound
            .predicate
            .metadata_keys()
            .iter()
            .any(|key| !witness.metadata_keys.contains(key))
    {
        return None;
    }
    let days = bound.terminal_window_days?;
    // Expiry counts the inclusive start's transition, not just point
    // membership. A missing pre-edge original model remains UNKNOWN.
    let (first, last) = input
        .clock
        .outcome_date_bounds(before, until, input.audit_cutoff)
        .ok()?;
    let touched: BTreeSet<_> = input
        .groups
        .iter()
        .flat_map(|g| &g.rows)
        .filter_map(packet_identity)
        .collect();
    let mut seen = BTreeSet::new();
    for packet in &witness.packets {
        checkpoint(budget).ok()?;
        let job_seq = packet.job_seq?;
        let last_seq = packet.last_seq?;
        if !seen.insert(&packet.id)
            || !authorized_packet(input, &packet.id)
            || job_seq <= 0
            || job_seq > last_seq
            || last_seq > witness.cut
            || witness.cut > witness.audit_cutoff
            || packet.last_at? >= witness.since
            || touched.contains(&packet.id)
        {
            return None;
        }
        let status: JobStatus =
            serde_json::from_value(serde_json::Value::String(packet.status.clone()?)).ok()?;
        if !matches!(status, JobStatus::Closed | JobStatus::Cancelled) {
            return None;
        }
        let kind = packet.kind.as_deref()?;
        let tags: Vec<String> = serde_json::from_value(packet.tags.clone()?).ok()?;
        let metadata = packet.metadata.as_ref()?;
        if !metadata.is_object() {
            return None;
        }
        if !bound
            .predicate
            .job_fields_match(kind, &status, &tags, metadata)
        {
            continue;
        }
        let closed: chrono::NaiveDate = packet.closed_on.as_ref()?.parse().ok()?;
        // This is the shared retention predicate's exact inequality,
        // without constructing a synthetic Job or inventing other fields.
        if (first - closed).num_days() <= i64::from(days)
            || (last - closed).num_days() <= i64::from(days)
        {
            return None;
        }
    }
    Some(Exclusion {
        since,
        until,
        source_cut: witness.cut,
        station_version: spec.version,
        publication_sequences: basis.publication_sequences.clone(),
        packets: input.initial.pruned_terminal,
    })
}
