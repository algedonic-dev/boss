//! Exact selection artifacts ride bounded immutable facts in their owning transaction.
use crate::{port::JobsError, station_claim::SelectionReceipt};
use boss_core::{event::Event, publisher::EventStamp};
use boss_events::replay::ReplayEvent;
use serde_json::json;
use sha2::{Digest, Sha256};

const CHUNK_BYTES: usize = 128 * 1024;
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn refused(reason: &str) -> JobsError {
    JobsError::Storage(format!("selection evidence: {reason}"))
}

pub fn conserve(
    receipt: &SelectionReceipt,
    claim: Event,
    stamp: &EventStamp,
) -> Result<Vec<Event>, JobsError> {
    conserve_before(receipt, claim, stamp, None)
}
pub(crate) fn conserve_before(
    receipt: &SelectionReceipt,
    mut claim: Event,
    stamp: &EventStamp,
    deadline: Option<tokio::time::Instant>,
) -> Result<Vec<Event>, JobsError> {
    if receipt["version"] != 2
        || claim.kind != crate::events::STATION_CLAIMED
        || claim.source != "jobs"
        || receipt["event_id"] != json!(claim.id)
        || receipt.value().get("event_actor") != claim.payload.get("_actor")
        || claim.timestamp != stamp.timestamp
    {
        return Err(refused("receipt is not bound to its owning claim event"));
    }
    let job_id = receipt
        .value()
        .pointer("/selected/job_id")
        .cloned()
        .ok_or_else(|| refused("receipt lacks selected packet identity"))?;
    let _: boss_core::job::JobId = serde_json::from_value(job_id.clone())
        .map_err(|_| refused("receipt selected packet identity is invalid"))?;
    crate::state_receipt::packet_binding(&claim.payload, &job_id)
        .map_err(|error| refused(&error))?;
    claim.payload["job_id"] = job_id.clone();
    let mut events = Vec::new();
    let mut entries = Vec::new();
    let mut remaining = receipt.json();
    while !remaining.is_empty() {
        crate::selection_serialization::check(deadline)?;
        let mut end = remaining.len().min(CHUNK_BYTES);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        let text = &remaining[..end];
        let chunk = stamp.event(
            crate::events::STATION_SELECTION_EVIDENCE,
            json!({
                "version":2,"claim_event_id":claim.id,"request_id":receipt["request_id"],"job_id":job_id,
                "index":events.len(),"text":text,
            }),
        );
        entries
            .push(json!({"event_id":chunk.id,"bytes":text.len(),"sha256":digest(text.as_bytes())}));
        events.push(chunk);
        remaining = &remaining[end..];
    }
    claim
        .payload
        .as_object_mut()
        .ok_or_else(|| refused("claim payload is not an object"))?
        .remove("receipt_json");
    claim.payload["receipt_manifest"] = json!({"version":2,"request_id":receipt["request_id"],
        "bytes":receipt.json().len(),"sha256":crate::selection_serialization::hash(receipt.json().as_bytes(),deadline)?,"chunks":entries});
    events.push(claim);
    for event in &events {
        crate::selection_serialization::check(deadline)?;
        let size = serde_json::to_vec(event)
            .map_err(|e| refused(&e.to_string()))?
            .len();
        if size > boss_events::outbox::MAX_STAGED_EVENT_BYTES {
            return Err(refused("bounded fact exceeds outbox limit"));
        }
    }
    Ok(events)
}

/// Validate the complete immutable batch before any projection wipe. Current rows are never read.
pub fn reconstruct(events: &[ReplayEvent]) -> Result<Vec<ReplayEvent>, JobsError> {
    let mut pending = std::collections::HashMap::<uuid::Uuid, Vec<&ReplayEvent>>::new();
    let mut planned = Vec::new();
    let mut identities = std::collections::HashSet::new();
    for event in events {
        if event.kind == crate::events::STATION_SELECTION_EVIDENCE {
            if !identities.insert(event.event_id) {
                return Err(refused("duplicate chunk identity"));
            }
            let claim = serde_json::from_value(event.payload["claim_event_id"].clone())
                .map_err(|_| refused("chunk lacks claim identity"))?;
            pending.entry(claim).or_default().push(event);
            continue;
        }
        let mut copied = event.clone();
        if event.kind == crate::events::STATION_CLAIMED {
            let chunks = pending.remove(&event.event_id).unwrap_or_default();
            if let Some(manifest) = event.payload.get("receipt_manifest") {
                if event.payload.get("receipt_json").is_some() || manifest["version"] != 2 {
                    return Err(refused("ambiguous or unsupported manifest"));
                }
                let entries = manifest["chunks"]
                    .as_array()
                    .ok_or_else(|| refused("manifest lacks chunk list"))?;
                if entries.is_empty() || entries.len() != chunks.len() {
                    return Err(refused("missing or extra chunk"));
                }
                let mut text = String::new();
                for (index, (entry, chunk)) in entries.iter().zip(chunks.iter()).enumerate() {
                    let body = chunk.payload["text"]
                        .as_str()
                        .ok_or_else(|| refused("chunk lacks exact text"))?;
                    if chunk.payload["version"] != 2
                        || chunk.payload["index"] != index
                        || chunk.payload["request_id"] != manifest["request_id"]
                        || chunk.payload.get("_actor") != event.payload.get("_actor")
                        || chunk.ts != event.ts
                        || entry["event_id"] != json!(chunk.event_id)
                        || entry["bytes"] != body.len()
                        || entry["sha256"] != digest(body.as_bytes())
                        || body.len() > CHUNK_BYTES
                    {
                        return Err(refused("chunk binding, order, length or hash differs"));
                    }
                    text.push_str(body);
                }
                if manifest["bytes"] != text.len() || manifest["sha256"] != digest(text.as_bytes())
                {
                    return Err(refused("full receipt length or hash differs"));
                }
                let receipt = SelectionReceipt::from_json(text.clone())?;
                let selected = &receipt["selected"]["job_id"];
                crate::state_receipt::packet_binding(&event.payload, selected)
                    .map_err(|error| refused(&error))?;
                for chunk in chunks {
                    crate::state_receipt::packet_binding(&chunk.payload, selected)
                        .map_err(|error| refused(&error))?;
                }
                if receipt["version"] != 2
                    || receipt["request_id"] != manifest["request_id"]
                    || receipt["event_id"] != json!(event.event_id)
                {
                    return Err(refused("receipt identity or version differs"));
                }
                copied.payload["receipt_json"] = json!(text);
            } else if !chunks.is_empty() {
                return Err(refused("chunks have no final manifest"));
            }
        }
        planned.push(copied);
    }
    if !pending.is_empty() {
        return Err(refused("orphan chunks have no committed claim"));
    }
    Ok(planned)
}
