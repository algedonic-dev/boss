//! Native admission observations are evidence beside a diagnosis, never corrections.
//! Keep this separate: move_diagnosis.rs is itself a frozen v2 fingerprint input.
use crate::move_diagnosis::{Diagnosis, HistoricalEvent, hash};
use boss_core::{event::Event, job::Job, partition::Partition};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const EVENT_KIND: &str = "jobs.yard.move.admission-observed";
pub const MAX_BYTES: usize = 2_000_000;
pub const MAX_ROWS: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub captured_max_seq: i64,
    pub acquired_at: DateTime<Utc>,
    pub run: Vec<HistoricalEvent>,
    pub parent: Vec<HistoricalEvent>,
    pub native_admissions: Vec<HistoricalEvent>,
}
/// Equal cutoff slices are retained once, in the complete copied diagnosis.
/// The reference is internal to this immutable receipt, never a mutable lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CutoffSlices {
    ConservedDiagnosis {
        hash: String,
    },
    Observed {
        run: Vec<HistoricalEvent>,
        parent: Vec<HistoricalEvent>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSnapshot {
    pub captured_max_seq: i64,
    pub acquired_at: DateTime<Utc>,
    pub cutoff_slices: CutoffSlices,
    pub native_admissions: Vec<HistoricalEvent>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Observed,
    AbsentInSnapshot,
    AmbiguousAdmission,
    SliceDisagreement,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Order {
    BeforeCause,
    AtCause,
    AfterCause,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub schema: u32,
    pub id: Uuid,
    pub model: String,
    pub partition: Partition,
    pub diagnosis_hash: String,
    pub diagnosis: Box<Diagnosis>,
    pub snapshot: EvidenceSnapshot,
    pub outcome: Outcome,
    pub order: Option<Order>,
}
pub fn model() -> String {
    let digest = Sha256::digest(include_str!("move_admission.rs").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!("native-admission-observation-v1:{digest}")
}

/// Resolve identities only from the complete immutable original, not a caller's query.
pub fn identities(d: &Diagnosis) -> Result<(Uuid, Uuid), String> {
    let m = &d.original.r#move;
    if d.original.seq < 1
        || m.cause_seq < 1
        || m.kind != "agent-run"
        || m.lineage.as_deref() != Some("packet")
        || d.original_hash != hash(&d.original)?
    {
        return Err("unsupported or mismatched original admission identity".into());
    }
    let run = m
        .packet
        .parse::<Uuid>()
        .map_err(|_| "invalid original run identity")?;
    let parent = m
        .handoff_from
        .as_deref()
        .ok_or("original predecessor missing")?
        .parse::<Uuid>()
        .map_err(|_| "invalid original predecessor identity")?;
    if run == parent || run.is_nil() || parent.is_nil() {
        return Err("ambiguous original identities".into());
    }
    Ok((run, parent))
}

fn rows(
    rows: &[HistoricalEvent],
    packet: &str,
    upto: i64,
    partition: Partition,
) -> Result<(), String> {
    if rows.len() > MAX_ROWS {
        return Err("historical event bound exceeded".into());
    }
    let mut last = 0;
    let mut ids = std::collections::BTreeSet::new();
    for row in rows {
        let e = &row.event;
        if row.seq <= last
            || row.seq > upto
            || row.created_at.is_none()
            || e.id.is_nil()
            || !ids.insert(e.id)
            || e.source != "jobs"
            || e.payload
                .get("job_id")
                .or_else(|| e.payload.get("id"))
                .and_then(|v| v.as_str())
                != Some(packet)
        {
            return Err("native history identity, insertion time or cutoff mismatch".into());
        }
        let marker = e
            .payload
            .get("_partition")
            .or_else(|| e.payload.get("partition"))
            .and_then(|v| v.as_str());
        if marker != Some(partition.as_str()) {
            return Err("native history partition unavailable or mismatched".into());
        }
        if e.payload
            .get("partition")
            .is_some_and(|v| v.as_str() != Some(partition.as_str()))
        {
            return Err("native payload partition disagrees".into());
        }
        last = row.seq;
    }
    Ok(())
}

fn comparison(
    d: &Diagnosis,
    s: &Snapshot,
    partition: Partition,
) -> Result<(Outcome, Option<Order>), String> {
    let (run, parent) = identities(d)?;
    let m = &d.original.r#move;
    if s.captured_max_seq < m.cause_seq || s.native_admissions.len() > 2 {
        return Err("invalid snapshot bound".into());
    }
    rows(&s.run, &run.to_string(), m.cause_seq, partition)?;
    rows(&s.parent, &parent.to_string(), m.cause_seq, partition)?;
    rows(
        &s.native_admissions,
        &run.to_string(),
        s.captured_max_seq,
        partition,
    )?;
    let mut seen_sequences = std::collections::BTreeSet::new();
    let mut seen_ids = std::collections::BTreeSet::new();
    for row in s.run.iter().chain(&s.parent) {
        if !seen_sequences.insert(row.seq) || !seen_ids.insert(row.event.id) {
            return Err("native cutoff slices overlap".into());
        }
    }
    let cause = s
        .run
        .iter()
        .chain(&s.parent)
        .filter(|r| r.seq == m.cause_seq)
        .collect::<Vec<_>>();
    if cause.len() != 1
        || cause[0].event.id != m.cause_event_id
        || cause[0].event.kind != m.cause_kind
        || cause[0].created_at != Some(m.at)
    {
        return Err("exact original cause unavailable or mismatched".into());
    }
    for row in &s.native_admissions {
        let job: Job = serde_json::from_value(row.event.payload.clone())
            .map_err(|e| format!("native admission payload: {e}"))?;
        if row.event.kind != crate::events::JOB_CREATED
            || job.id.to_string() != m.packet
            || job.kind != "agent-run"
            || job.partition != partition
        {
            return Err("native admission source, kind, packet or partition mismatch".into());
        }
        if row.seq <= m.cause_seq && !s.run.iter().any(|old| hash(old).ok() == hash(row).ok()) {
            return Err("native admission disagrees with cutoff slice".into());
        }
        if row.seq > m.cause_seq && seen_ids.contains(&row.event.id) {
            return Err("native admission event identity reused".into());
        }
    }
    if s.native_admissions.len() < 2
        && s.run
            .iter()
            .filter(|r| r.event.kind == crate::events::JOB_CREATED)
            .any(|row| {
                !s.native_admissions
                    .iter()
                    .any(|a| hash(a).ok() == hash(row).ok())
            })
    {
        return Err("native admission selection disagrees with cutoff slice".into());
    }
    if hash(&s.run)? != hash(&d.inputs.run)? || hash(&s.parent)? != hash(&d.inputs.parent)? {
        return Ok((Outcome::SliceDisagreement, None));
    }
    Ok(match s.native_admissions.as_slice() {
        [] => (Outcome::AbsentInSnapshot, None),
        [row] => (
            Outcome::Observed,
            Some(match row.seq.cmp(&m.cause_seq) {
                std::cmp::Ordering::Less => Order::BeforeCause,
                std::cmp::Ordering::Equal => Order::AtCause,
                std::cmp::Ordering::Greater => Order::AfterCause,
            }),
        ),
        _ => (Outcome::AmbiguousAdmission, None),
    })
}
pub fn observe(
    id: Uuid,
    diagnosis: Diagnosis,
    snapshot: Snapshot,
    partition: Partition,
) -> Result<Observation, String> {
    if id.is_nil() || id == diagnosis.id {
        return Err("new observation identity required".into());
    }
    let (outcome, order) = comparison(&diagnosis, &snapshot, partition)?;
    let cutoff_slices = if outcome == Outcome::SliceDisagreement {
        CutoffSlices::Observed {
            run: snapshot.run,
            parent: snapshot.parent,
        }
    } else {
        CutoffSlices::ConservedDiagnosis {
            hash: hash(&(&diagnosis.inputs.run, &diagnosis.inputs.parent))?,
        }
    };
    let snapshot = EvidenceSnapshot {
        captured_max_seq: snapshot.captured_max_seq,
        acquired_at: snapshot.acquired_at,
        cutoff_slices,
        native_admissions: snapshot.native_admissions,
    };
    let receipt = Observation {
        schema: 1,
        id,
        model: model(),
        partition,
        diagnosis_hash: hash(&diagnosis)?,
        diagnosis: Box::new(diagnosis),
        snapshot,
        outcome,
        order,
    };
    validate(&receipt)?;
    Ok(receipt)
}
/// Replay validates copied data, never re-reads today's state or reruns an effect.
pub fn validate(o: &Observation) -> Result<(), String> {
    if o.schema != 1
        || o.id.is_nil()
        || o.id == o.diagnosis.id
        || o.model.is_empty()
        || o.diagnosis_hash != hash(&o.diagnosis)?
    {
        return Err("admission observation envelope mismatch".into());
    }
    if serde_json::to_vec(o).map_err(|e| e.to_string())?.len() > MAX_BYTES {
        return Err("whole admission observation exceeds transport bound".into());
    }
    let (run, parent) = match &o.snapshot.cutoff_slices {
        CutoffSlices::ConservedDiagnosis { hash: expected } => {
            if *expected != hash(&(&o.diagnosis.inputs.run, &o.diagnosis.inputs.parent))? {
                return Err("conserved cutoff slices hash mismatch".into());
            }
            (
                o.diagnosis.inputs.run.clone(),
                o.diagnosis.inputs.parent.clone(),
            )
        }
        CutoffSlices::Observed { run, parent } => (run.clone(), parent.clone()),
    };
    let snapshot = Snapshot {
        captured_max_seq: o.snapshot.captured_max_seq,
        acquired_at: o.snapshot.acquired_at,
        run,
        parent,
        native_admissions: o.snapshot.native_admissions.clone(),
    };
    let (outcome, order) = comparison(&o.diagnosis, &snapshot, o.partition)?;
    if o.outcome != outcome || o.order != order {
        return Err("admission observation does not reproduce".into());
    }
    Ok(())
}
pub fn validate_event(o: &Observation, e: &Event) -> Result<(), String> {
    validate(o)?;
    if e.source != "jobs"
        || e.kind != EVENT_KIND
        || e.payload.get("observation")
            != Some(&serde_json::to_value(o).map_err(|e| e.to_string())?)
        || e.payload.get("job_id").and_then(|v| v.as_str())
            != Some(o.diagnosis.original.r#move.packet.as_str())
        || e.payload
            .get("_actor")
            .and_then(|v| v.as_str())
            .is_none_or(str::is_empty)
        || e.payload.get("_partition").and_then(|v| v.as_str()) != Some(o.partition.as_str())
    {
        return Err("admission observation event provenance mismatch".into());
    }
    Ok(())
}

pub fn validate_binding(
    o: &Observation,
    original: &crate::moves::Recorded,
    entries: &[crate::move_diagnosis::Entry],
) -> Result<(), String> {
    if hash(original)? != o.diagnosis.original_hash
        || hash(&o.diagnosis.original)? != hash(original)?
    {
        return Err("observation original binding mismatch".into());
    }
    let diagnosis = entries
        .iter()
        .find_map(|entry| match entry {
            crate::move_diagnosis::Entry::Diagnosis { receipt } if receipt.id == o.diagnosis.id => {
                Some(receipt)
            }
            _ => None,
        })
        .ok_or("observation diagnosis missing")?;
    if hash(diagnosis)? != o.diagnosis_hash || entries.iter().any(|entry| entry.id() == o.id) {
        return Err("observation diagnosis or request binding mismatch".into());
    }
    Ok(())
}
/// Concurrent repetitions may have acquired different snapshots before the first
/// append wins. The command names only this binding; return the winner unchanged.
pub fn same_request(a: &Observation, b: &Observation) -> bool {
    a.id == b.id
        && a.diagnosis.id == b.diagnosis.id
        && a.diagnosis_hash == b.diagnosis_hash
        && a.diagnosis.original_hash == b.diagnosis.original_hash
        && a.partition == b.partition
}

pub fn reconstruct(events: &[Event]) -> Result<Vec<(Uuid, Observation)>, String> {
    let mut result: std::collections::BTreeMap<Uuid, (Uuid, Observation)> =
        std::collections::BTreeMap::new();
    for event in events.iter().filter(|e| e.kind == EVENT_KIND) {
        let o: Observation = serde_json::from_value(
            event
                .payload
                .get("observation")
                .cloned()
                .ok_or("observation event lacks receipt")?,
        )
        .map_err(|e| e.to_string())?;
        validate_event(&o, event)?;
        if let Some((_, old)) = result.get(&o.id) {
            if hash(old)? != hash(&o)? {
                return Err("observation replay identity conflict".into());
            }
        } else {
            result.insert(o.id, (event.id, o));
        }
    }
    Ok(result.into_values().collect())
}
