//! Shared immutable state receipt validation. Historical readers and rebuilders
//! must establish the same event, actor, timestamp and canonical-value bindings.
use boss_events::replay::ReplayEvent;

pub(crate) fn packet_binding(
    payload: &serde_json::Value,
    job_id: &serde_json::Value,
) -> Result<(), String> {
    if payload.get("job_id").is_some_and(|copied| copied != job_id) {
        return Err("routing packet identity disagrees with its exact original receipt".into());
    }
    Ok(())
}

pub fn first_record(
    ev: &ReplayEvent,
) -> Result<(boss_core::job::Step, crate::first_record::FirstRecord), String> {
    let step: boss_core::job::Step = serde_json::from_value(
        ev.payload
            .get("step")
            .cloned()
            .ok_or("first record lacks step state")?,
    )
    .map_err(|_| "first record step state is invalid".to_string())?;
    let record: crate::first_record::FirstRecord = serde_json::from_value(
        ev.payload
            .get("record")
            .cloned()
            .ok_or("first record lacks receipt")?,
    )
    .map_err(|_| "first record receipt is invalid".to_string())?;
    let value: serde_json::Value = serde_json::from_str(&record.value_json)
        .map_err(|_| "first record authoritative value is invalid".to_string())?;
    use sha2::{Digest, Sha256};
    if record.step_id != step.id
        || record.job_id != step.job_id
        || record.event_id != ev.event_id
        || record.recorded_at.timestamp_micros() != ev.ts.timestamp_micros()
        || serde_json::to_value(&record.actor).map_err(|e| e.to_string())?
            != *ev
                .payload
                .get("_actor")
                .ok_or("first record lacks server actor")?
        || !crate::first_record::valid_key(&record.key)
        || !record.matches(&value)
        || !record.matches(&record.value)
        || record.digest != hex::encode(Sha256::digest(&record.canonical))
        || !step
            .metadata
            .get(&record.key)
            .is_some_and(|v| record.matches(v))
    {
        return Err(
            "first record receipt disagrees with immutable event or full step state".into(),
        );
    }
    Ok((step, record))
}

pub fn station_claim(
    ev: &ReplayEvent,
    receipt_json: &str,
) -> Result<(boss_core::job::Step, uuid::Uuid), String> {
    let parsed = crate::station_claim::SelectionReceipt::from_json(receipt_json.to_owned())
        .map_err(|error| error.to_string())?;
    // Historical events carried a redundant parsed copy. Check
    // it when present; new events conserve only the exact text.
    if ev
        .payload
        .get("receipt")
        .is_some_and(|receipt| parsed.value() != receipt)
    {
        return Err("station selection receipt disagrees with its immutable event JSON".into());
    }
    let receipt = parsed.value();
    packet_binding(&ev.payload, &receipt["selected"]["job_id"])?;
    let request_id: uuid::Uuid = serde_json::from_value(
        receipt
            .get("request_id")
            .cloned()
            .ok_or("station claim lacks request identity")?,
    )
    .map_err(|error| error.to_string())?;
    let step: boss_core::job::Step = serde_json::from_value(
        receipt
            .get("claimed_step")
            .cloned()
            .ok_or("station claim lacks claimed state")?,
    )
    .map_err(|error| error.to_string())?;
    let selected_at: chrono::DateTime<chrono::Utc> = serde_json::from_value(
        receipt
            .get("selected_at")
            .cloned()
            .ok_or("station claim lacks server timestamp")?,
    )
    .map_err(|error| error.to_string())?;
    use sha2::Digest;
    let station = receipt
        .get("station")
        .ok_or("station claim lacks its definition")?;
    let station_hash = hex::encode(sha2::Sha256::digest(boss_core::job::canonical_json_bytes(
        station,
    )));
    if !matches!(
        receipt.get("version").and_then(serde_json::Value::as_u64),
        Some(1 | 2)
    ) || receipt.get("event_id") != Some(&serde_json::json!(ev.event_id))
        || receipt.get("event_actor") != ev.payload.get("_actor")
        || receipt
            .get("station_hash")
            .and_then(serde_json::Value::as_str)
            != Some(station_hash.as_str())
        || selected_at.timestamp_micros() != ev.ts.timestamp_micros()
        || receipt.pointer("/selected/step_id") != Some(&serde_json::json!(step.id))
        || receipt.pointer("/selected/job_id") != Some(&serde_json::json!(step.job_id))
        || step.status != boss_core::job::StepStatus::Active
        || step.assignee_id.as_deref() != receipt.get("actor").and_then(serde_json::Value::as_str)
    {
        return Err(
            "station selection receipt disagrees with its immutable event or owned step".into(),
        );
    }
    Ok((step, request_id))
}
