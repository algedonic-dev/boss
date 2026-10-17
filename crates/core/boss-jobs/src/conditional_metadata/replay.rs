//! A derived index of the existing receipt/state pairs, never durable truth.
use super::{EVENT, RECEIPT_LINK, Receipt};
use boss_core::job::Job;
use boss_events::replay::ReplayEvent;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct Pairs {
    states: BTreeMap<Uuid, Receipt>,
    markers: BTreeMap<Uuid, Uuid>,
}

impl Pairs {
    pub fn read(events: &[ReplayEvent]) -> Result<Self, String> {
        let mut index = BTreeMap::new();
        for event in events {
            if index.insert(event.event_id, event).is_some() {
                return Err("duplicate immutable event identity".into());
            }
        }
        let mut pairs = Self::default();
        let mut commands = BTreeSet::new();
        for marker in events.iter().filter(|event| event.kind == EVENT) {
            let receipt: Receipt = serde_json::from_value(marker.payload.clone())
                .map_err(|error| error.to_string())?;
            receipt.command.validate().map_err(str::to_owned)?;
            if receipt.contract_version != receipt.command.contract_version
                || receipt.job_id != receipt.job.id
                || receipt.event_id.is_nil()
                || receipt.applied_at.timestamp_micros() != marker.ts.timestamp_micros()
                || marker
                    .payload
                    .get("_actor")
                    .and_then(serde_json::Value::as_str)
                    != Some(receipt.actor.as_str())
            {
                return Err(
                    "conditional receipt disagrees with its event, command, actor or packet".into(),
                );
            }
            if !commands.insert((
                receipt.job_id.to_string(),
                receipt.actor.clone(),
                receipt.command.idempotency_key,
            )) {
                return Err("duplicate original conditional command receipt".into());
            }
            let state = index
                .get(&receipt.event_id)
                .ok_or("conditional receipt lacks its state sibling")?;
            let job: Job =
                serde_json::from_value(state.payload.clone()).map_err(|error| error.to_string())?;
            if state.kind != crate::events::JOB_UPDATED
                || state.audit_id >= marker.audit_id
                || state.ts.timestamp_micros() != receipt.applied_at.timestamp_micros()
                || state
                    .payload
                    .get("_actor")
                    .and_then(serde_json::Value::as_str)
                    != Some(receipt.actor.as_str())
                || job != receipt.job
            {
                return Err(
                    "conditional receipt disagrees with its ordered complete state sibling".into(),
                );
            }
            match receipt.contract_version {
                1 if receipt.receipt_event_id.is_none()
                    && state.payload.get(RECEIPT_LINK).is_none() => {}
                2 if receipt.receipt_event_id == Some(marker.event_id)
                    && state
                        .payload
                        .get(RECEIPT_LINK)
                        .and_then(serde_json::Value::as_str)
                        .and_then(|id| Uuid::parse_str(id).ok())
                        == Some(marker.event_id) => {}
                _ => {
                    return Err(
                        "conditional pair has missing, unknown or nonreciprocal versioned links"
                            .into(),
                    );
                }
            }
            for between in events.iter().filter(|event| {
                state.audit_id < event.audit_id
                    && event.audit_id < marker.audit_id
                    && matches!(event.kind.as_str(), "jobs.job.created" | "jobs.job.updated")
            }) {
                if between.payload.get("id") == state.payload.get("id") {
                    return Err(
                        "another state for the same packet splits a conditional pair".into(),
                    );
                }
            }
            if pairs.states.insert(state.event_id, receipt).is_some() {
                return Err("one state has multiple conditional receipts".into());
            }
            pairs.markers.insert(marker.event_id, state.event_id);
        }
        for state in events
            .iter()
            .filter(|event| event.payload.get(RECEIPT_LINK).is_some())
        {
            if state.kind != crate::events::JOB_UPDATED
                || !pairs.states.contains_key(&state.event_id)
            {
                return Err("linked conditional state lacks its unique receipt".into());
            }
        }
        Ok(pairs)
    }

    pub fn state(&self, event_id: Uuid) -> Option<&Receipt> {
        self.states.get(&event_id)
    }
    pub fn marker(&self, event_id: Uuid) -> Option<&Receipt> {
        self.markers
            .get(&event_id)
            .and_then(|id| self.states.get(id))
    }

    pub fn next(
        &self,
        event_id: Uuid,
        before: &Job,
        schema: Option<&serde_json::Value>,
    ) -> Result<Option<Job>, String> {
        let Some(receipt) = self.state(event_id) else {
            return Ok(None);
        };
        if !receipt.command.matches(before) {
            return Err("conditional replay predicate fails against preceding state".into());
        }
        let next = receipt
            .command
            .apply(before)
            .map_err(|error| error.to_string())?;
        if !receipt.command.guarded_keys.is_empty() || !receipt.command.immutable_keys.is_empty() {
            crate::metadata_immutability::validate_protocol(
                schema.ok_or("historical pinned guard declaration unavailable")?,
                &before.metadata,
                &next.metadata,
                Some(&receipt.command),
            )?;
        }
        if next != receipt.job {
            return Err("conditional siblings do not equal applying their command to the preceding complete state".into());
        }
        Ok(Some(next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::{
        actor::ActorId,
        job::{Priority, Subject},
        publisher::EventStamp,
    };
    use serde_json::json;

    fn fixture() -> (Job, Vec<ReplayEvent>) {
        let mut before = Job::new(
            "fixture",
            Subject::new("custom", "fixture"),
            "Pair",
            "agent-test",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
        );
        before.status = boss_core::job::JobStatus::Open;
        before.metadata = json!({"value":"before"});
        let command = serde_json::from_value(
            json!({"contract_version":2,"idempotency_key":Uuid::new_v4(),
            "expected":{"value":{"present":true,"value":"before"}},"patch":{"value":"after"}}),
        )
        .unwrap();
        let next = super::super::Command::apply(&command, &before).unwrap();
        let stamp = EventStamp::new("jobs", ActorId::Automation("fixture".into()));
        let (_, events) = super::super::recorded_pair(&stamp, &command, next).unwrap();
        let events = events
            .into_iter()
            .enumerate()
            .map(|(index, event)| ReplayEvent {
                audit_id: index as i64 + 1,
                event_id: event.id,
                kind: event.kind,
                ts: event.timestamp,
                payload: event.payload,
            })
            .collect();
        (before, events)
    }

    fn valid(before: &Job, events: &[ReplayEvent]) -> bool {
        match Pairs::read(events).and_then(|pairs| pairs.next(events[0].event_id, before, None)) {
            Ok(_) => true,
            Err(error) => {
                eprintln!("pair validation: {error}");
                false
            }
        }
    }

    #[test]
    fn version2_pairs_require_both_links_order_and_complete_causal_state() {
        let (before, events) = fixture();
        assert!(valid(&before, &events));
        for mutation in [
            "missing-state",
            "missing-receipt",
            "state-link",
            "receipt-link",
            "state-actor",
            "receipt-actor",
            "timestamp",
            "kind",
            "packet",
            "post-state",
            "predicate",
            "patch",
            "version",
            "reverse",
            "duplicate-state",
            "duplicate-receipt",
            "split-pair",
            "matching-forged",
        ] {
            let mut altered = events.clone();
            match mutation {
                "missing-state" => {
                    altered.remove(0);
                }
                "missing-receipt" => {
                    altered.pop();
                }
                "state-link" => altered[0].payload[RECEIPT_LINK] = json!(Uuid::new_v4()),
                "receipt-link" => altered[1].payload["event_id"] = json!(Uuid::new_v4()),
                "state-actor" => altered[0].payload["_actor"] = json!("forged"),
                "receipt-actor" => altered[1].payload["_actor"] = json!("forged"),
                "timestamp" => altered[0].ts += chrono::Duration::seconds(1),
                "kind" => altered[0].kind = "jobs.job.created".into(),
                "packet" => altered[1].payload["job_id"] = json!(Uuid::new_v4()),
                "post-state" => altered[0].payload["metadata"]["value"] = json!("forged"),
                "predicate" => {
                    altered[1].payload["command"]["expected"]["value"]["value"] =
                        json!("wrong-before")
                }
                "patch" => altered[1].payload["command"]["patch"]["value"] = json!("wrong-after"),
                "version" => altered[1].payload["contract_version"] = json!(99),
                "reverse" => {
                    altered[0].audit_id = 3;
                }
                "duplicate-state" => altered.push(altered[0].clone()),
                "duplicate-receipt" => altered.push(altered[1].clone()),
                "split-pair" => {
                    let mut between = altered[0].clone();
                    between.event_id = Uuid::new_v4();
                    between.audit_id = 2;
                    between
                        .payload
                        .as_object_mut()
                        .unwrap()
                        .remove(RECEIPT_LINK);
                    altered[1].audit_id = 3;
                    altered.insert(1, between);
                }
                "matching-forged" => {
                    altered[0].payload["metadata"]["value"] = json!("forged");
                    altered[1].payload["job"]["metadata"]["value"] = json!("forged");
                }
                _ => unreachable!(),
            }
            assert!(!valid(&before, &altered), "{mutation} must refuse");
        }
    }

    #[test]
    fn unrelated_jobs_may_interleave_a_valid_pair() {
        let (before, mut events) = fixture();
        let (_, other) = fixture();
        let mut unrelated = other[0].clone();
        unrelated.audit_id = 2;
        unrelated
            .payload
            .as_object_mut()
            .unwrap()
            .remove(RECEIPT_LINK);
        events[1].audit_id = 3;
        events.insert(1, unrelated);
        assert!(valid(&before, &events));
    }

    #[test]
    fn immutable_first_bindings_need_the_exact_historical_declaration_on_replay() {
        let (before, _) = fixture();
        let command:super::super::Command=serde_json::from_value(json!({"contract_version":2,"idempotency_key":Uuid::new_v4(),
            "expected":{"intent":{"present":false}},"patch":{"intent":{"head":"original"}},"immutable_keys":["intent"]})).unwrap();
        let next = command.apply(&before).unwrap();
        let stamp = EventStamp::new("jobs", ActorId::Automation("fixture".into()));
        let (_, raw) = super::super::recorded_pair(&stamp, &command, next).unwrap();
        let events: Vec<_> = raw
            .into_iter()
            .enumerate()
            .map(|(n, e)| ReplayEvent {
                audit_id: n as i64 + 1,
                event_id: e.id,
                kind: e.kind,
                ts: e.timestamp,
                payload: e.payload,
            })
            .collect();
        let pairs = Pairs::read(&events).unwrap();
        assert!(
            pairs.next(events[0].event_id, &before, None).is_err(),
            "a matching pair cannot invent first-binding authority absent from the historical registry"
        );
        assert!(
            pairs
                .next(
                    events[0].event_id,
                    &before,
                    Some(&json!({"immutable_fields":["intent"]}))
                )
                .is_ok()
        );
    }
}
