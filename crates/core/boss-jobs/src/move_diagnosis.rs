//! Closed-input diagnosis of one historical registered agent handoff.
//! Other historical placement errors are deliberately unsupported.
use crate::{
    moves::{DispatchContext, Recorded},
    registry::WorkflowSpec,
    stations::StationSpec,
};
use boss_core::{
    event::Event,
    job::{Job, Step},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub mod proof;
pub(crate) mod stations;

pub const EVENT_KIND: &str = "jobs.yard.move.diagnosed";
pub const MAX_RECEIPT_BYTES: usize = 512_000;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoricalEvent {
    pub seq: i64,
    /// Original audit insertion instant. None is unknown; preserving omission
    /// keeps copied legacy receipts unchanged instead of backfilling a field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub event: Event,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inputs {
    pub run: Vec<HistoricalEvent>,
    pub parent: Vec<HistoricalEvent>,
    pub workflows: Vec<WorkflowSpec>,
    pub stations: Vec<StationSpec>,
    pub station_history: Vec<HistoricalEvent>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Eligible,
    Genuine,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnosis {
    pub id: uuid::Uuid,
    pub original: Recorded,
    pub original_hash: String,
    pub model: String,
    pub inputs: Inputs,
    pub status: Status,
    pub why: String,
    pub dispatch: Option<DispatchContext>,
}
pub fn hash<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_value(value)
        .map(|value| {
            Sha256::digest(boss_core::job::canonical_json_bytes(&value))
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        })
        .map_err(|e| e.to_string())
}
pub fn model() -> String {
    let mut h = Sha256::new();
    for part in [
        include_str!("move_diagnosis.rs"),
        include_str!("moves/dispatch.rs"),
        include_str!("routes.rs"),
        include_str!("agent_spec.rs"),
        include_str!("move_diagnosis/stations.rs"),
        include_str!("move_diagnosis/proof.rs"),
        crate::routes::HANDOFFS_TOML,
    ] {
        h.update(part.as_bytes());
    }
    format!(
        "registered-agent-handoff-v2:{}",
        h.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}
/// Fold ONLY full immutable state events. The admission and every step's
/// creation must be present; an incomplete slice never becomes authority.
pub fn state(
    rows: &[HistoricalEvent],
    packet: &str,
    upto: i64,
) -> Result<(Job, Vec<Step>), String> {
    let mut job = None;
    let mut steps = std::collections::BTreeMap::new();
    let mut created = false;
    let mut last = 0;
    let mut ids = std::collections::BTreeSet::new();
    for row in rows {
        if row.seq <= last || row.seq > upto || !ids.insert(row.event.id) {
            return Err("history order, cutoff or identity mismatch".into());
        }
        last = row.seq;
        let e = &row.event;
        if e.source != "jobs" {
            return Err("history source mismatch".into());
        }
        let names = e
            .payload
            .get("job_id")
            .or_else(|| e.payload.get("id"))
            .and_then(|v| v.as_str());
        if names != Some(packet) {
            return Err("history packet mismatch".into());
        }
        match e.kind.as_str() {
            crate::events::JOB_CREATED | crate::events::JOB_UPDATED => {
                let value: Job =
                    serde_json::from_value(e.payload.clone()).map_err(|e| e.to_string())?;
                if e.kind == crate::events::JOB_CREATED {
                    if created {
                        return Err("duplicate admission".into());
                    }
                    created = true;
                }
                if !created {
                    return Err("admission missing".into());
                }
                job = Some(value);
            }
            crate::events::STEP_CREATED | crate::events::STEP_UPDATED => {
                let value: Step =
                    serde_json::from_value(e.payload.clone()).map_err(|e| e.to_string())?;
                if e.kind == crate::events::STEP_UPDATED
                    && !steps.contains_key(&value.id.to_string())
                {
                    return Err("step creation missing".into());
                }
                if e.kind == crate::events::STEP_CREATED
                    && steps.contains_key(&value.id.to_string())
                {
                    return Err("duplicate step creation".into());
                }
                steps.insert(value.id.to_string(), value);
            }
            crate::events::STEP_FIRST_RECORDED | crate::events::STATION_CLAIMED => {
                // These doors may carry the only full post-write state. Until
                // their full/chunked receipt has been conserved, do not reuse stale state.
                return Err("history contains an unsupported full-state receipt".into());
            }
            _ => {} // informational markers do not replace a full state event
        }
    }
    let job = job.ok_or("admission missing")?;
    if steps.is_empty() {
        return Err("step state missing".into());
    }
    Ok((job, steps.into_values().collect()))
}
pub fn diagnose(id: uuid::Uuid, original: Recorded, inputs: Inputs) -> Result<Diagnosis, String> {
    let original_hash = hash(&original)?;
    let mut out = Diagnosis {
        id,
        original,
        original_hash,
        model: model(),
        inputs,
        status: Status::Unknown,
        why: "native registration proof unavailable".into(),
        dispatch: None,
    };
    let m = &out.original.r#move;
    let mut protocols = std::collections::BTreeSet::new();
    let mut stations = std::collections::BTreeSet::new();
    if out
        .inputs
        .workflows
        .iter()
        .any(|w| !protocols.insert((&w.kind, w.version)))
        || out
            .inputs
            .stations
            .iter()
            .any(|s| !stations.insert(&s.name))
    {
        out.why = "duplicate or ambiguous registry inputs".into();
        return Ok(out);
    }
    if out
        .inputs
        .workflows
        .iter()
        .any(|w| w.created_at > m.at || w.status == crate::registry::WorkflowStatus::Draft)
    {
        out.why = "pinned protocol was not available at the original cause".into();
        return Ok(out);
    }
    if serde_json::to_vec(&out.inputs)
        .map_err(|e| e.to_string())?
        .len()
        > 2_000_000
        || out.inputs.run.len() > 4096
        || out.inputs.parent.len() > 4096
    {
        out.why = "native input bound exceeded".into();
        return Ok(out);
    }
    if m.kind != "agent-run"
        || m.lineage.as_deref() != Some("packet")
        || m.dispatch.is_some()
        || m.terminal.is_some()
        || !m.aboard.is_empty()
    {
        out.why = "unsupported historical interpretation class".into();
        return Ok(out);
    }
    let Some(parent_id) = m.handoff_from.as_deref() else {
        return Ok(out);
    };
    // The cause must be an exact row in one conserved packet slice.
    let cause = out.inputs.run.iter().chain(&out.inputs.parent).find(|e| {
        e.seq == m.cause_seq
            && e.event.id == m.cause_event_id
            && e.event.kind == m.cause_kind
            // Moves use the native audit insertion clock, not the producer clock.
            && e.created_at == Some(m.at)
    });
    if cause.is_none() {
        out.why = "original cause absent from bounded native history".into();
        return Ok(out);
    }
    let (run, run_steps) = match state(&out.inputs.run, &m.packet, m.cause_seq) {
        Ok(v) => v,
        Err(e) => {
            out.why = e;
            return Ok(out);
        }
    };
    let (parent, parent_steps) = match state(&out.inputs.parent, parent_id, m.cause_seq) {
        Ok(v) => v,
        Err(e) => {
            out.why = e;
            return Ok(out);
        }
    };
    if parent.kind != crate::regions::CAR_KIND {
        out.why = "this model does not prove other families' historical station snapshots".into();
        return Ok(out);
    }
    match stations::at(&out.inputs.station_history, m.cause_seq) {
        Ok(original_stations) if hash(&original_stations)? == hash(&out.inputs.stations)? => {}
        _ => {
            out.why = "original station publication proof unavailable or disagrees".into();
            return Ok(out);
        }
    }
    let spec = out
        .inputs
        .workflows
        .iter()
        .find(|w| w.kind == parent.kind && w.version == parent.workflow_version);
    let context = crate::moves::dispatch_context(&run, &run_steps, &parent, &parent_steps, spec);
    let routes =
        crate::routes::derive_from(&out.inputs.workflows, &out.inputs.stations, None, m.at);
    let judgment = routes.judges_context(
        m.from.as_deref(),
        m.to.as_deref(),
        Some(&m.kind),
        None,
        m.lineage.as_deref(),
        context.as_ref(),
    );
    out.dispatch = context;
    match judgment {
        Some(true) => {
            out.status = Status::Eligible;
            out.why =
                "exact native registration and pinned route prove the missing historical context"
                    .into();
        }
        Some(false) => {
            out.status = Status::Genuine;
            out.why = "native registration does not authorize this crossing".into();
        }
        None => {}
    }
    Ok(out)
}
/// A stored receipt is usable only when its entire copied-input computation
/// reproduces. A model/version string alone grants nothing.
pub fn validated(receipt: &Diagnosis) -> bool {
    receipt.status == Status::Eligible
        && diagnose(receipt.id, receipt.original.clone(), receipt.inputs.clone())
            .ok()
            .is_some_and(|again| {
                hash(&again)
                    .ok()
                    .zip(hash(receipt).ok())
                    .is_some_and(|(a, b)| a == b)
            })
}

pub const CORRECTED_KIND: &str = "jobs.yard.move.corrected";
pub const WITHDRAWN_KIND: &str = "jobs.yard.move.withdrawn";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Entry {
    Diagnosis {
        receipt: Box<Diagnosis>,
    },
    Correction {
        id: uuid::Uuid,
        original_seq: i64,
        diagnosis_id: uuid::Uuid,
        original_hash: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proof: Option<Box<proof::MachineProof>>,
    },
    Withdrawal {
        id: uuid::Uuid,
        original_seq: i64,
        correction_id: uuid::Uuid,
    },
}
impl Entry {
    pub fn id(&self) -> uuid::Uuid {
        match self {
            Self::Diagnosis { receipt } => receipt.id,
            Self::Correction { id, .. } | Self::Withdrawal { id, .. } => *id,
        }
    }
    pub fn original_seq(&self) -> i64 {
        match self {
            Self::Diagnosis { receipt } => receipt.original.seq,
            Self::Correction { original_seq, .. } | Self::Withdrawal { original_seq, .. } => {
                *original_seq
            }
        }
    }
    pub fn event_kind(&self) -> &'static str {
        match self {
            Self::Diagnosis { .. } => EVENT_KIND,
            Self::Correction { .. } => CORRECTED_KIND,
            Self::Withdrawal { .. } => WITHDRAWN_KIND,
        }
    }
}
/// Whole-ledger validation: disagreement is an error, not last-writer-wins.
pub fn append(entries: &[Entry], entry: &Entry, original: &Recorded) -> Result<bool, String> {
    if serde_json::to_vec(entry).map_err(|e| e.to_string())?.len() > MAX_RECEIPT_BYTES {
        return Err("whole correction receipt exceeds its transport bound".into());
    }
    if let Some(old) = entries.iter().find(|e| e.id() == entry.id()) {
        return if hash(old)? == hash(entry)? {
            Ok(false)
        } else {
            Err("request identity already names different inputs".into())
        };
    }
    if entry.original_seq() != original.seq {
        return Err("entry names another original".into());
    }
    match entry {
        Entry::Diagnosis{receipt} => {
            if hash(&receipt.original)?!=hash(original)? || hash(&receipt.original)?!=receipt.original_hash {return Err("original identity mismatch".into());}
            // Unknown and genuine diagnoses are stored too, but cannot clear anything.
            let replay=diagnose(receipt.id,original.clone(),receipt.inputs.clone())?;
            if hash(&replay)?!=hash(receipt.as_ref())? {return Err("diagnosis does not reproduce".into());}
        }
        Entry::Correction{diagnosis_id,original_hash,proof,..} => {
            let receipt=entries.iter().find_map(|e|match e {Entry::Diagnosis{receipt} if receipt.id==*diagnosis_id=>Some(receipt),_=>None}).ok_or("diagnosis missing")?;
            if !validated(receipt) || *original_hash!=hash(original)? || receipt.original_hash!=*original_hash {return Err("diagnosis cannot correct this original".into());}
            proof::validate(proof.as_deref().ok_or("independent machine proof missing")?, receipt)?;
            if active(entries,original)?.is_some() {return Err("original already has an active correction".into());}
        }
        Entry::Withdrawal{correction_id,..} => {
            if !entries.iter().any(|e|matches!(e,Entry::Correction{id,original_seq,..} if id==correction_id && *original_seq==original.seq)) {return Err("correction missing or names another original".into());}
        }
    }
    Ok(true)
}
pub fn active(entries: &[Entry], original: &Recorded) -> Result<Option<uuid::Uuid>, String> {
    let mut candidates = Vec::new();
    for e in entries {
        if let Entry::Correction {
            id,
            original_seq,
            diagnosis_id,
            original_hash,
            proof,
        } = e
        {
            if *original_seq != original.seq {
                continue;
            }
            if entries
                .iter()
                .any(|w| matches!(w,Entry::Withdrawal{correction_id,..} if correction_id==id))
            {
                continue;
            }
            let receipt = entries
                .iter()
                .find_map(|d| match d {
                    Entry::Diagnosis { receipt } if receipt.id == *diagnosis_id => Some(receipt),
                    _ => None,
                })
                .ok_or("correction diagnosis missing")?;
            if receipt.original_hash != *original_hash
                || *original_hash != hash(original)?
                || !validated(receipt)
                || proof
                    .as_deref()
                    .is_none_or(|p| proof::validate(p, receipt).is_err())
            {
                return Err("correction proof unavailable or disagrees".into());
            }
            candidates.push(*id);
        }
    }
    if candidates.len() > 1 {
        return Err("conflicting active corrections".into());
    }
    Ok(candidates.pop())
}

/// Replay copies the immutable receipts, including historical models that this
/// executable cannot run. Those remain retained but cannot clear a current finding.
pub fn reconstruct(events: &[Event]) -> Result<Vec<(uuid::Uuid, Entry)>, String> {
    let mut out: Vec<(uuid::Uuid, Entry)> = Vec::new();
    let mut ordered: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e.kind.as_str(),
                EVENT_KIND | CORRECTED_KIND | WITHDRAWN_KIND
            )
        })
        .map(|e| {
            e.payload
                .get("ledger_seq")
                .and_then(|v| v.as_i64())
                .filter(|seq| *seq > 0)
                .map(|seq| (seq, e))
                .ok_or("ledger sequence missing")
        })
        .collect::<Result<_, _>>()?;
    ordered.sort_by_key(|(seq, _)| *seq);
    for (index, (seq, event)) in ordered.into_iter().enumerate() {
        if seq != index as i64 + 1 {
            return Err("ledger sequence gap or duplicate".into());
        }
        let entry: Entry = serde_json::from_value(
            event
                .payload
                .get("entry")
                .cloned()
                .ok_or("ledger event lacks entry")?,
        )
        .map_err(|e| e.to_string())?;
        if entry.event_kind() != event.kind {
            return Err("ledger event kind mismatch".into());
        }
        if let Some((_, old)) = out.iter().find(|(_, old)| old.id() == entry.id()) {
            if hash(old)? != hash(&entry)? {
                return Err("ledger replay request identity conflict".into());
            }
            continue;
        }
        match &entry {
            Entry::Diagnosis{receipt}=> {
                if receipt.original.seq<1 || receipt.original_hash!=hash(&receipt.original)? {return Err("ledger original identity mismatch".into());}
                if event.payload.get("job_id").and_then(|v|v.as_str())!=Some(receipt.original.r#move.packet.as_str()) {return Err("ledger event names another packet".into());}
            }
            Entry::Correction{original_seq,diagnosis_id,original_hash,..}=> {
                if !out.iter().any(|(_,d)|matches!(d,Entry::Diagnosis{receipt} if receipt.id==*diagnosis_id && receipt.original.seq==*original_seq && receipt.original_hash==*original_hash && receipt.status==Status::Eligible)){return Err("ledger correction lacks exact eligible diagnosis".into());}
            }
            Entry::Withdrawal{original_seq,correction_id,..}=> {
                if !out.iter().any(|(_,d)|matches!(d,Entry::Correction{id,original_seq:s,..} if id==correction_id && s==original_seq)){return Err("ledger withdrawal lacks original correction".into());}
            }
        }
        out.push((event.id, entry));
    }
    Ok(out)
}

/// The append order is minted under the adapter's writer lock and rides in
/// the immutable event, so parallel relays cannot change the ledger's history.
pub fn conserved_event(
    entry: &Entry,
    event: &Event,
    original: &Recorded,
    seq: i64,
) -> Result<Event, String> {
    if event.source != "jobs"
        || event.kind != entry.event_kind()
        || event.payload.get("entry")
            != Some(&serde_json::to_value(entry).map_err(|e| e.to_string())?)
        || event.payload.get("job_id").and_then(|v| v.as_str())
            != Some(original.r#move.packet.as_str())
        || event
            .payload
            .get("_actor")
            .and_then(|v| v.as_str())
            .is_none_or(str::is_empty)
    {
        return Err("correction event provenance mismatch".into());
    }
    let mut recorded = event.clone();
    recorded.payload["ledger_seq"] = serde_json::json!(seq);
    Ok(recorded)
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Conservation {
    pub raw: i64,
    pub active: i64,
    pub superseded: i64,
    pub unknown: i64,
}
impl Conservation {
    pub fn observe(&mut self, entries: &[Entry], original: &Recorded) {
        self.raw += 1;
        match active(entries, original) {
            Ok(Some(_)) => self.superseded += 1,
            Ok(None) => self.active += 1,
            Err(_) => self.unknown += 1,
        }
    }
}
