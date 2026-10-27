//! Bounded, process-local receipts of the existing owner; no history reads.
use super::{FLOW_READ_DEADLINE, MAX_GENERATIONS};
#[cfg(test)]
mod cause_tests;
mod causes;
use causes::CauseProjection;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};

const MAX_EVENTS: usize = 32;
// One delta/initial, one optional rebuild, one current-window fallback.
const MAX_REFUSALS: usize = 3;
const ORIGINAL_BYTES_PER_FLIGHT: usize = 65_536;

#[derive(Clone, Serialize)]
pub struct Refusal {
    pub attempt: &'static str,
    pub complete: bool,
    pub total_bytes: usize,
    pub total_bytes_complete: bool,
    pub retained_bytes: usize,
    pub total_stations: usize,
    pub retained_stations: usize,
    pub omitted_stations: usize,
    pub total_spans: usize,
    pub retained_spans: usize,
    pub omitted_spans: usize,
    pub total_causes: usize,
    pub retained_causes: usize,
    pub omitted_causes: usize,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    pub audit_cutoff: Option<i64>,
    pub denied: Option<bool>,
    pub original: Option<serde_json::Value>,
    pub unavailable_reason: Option<&'static str>,
    pub cause_projection: Option<CauseProjection>,
}

impl Refusal {
    fn omit_aggregate(&mut self) {
        self.complete = false;
        self.original = None;
        self.retained_bytes = 0;
        self.retained_stations = 0;
        self.retained_spans = 0;
        self.retained_causes = 0;
        self.omitted_stations = self.total_stations;
        self.omitted_spans = self.total_spans;
        self.omitted_causes = self.total_causes;
        self.unavailable_reason = Some(
            "original aggregate displaced by original causes within flight diagnostic byte bound",
        );
    }
}

fn retained_bytes(flight: &Flight) -> usize {
    flight
        .refusals
        .iter()
        .map(|r| r.retained_bytes + r.cause_projection.as_ref().map_or(0, |c| c.retained_bytes))
        .sum()
}

fn prioritize_causes(flight: &mut Flight, needed: usize) -> bool {
    let mut used = retained_bytes(flight);
    let reclaimable: usize = flight
        .refusals
        .iter()
        .filter(|r| r.cause_projection.as_ref().is_some_and(|c| c.complete))
        .map(|r| r.retained_bytes)
        .sum();
    if needed > ORIGINAL_BYTES_PER_FLIGHT - (used - reclaimable) {
        return false;
    }
    for refusal in &mut flight.refusals {
        if needed <= ORIGINAL_BYTES_PER_FLIGHT - used {
            break;
        }
        if refusal.retained_bytes == 0 {
            continue;
        }
        if refusal
            .cause_projection
            .as_ref()
            .is_some_and(|c| c.complete)
        {
            used -= refusal.retained_bytes;
            refusal.omit_aggregate();
        }
    }
    true
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Original<'a> {
    Unread(&'a crate::membership_replay::MembershipReading),
    RebuildRequired(&'a str),
}

// Count the complete UTF-8 serialization, but never allocate its oversized
// copy. A prefix is never exposed as an original refusal (12d34448).
struct BoundedOriginal {
    bytes: Vec<u8>,
    total: usize,
    bound: usize,
}
impl std::io::Write for BoundedOriginal {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.total = self
            .total
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("original byte count overflow"))?;
        if self.total <= self.bound {
            self.bytes.extend_from_slice(bytes);
        } else {
            self.bytes.clear();
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Serialize)]
pub struct Event {
    pub phase: &'static str,
    pub attempt: &'static str,
    pub elapsed_ms: u128,
    pub remaining_ms: u128,
    pub attempt_remaining_ms: u128,
    pub revision: Option<i64>,
    pub cancelled: bool,
}

#[derive(Clone, Serialize)]
pub struct Flight {
    pub id: uuid::Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub deadline_ms: u128,
    pub elapsed_ms: u128,
    pub remaining_ms: u128,
    pub outcome: Option<&'static str>,
    pub events: Vec<Event>,
    pub omitted_events: usize,
    pub cancelled: bool,
    pub record_complete: bool,
    pub refusals: Vec<Refusal>,
    pub omitted_refusals: usize,
    pub original_byte_limit: usize,
}

pub(super) struct Trace {
    pub started: Instant,
    pub deadline: Instant,
    flight: Mutex<Flight>,
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    test_now: Arc<Mutex<Option<Instant>>>,
    #[cfg(test)]
    changed: Mutex<Option<Arc<tokio::sync::Notify>>>,
}

impl Trace {
    pub fn new(started: Instant, cancelled: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            started,
            deadline: started + FLOW_READ_DEADLINE,
            cancelled,
            #[cfg(test)]
            test_now: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            changed: Mutex::new(None),
            flight: Mutex::new(Flight {
                id: uuid::Uuid::new_v4(),
                started_at: boss_clock_client::wall_now(),
                deadline_ms: FLOW_READ_DEADLINE.as_millis(),
                elapsed_ms: 0,
                remaining_ms: FLOW_READ_DEADLINE.as_millis(),
                outcome: None,
                events: Vec::new(),
                omitted_events: 0,
                cancelled: false,
                record_complete: false,
                refusals: Vec::new(),
                omitted_refusals: 0,
                original_byte_limit: ORIGINAL_BYTES_PER_FLIGHT,
            }),
        })
    }

    pub(super) fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = *self.test_now.lock().unwrap() {
            return now;
        }
        Instant::now()
    }

    pub(super) fn budget(
        &self,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> crate::membership_replay::ReplayBudget {
        let budget = crate::membership_replay::ReplayBudget::new(
            deadline,
            cancelled,
            super::FLOW_REPLAY_FUEL,
        );
        #[cfg(test)]
        let budget = budget.with_test_clock(self.test_now.clone());
        budget
    }

    #[cfg(test)]
    pub(super) fn freeze(&self, now: Instant) {
        *self.test_now.lock().unwrap() = Some(now);
    }

    #[cfg(test)]
    fn advance(&self, elapsed: std::time::Duration) {
        let mut now = self.test_now.lock().unwrap();
        *now = Some(now.unwrap_or_else(Instant::now) + elapsed);
    }

    pub fn refusal(
        &self,
        attempt: &'static str,
        error: &crate::membership_replay::projection::ProjectionRefusal,
    ) {
        use crate::membership_replay::projection::ProjectionRefusal;
        let Ok(mut flight) = self.flight.lock() else {
            return;
        };
        if flight.refusals.len() == MAX_REFUSALS {
            flight.omitted_refusals += 1;
            return;
        }
        drop(flight);
        let (original, reading) = match error {
            ProjectionRefusal::Unread(reading) => {
                (Original::Unread(reading), Some(reading.as_ref()))
            }
            ProjectionRefusal::RebuildRequired(why) => (Original::RebuildRequired(why), None),
        };
        let mut writer = BoundedOriginal {
            bytes: Vec::new(),
            total: 0,
            bound: ORIGINAL_BYTES_PER_FLIGHT,
        };
        let serialized = serde_json::to_writer(&mut writer, &original);
        let mut retained = if serialized.is_ok() && writer.total <= writer.bound {
            serde_json::from_slice(&writer.bytes).ok()
        } else {
            None
        };
        let mut cause_projection = reading.map(CauseProjection::capture);
        // Serialization/counting is outside the observer's mutex. Recheck the
        // aggregate on insertion rather than making a reader wait for it.
        let Ok(mut flight) = self.flight.lock() else {
            return;
        };
        if flight.refusals.len() == MAX_REFUSALS {
            flight.omitted_refusals += 1;
            return;
        }
        if let Some(causes) = &mut cause_projection {
            if causes.complete && !prioritize_causes(&mut flight, causes.retained_bytes) {
                causes.omit();
            }
        } else if retained.is_some() && !prioritize_causes(&mut flight, writer.total) {
            retained = None;
        }
        let cause_bytes = cause_projection.as_ref().map_or(0, |c| c.retained_bytes);
        if writer.total > ORIGINAL_BYTES_PER_FLIGHT - retained_bytes(&flight) - cause_bytes {
            retained = None;
        }
        let complete = retained.is_some();
        let stations = reading.map_or(0, |r| r.stations.len());
        let spans = reading.map_or(0, |r| r.stations.iter().map(|s| s.unknown.len()).sum());
        let causes = reading.map_or(1, |r| spans + usize::from(r.denied));
        flight.refusals.push(Refusal {
            attempt,
            complete,
            total_bytes: writer.total,
            total_bytes_complete: serialized.is_ok(),
            retained_bytes: if complete { writer.total } else { 0 },
            total_stations: stations,
            retained_stations: if complete { stations } else { 0 },
            omitted_stations: if complete { 0 } else { stations },
            total_spans: spans,
            retained_spans: if complete { spans } else { 0 },
            omitted_spans: if complete { 0 } else { spans },
            total_causes: causes,
            retained_causes: if complete { causes } else { 0 },
            omitted_causes: if complete { 0 } else { causes },
            since: reading.map(|r| r.since),
            until: reading.map(|r| r.until),
            audit_cutoff: reading.map(|r| r.audit_cutoff),
            denied: reading.map(|r| r.denied),
            original: retained,
            cause_projection,
            unavailable_reason: if complete {
                None
            } else if serialized.is_err() {
                Some("original refusal serialization unavailable")
            } else {
                Some("original refusal exceeds flight diagnostic byte bound")
            },
        });
    }

    // Recording never checks/spends ReplayBudget fuel or changes its refusal.
    pub fn event(
        &self,
        phase: &'static str,
        attempt: &'static str,
        deadline: Instant,
        revision: Option<i64>,
    ) {
        if let Ok(mut flight) = self.flight.lock() {
            if flight.events.len() == MAX_EVENTS {
                flight.omitted_events += 1;
            } else {
                let now = self.now();
                flight.events.push(Event {
                    phase,
                    attempt,
                    elapsed_ms: now.saturating_duration_since(self.started).as_millis(),
                    remaining_ms: self.deadline.saturating_duration_since(now).as_millis(),
                    attempt_remaining_ms: deadline.saturating_duration_since(now).as_millis(),
                    revision,
                    cancelled: self.cancelled.load(Ordering::Acquire),
                });
            }
        }
        #[cfg(test)]
        if let Some(changed) = self.changed.lock().unwrap().as_ref() {
            changed.notify_one();
        }
    }

    pub fn finish(&self, outcome: &'static str) {
        self.event("owner_end", "flight", self.deadline, None);
        if let Ok(mut flight) = self.flight.lock() {
            flight.outcome = Some(outcome);
            flight.record_complete = flight.omitted_events == 0
                && flight.omitted_refusals == 0
                && flight.refusals.iter().all(|r| r.complete);
            flight.elapsed_ms = self
                .now()
                .saturating_duration_since(self.started)
                .as_millis();
            flight.remaining_ms = self
                .deadline
                .saturating_duration_since(self.now())
                .as_millis();
            flight.cancelled = self.cancelled.load(Ordering::Acquire);
        }
    }

    fn snapshot(&self) -> Option<Flight> {
        let mut flight = self.flight.lock().ok()?.clone();
        if flight.outcome.is_none() {
            flight.elapsed_ms = self
                .now()
                .saturating_duration_since(self.started)
                .as_millis();
            flight.remaining_ms = self
                .deadline
                .saturating_duration_since(self.now())
                .as_millis();
            flight.cancelled = self.cancelled.load(Ordering::Acquire);
        }
        Some(flight)
    }
}

#[derive(Default)]
struct Entry {
    active: Option<Arc<Trace>>,
    latest_completed: Option<Flight>,
}

#[derive(Default)]
pub(super) struct Receipts(
    Mutex<BTreeMap<String, Entry>>,
    #[cfg(test)] Arc<tokio::sync::Notify>,
);

#[derive(Serialize)]
pub struct Observation {
    pub state: &'static str,
    pub active: Option<Flight>,
    pub latest_completed: Option<Flight>,
    pub cached_failure_flight: Option<uuid::Uuid>,
    pub cached_failure: Option<Flight>,
}

impl Receipts {
    #[cfg(test)]
    pub(super) fn changed(&self) -> &tokio::sync::Notify {
        &self.1
    }
    #[cfg(test)]
    pub(super) fn advance(&self, key: &str, elapsed: std::time::Duration) {
        let entries = self.0.lock().unwrap();
        entries
            .get(key)
            .unwrap()
            .active
            .as_ref()
            .unwrap()
            .advance(elapsed);
    }
    pub fn cancel_requested(&self, key: &str) {
        if let Ok(entries) = self.0.lock()
            && let Some(trace) = entries.get(key).and_then(|entry| entry.active.as_ref())
        {
            trace.event("cancel_requested", "flight", trace.deadline, None);
        }
    }
    pub fn begin(&self, key: &str, trace: Arc<Trace>) {
        #[cfg(test)]
        {
            *trace.changed.lock().unwrap() = Some(self.1.clone());
        }
        if let Ok(mut entries) = self.0.lock() {
            if entries.len() == MAX_GENERATIONS && !entries.contains_key(key) {
                entries.pop_first();
            }
            entries.entry(key.into()).or_default().active = Some(trace);
        }
        #[cfg(test)]
        self.1.notify_one();
    }

    pub fn finish(&self, key: &str, trace: &Arc<Trace>) {
        if let Ok(mut entries) = self.0.lock()
            && let Some(entry) = entries.get_mut(key)
            && entry
                .active
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, trace))
        {
            entry.latest_completed = trace.snapshot();
            entry.active = None;
        }
    }

    pub fn read(&self, key: &str) -> Observation {
        let Ok(entries) = self.0.lock() else {
            return Observation {
                state: "unavailable",
                active: None,
                latest_completed: None,
                cached_failure_flight: None,
                cached_failure: None,
            };
        };
        let entry = entries.get(key);
        let active = entry
            .and_then(|entry| entry.active.as_ref())
            .and_then(|trace| trace.snapshot());
        let latest_completed = entry.and_then(|entry| entry.latest_completed.clone());
        let unavailable = entry.is_some_and(|entry| entry.active.is_some()) && active.is_none();
        Observation {
            state: if unavailable {
                "unavailable"
            } else if active.is_some() || latest_completed.is_some() {
                "observed"
            } else {
                "not_observed"
            },
            active,
            latest_completed,
            cached_failure_flight: None,
            cached_failure: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_controlled_flight_clock_is_shared_with_the_sticky_replay_budget() {
        let trace = Trace::new(Instant::now(), Arc::new(AtomicBool::new(false)));
        trace.freeze(trace.started);
        let budget = trace.budget(trace.deadline, Arc::new(AtomicBool::new(false)));
        trace.freeze(trace.deadline - std::time::Duration::from_millis(1));
        assert!(budget.check().is_ok());
        trace.freeze(trace.deadline);
        trace.event("deadline_control", "flight", trace.deadline, None);
        assert!(budget.check().unwrap_err().contains("deadline exhausted"));
        assert_eq!(
            trace
                .snapshot()
                .unwrap()
                .events
                .last()
                .unwrap()
                .remaining_ms,
            0
        );
        trace.freeze(trace.started);
        assert!(budget.check().unwrap_err().contains("deadline exhausted"));
    }

    #[test]
    fn aggregate_metrics_do_not_crowd_out_original_synthetic_causes() {
        use crate::membership_replay::{MembershipReading, projection::ProjectionRefusal};
        let until = chrono::DateTime::parse_from_rfc3339("2026-10-26T00:54:01.950227Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let since = until - chrono::Duration::hours(24);
        let stations: Vec<_> = (0..18).map(|index| serde_json::json!({
            "station": format!("synthetic-{index}"), "basis": "synthetic test fixture",
            "arrived": 1, "departed": 0, "served": 0,
            "known_arrived": 1, "known_departed": 0, "known_served": 0,
            "departure_causes": {},
            "unknown": if index < 17 { vec![serde_json::json!({
                "since": since, "until": until,
                "reason": format!("synthetic cause {index}: α\n\"exact\"")
            })] } else { vec![] },
            "covered": vec![serde_json::json!({"since": since, "until": until}); 100],
            "versions": [{"version": 1, "source": "synthetic", "publication_sequences": vec![7; 2_000]}],
            "caller_binding": "agent-codex", "scope": {"OwnerIs": "agent-codex"}
        })).collect();
        let expected = serde_json::json!({"stations": stations, "since": since,
            "until": until, "audit_cutoff": 2890800, "denied": false});
        let reading: MembershipReading = serde_json::from_value(expected.clone()).unwrap();
        assert!(
            serde_json::to_vec(&Original::Unread(&reading))
                .unwrap()
                .len()
                > ORIGINAL_BYTES_PER_FLIGHT
        );
        let trace = Trace::new(Instant::now(), Arc::new(AtomicBool::new(false)));
        trace.refusal(
            "fallback_24h",
            &ProjectionRefusal::Unread(Box::new(reading)),
        );
        trace.finish("refused");
        let receipt = serde_json::to_value(trace.snapshot().unwrap()).unwrap();
        let refusal = &receipt["refusals"][0];
        assert_eq!(refusal["complete"], false);
        assert!(refusal["original"].is_null());
        let projected = &refusal["cause_projection"];
        assert_eq!(projected["complete"], true);
        assert_eq!(projected["retained_causes"], 17);
        let payload = &projected["original_causes"];
        for key in ["since", "until", "audit_cutoff", "denied"] {
            assert_eq!(payload[key], expected[key]);
        }
        assert_eq!(payload["stations"].as_array().unwrap().len(), 18);
        for index in 0..18 {
            for key in ["station", "scope", "caller_binding", "unknown"] {
                assert_eq!(
                    payload["stations"][index][key],
                    expected["stations"][index][key]
                );
            }
            assert!(payload["stations"][index].get("covered").is_none());
            assert!(payload["stations"][index].get("versions").is_none());
        }
        assert_eq!(receipt["record_complete"], false);
    }

    #[test]
    fn original_bytes_are_aggregate_utf8_bounded_and_omissions_are_not_a_pass() {
        use crate::membership_replay::projection::ProjectionRefusal;
        let trace = Trace::new(Instant::now(), Arc::new(AtomicBool::new(false)));
        let why = "α\n\"".repeat(4_000);
        for attempt in ["delta", "rebuild_48h", "fallback_24h"] {
            trace.refusal(attempt, &ProjectionRefusal::RebuildRequired(why.clone()));
        }
        trace.finish("refused");
        let receipt = trace.snapshot().unwrap();
        assert_eq!(receipt.refusals.len(), 3);
        assert!(receipt.refusals[0].complete);
        assert!(!receipt.refusals[2].complete);
        assert!(!receipt.record_complete);
        assert!(
            receipt
                .refusals
                .iter()
                .map(|r| r.retained_bytes)
                .sum::<usize>()
                <= ORIGINAL_BYTES_PER_FLIGHT
        );
        for record in &receipt.refusals {
            let expected = serde_json::to_vec(&Original::RebuildRequired(&why)).unwrap();
            assert_eq!(record.total_bytes, expected.len());
            if let Some(original) = &record.original {
                assert_eq!(record.retained_causes, 1);
                assert_eq!(original["rebuild_required"], why);
                assert_eq!(serde_json::to_vec(original).unwrap(), expected);
            } else {
                assert_eq!(record.retained_bytes, 0);
                assert_eq!(record.omitted_causes, 1);
            }
        }
        trace.refusal(
            "unexpected_extra_attempt",
            &ProjectionRefusal::RebuildRequired("extra".into()),
        );
        trace.finish("refused");
        let receipt = trace.snapshot().unwrap();
        assert_eq!(receipt.refusals.len(), 3);
        assert_eq!(receipt.omitted_refusals, 1);
        assert!(!receipt.record_complete);
    }

    #[test]
    fn a_typed_denied_reading_retains_the_original_flag_and_context() {
        use crate::membership_replay::{MembershipReading, projection::ProjectionRefusal};
        let until = chrono::Utc::now();
        let reading = MembershipReading {
            stations: vec![],
            since: until - chrono::Duration::hours(24),
            until,
            audit_cutoff: 17,
            denied: true,
        };
        let expected = serde_json::to_value(&reading).unwrap();
        let trace = Trace::new(Instant::now(), Arc::new(AtomicBool::new(false)));
        trace.refusal(
            "fallback_24h",
            &ProjectionRefusal::Unread(Box::new(reading)),
        );
        trace.finish("refused");
        let receipt = trace.snapshot().unwrap();
        assert!(receipt.record_complete);
        let original = &receipt.refusals[0];
        assert_eq!(original.denied, Some(true));
        assert_eq!(original.audit_cutoff, Some(17));
        assert_eq!(original.original.as_ref().unwrap()["unread"], expected);
        let projected = serde_json::to_value(original.cause_projection.as_ref().unwrap()).unwrap();
        assert_eq!(projected["complete"], true);
        assert_eq!(projected["retained_causes"], 1);
        assert_eq!(projected["original_causes"], expected);
    }

    #[test]
    fn receipts_and_events_are_bounded_and_an_old_join_cannot_clear_a_new_flight() {
        let receipts = Receipts::default();
        let old = Trace::new(Instant::now(), Arc::new(AtomicBool::new(false)));
        receipts.begin("reader-00", old.clone());
        let next = Trace::new(Instant::now(), Arc::new(AtomicBool::new(false)));
        receipts.begin("reader-00", next.clone());
        old.finish("refused");
        receipts.finish("reader-00", &old);
        assert_eq!(
            receipts.read("reader-00").active.unwrap().id,
            next.snapshot().unwrap().id
        );
        for index in 1..=MAX_GENERATIONS {
            let trace = Trace::new(Instant::now(), Arc::new(AtomicBool::new(false)));
            receipts.begin(&format!("reader-{index:02}"), trace.clone());
            for _ in 0..MAX_EVENTS + 3 {
                trace.event("source_begin", "initial_48h", trace.deadline, None);
            }
            trace.finish("refused");
            receipts.finish(&format!("reader-{index:02}"), &trace);
        }
        assert_eq!(receipts.0.lock().unwrap().len(), MAX_GENERATIONS);
        assert_eq!(receipts.read("reader-00").state, "not_observed");
        let last = receipts.read("reader-16").latest_completed.unwrap();
        assert_eq!(last.events.len(), MAX_EVENTS);
        assert_eq!(
            last.omitted_events, 4,
            "overflow must state missing events, including owner_end"
        );
        assert!(!last.record_complete);
    }
}
