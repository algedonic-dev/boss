//! Live readiness of the dispatcher's durable JetStream consumers — the
//! signal a `/api/dispatcher/health` 200 cannot give you.
//!
//! `boss-dispatcher` spawns its consumer loops detached: if one's message
//! stream ends — a NATS blip, or JetStream not ready at cold start under a
//! launcher with NO per-service restart (the docker `services-launcher.sh`
//! uses `wait -n`, unlike systemd which restarts a dead unit) — the loop just
//! logs and ends while the PROCESS stays up and health-green. With the rules
//! consumer dead, no finished step's side effect runs and every Job that
//! waits on one stalls `open`: the exact "thousands of jobs in flight,
//! nothing reaching a terminal state" failure. (The assignment consumer that
//! once stood beside it was retired by backlog e1eaf72b.)
//!
//! This handle makes that visible at `/api/dispatcher/readyz` so an operator —
//! and the brewery sim's pre-Go readiness gate — can confirm the consumers are
//! actually bound and draining before trusting the stack with work.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

/// Completed phase durations, summed across concurrent events. Totals
/// are work time, not wall time or a promise about currently stuck work.
#[derive(Default, serde::Serialize)]
struct RulesPhaseCosts {
    events: u64,
    match_nanos: u64,
    dispatch_calls: u64,
    dispatch_nanos: u64,
    record_calls: u64,
    record_rows_attempted: u64,
    record_nanos: u64,
}

struct LaneState {
    cutover: Option<crate::rules::delivery::Cutover>,
    running: bool,
    lag: Option<Result<crate::rules::lag::ConsumerLag, String>>,
}

/// Shared liveness and operational readings for the consumers. One instance is
/// created in `main`, cloned into each consumer loop (which marks it), and
/// cloned into the HTTP surface (which reads it for `/readyz`).
#[derive(Default)]
pub struct DispatcherLiveness {
    delivery_lanes: std::sync::RwLock<std::collections::BTreeMap<String, LaneState>>,
    rules_lag: std::sync::RwLock<Option<Result<crate::rules::lag::ConsumerLag, String>>>,
    rules_costs: std::sync::Mutex<RulesPhaseCosts>,
    /// The rules runner's `dispatcher-rules` consumer is bound + draining.
    rules_running: AtomicBool,
    /// Side-effect events the rules runner has handled since bind (monotonic).
    rules_events: AtomicU64,
    /// The schedule runner's clock-stream loop is live (consuming ticks).
    /// Unlike the two JetStream consumers this is an SSE loop, but it's the
    /// same liveness question: a process that's health-200 but whose
    /// schedule loop died fires zero clock-driven rules (payroll, tax,
    /// sweeps stop). `true` only matters when the registry HAS schedule
    /// rules — with none, the runner never starts and this stays false.
    schedule_running: AtomicBool,
    /// Sim-days the schedule runner has fired since start (monotonic).
    schedule_events: AtomicU64,
    /// Wall-clock unix seconds of the most recently handled event (0 = none yet).
    last_event_unix: AtomicI64,
    /// Dead-letters since start (monotonic): handler failures past their
    /// budget, or permanent. Counted HERE — a local read — because the
    /// durable record (an annotation on the packet) needs the jobs API,
    /// which is exactly what a dead-letter often reports broken
    /// (8834804a). Six topics carry no packet at all; for those and for
    /// a failed annotation write, this number is the only record that
    /// outlives the log line.
    dead_letters: AtomicU64,
    /// Of those, how many left NO durable record: no packet to annotate,
    /// no sink configured, or the annotation write failed.
    dead_letters_unrecorded: AtomicU64,
    /// Wall-clock unix seconds of the most recent dead-letter (0 = none).
    last_dead_letter_unix: AtomicI64,
    /// Wall-clock unix seconds of the most recent UNRECORDED dead-letter
    /// (0 = none). Its own stamp, because the estate chain's finding
    /// (`estate.compare`, 8834804a) is "an unrecorded one happened
    /// recently" judged against the observation's stamp — and a
    /// recorded dead-letter moving `last_dead_letter_unix` would keep a
    /// stale unrecorded one looking fresh.
    last_unrecorded_dead_letter_unix: AtomicI64,
}

impl DispatcherLiveness {
    pub fn configure_delivery_lanes(&self, cutovers: &[crate::rules::delivery::Cutover]) {
        if let Ok(mut lanes) = self.delivery_lanes.write() {
            *lanes = cutovers
                .iter()
                .map(|c| {
                    (
                        c.lane.clone(),
                        LaneState {
                            cutover: Some(c.clone()),
                            running: false,
                            lag: None,
                        },
                    )
                })
                .collect();
        }
    }

    /// Refused lanes remain visibly not-ready while independent consumers run.
    pub fn configure_delivery_lane_refusals(
        &self,
        refused: &std::collections::BTreeMap<String, String>,
    ) {
        if let Ok(mut lanes) = self.delivery_lanes.write() {
            for (name, reason) in refused {
                lanes.insert(
                    name.clone(),
                    LaneState {
                        cutover: None,
                        running: false,
                        lag: Some(Err(reason.clone())),
                    },
                );
            }
        }
    }

    pub fn mark_delivery_lane(&self, lane: &str, running: bool) {
        if let Ok(mut lanes) = self.delivery_lanes.write()
            && let Some(state) = lanes.get_mut(lane)
        {
            state.running = running;
        }
    }

    pub fn record_delivery_lane_lag(
        &self,
        lane: &str,
        lag: Result<crate::rules::lag::ConsumerLag, String>,
    ) {
        if let Ok(mut lanes) = self.delivery_lanes.write()
            && let Some(state) = lanes.get_mut(lane)
        {
            state.lag = Some(lag);
        }
    }

    fn delivery_lanes_observation(&self) -> serde_json::Value {
        match self.delivery_lanes.read() {
            Ok(lanes) => {
                serde_json::json!({"capability_version":1,"supported_transports":["jetstream"],"lanes":lanes.iter().map(|(name,state)| {
                let lag = match &state.lag {
                    Some(Ok(value)) => value.snapshot(Self::now_unix()),
                    Some(Err(error)) => serde_json::json!({"status":"unavailable","error":error}),
                    None => serde_json::json!({"status":"unobserved"}),
                };
                (name.clone(),serde_json::json!({"cutover":state.cutover,"running":state.running,"lag":lag}))
            }).collect::<std::collections::BTreeMap<_,_>>()})
            }
            Err(_) => {
                serde_json::json!({"capability_version":1,"status":"unavailable","reason":"delivery-lane observation lock unread"})
            }
        }
    }
    pub fn record_match_cost(&self, elapsed: std::time::Duration) {
        if let Ok(mut costs) = self.rules_costs.lock() {
            costs.events = costs.events.saturating_add(1);
            costs.match_nanos = costs
                .match_nanos
                .saturating_add(elapsed.as_nanos().try_into().unwrap_or(u64::MAX));
        }
    }

    pub fn record_dispatch_cost(&self, elapsed: std::time::Duration) {
        if let Ok(mut costs) = self.rules_costs.lock() {
            costs.dispatch_calls = costs.dispatch_calls.saturating_add(1);
            costs.dispatch_nanos = costs
                .dispatch_nanos
                .saturating_add(elapsed.as_nanos().try_into().unwrap_or(u64::MAX));
        }
    }

    pub fn record_firing_cost(&self, elapsed: std::time::Duration, rows: usize) {
        if let Ok(mut costs) = self.rules_costs.lock() {
            costs.record_calls = costs.record_calls.saturating_add(1);
            costs.record_rows_attempted = costs.record_rows_attempted.saturating_add(rows as u64);
            costs.record_nanos = costs
                .record_nanos
                .saturating_add(elapsed.as_nanos().try_into().unwrap_or(u64::MAX));
        }
    }
    pub fn record_rules_lag(&self, sample: Result<crate::rules::lag::ConsumerLag, String>) {
        if let Ok(mut current) = self.rules_lag.write() {
            *current = Some(sample);
        }
    }

    fn rules_lag_snapshot(&self) -> serde_json::Value {
        match self.rules_lag.read().ok().as_deref() {
            Some(Some(Ok(sample))) => sample.snapshot(Self::now_unix()),
            Some(Some(Err(error))) => serde_json::json!({"status": "unavailable", "error": error}),
            _ => serde_json::json!({"status": "unobserved"}),
        }
    }

    /// Health's richer operational reading. Readyz keeps its original
    /// numbers-and-flags-only public contract.
    pub fn observation(&self) -> serde_json::Value {
        let mut value = self.snapshot();
        value["rules_consumer_lag"] = self.rules_lag_snapshot();
        value["rules_delivery_lanes"] = self.delivery_lanes_observation();
        value["rules_phase_costs"] = self
            .rules_costs
            .lock()
            .ok()
            .map(|costs| serde_json::json!(*costs))
            .unwrap_or(serde_json::Value::Null);
        value
    }
    fn now_unix() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Called by the rules runner once its durable consumer is bound.
    pub fn mark_rules_running(&self) {
        self.rules_running.store(true, Ordering::Relaxed);
    }
    /// Called by the rules runner when its message stream ends.
    pub fn mark_rules_stopped(&self) {
        self.rules_running.store(false, Ordering::Relaxed);
    }
    /// Called by the rules runner per handled side-effect event.
    pub fn record_rules(&self) {
        self.rules_events.fetch_add(1, Ordering::Relaxed);
        self.last_event_unix
            .store(Self::now_unix(), Ordering::Relaxed);
    }

    /// Called by the schedule runner once its clock-stream loop is live.
    pub fn mark_schedule_running(&self) {
        self.schedule_running.store(true, Ordering::Relaxed);
    }
    /// Called by the schedule runner when its clock stream ends.
    pub fn mark_schedule_stopped(&self) {
        self.schedule_running.store(false, Ordering::Relaxed);
    }
    /// Called by the schedule runner per sim-day it fires.
    /// One dead-letter happened; `recorded` says whether a durable record
    /// (the packet annotation) landed for it.
    pub fn record_dead_letter(&self, recorded: bool) {
        let now = Self::now_unix();
        self.dead_letters.fetch_add(1, Ordering::Relaxed);
        if !recorded {
            self.dead_letters_unrecorded.fetch_add(1, Ordering::Relaxed);
            self.last_unrecorded_dead_letter_unix
                .store(now, Ordering::Relaxed);
        }
        self.last_dead_letter_unix.store(now, Ordering::Relaxed);
    }

    pub fn record_schedule(&self) {
        self.schedule_events.fetch_add(1, Ordering::Relaxed);
        self.last_event_unix
            .store(Self::now_unix(), Ordering::Relaxed);
    }

    /// JSON body for `/api/dispatcher/readyz`. `ready` is true only when the
    /// rules consumer and every delivery lane are bound — the precondition
    /// for a finished step's side effects to run. (It also required the
    /// assignment consumer until that was retired, backlog e1eaf72b: a
    /// ready step now stands at its station until an executor pulls it.)
    pub fn snapshot(&self) -> serde_json::Value {
        let rules_running = self.rules_running.load(Ordering::Relaxed);
        let lanes_running = self
            .delivery_lanes
            .read()
            .map(|lanes| lanes.values().all(|v| v.running))
            .unwrap_or(false);
        // `ready` stays gated on the JetStream consumers only — the
        // schedule runner is optional (it never starts when the registry
        // has no schedule rules), so gating readiness on it would falsely
        // mark a schedule-free deployment not-ready. Its liveness is
        // reported as informational fields instead.
        serde_json::json!({
            "ready": rules_running && lanes_running,
            "rules_running": rules_running,
            "rules_events": self.rules_events.load(Ordering::Relaxed),
            "schedule_running": self.schedule_running.load(Ordering::Relaxed),
            "schedule_events": self.schedule_events.load(Ordering::Relaxed),
            "last_event_unix": self.last_event_unix.load(Ordering::Relaxed),
            "dead_letters": self.dead_letters.load(Ordering::Relaxed),
            "dead_letters_unrecorded": self.dead_letters_unrecorded.load(Ordering::Relaxed),
            "last_dead_letter_unix": self.last_dead_letter_unix.load(Ordering::Relaxed),
            "last_unrecorded_dead_letter_unix": self.last_unrecorded_dead_letter_unix.load(Ordering::Relaxed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bound_consumer_is_not_evidence_of_a_measured_backlog() {
        let live = DispatcherLiveness::default();
        live.mark_rules_running();
        let snapshot = live.observation();
        assert_eq!(snapshot["rules_consumer_lag"]["status"], "unobserved");
        assert!(snapshot["rules_consumer_lag"]["pending"].is_null());
        assert!(snapshot["rules_consumer_lag"]["oldest_unacked_age_seconds"].is_null());
    }

    /// The finding the estate chain raises on (8834804a) is "an
    /// unrecorded dead-letter happened RECENTLY", judged against the
    /// observation's own stamp. `last_dead_letter_unix` cannot carry
    /// that: a recorded dead-letter after an unrecorded one would move
    /// it and keep an old unrecorded one looking fresh. So the
    /// unrecorded ones get a stamp of their own, which a recorded one
    /// leaves alone.
    #[test]
    fn an_unrecorded_dead_letter_has_its_own_stamp_that_a_recorded_one_does_not_move() {
        let live = DispatcherLiveness::default();
        let snap = live.snapshot();
        assert_eq!(snap["last_unrecorded_dead_letter_unix"], 0, "{snap}");

        live.record_dead_letter(true);
        let snap = live.snapshot();
        assert!(
            snap["last_dead_letter_unix"].as_i64().unwrap_or(0) > 0,
            "{snap}"
        );
        assert_eq!(
            snap["last_unrecorded_dead_letter_unix"], 0,
            "a recorded dead-letter is not an unrecorded one: {snap}"
        );

        live.record_dead_letter(false);
        let snap = live.snapshot();
        let unrecorded_at = snap["last_unrecorded_dead_letter_unix"]
            .as_i64()
            .unwrap_or(0);
        assert!(unrecorded_at > 0, "{snap}");
        assert_eq!(snap["dead_letters"], 2, "{snap}");
        assert_eq!(snap["dead_letters_unrecorded"], 1, "{snap}");

        // Push the stamp into the past and record a RECORDED one: the
        // unrecorded stamp must not follow it.
        live.last_unrecorded_dead_letter_unix
            .store(unrecorded_at - 3600, Ordering::Relaxed);
        live.record_dead_letter(true);
        let snap = live.snapshot();
        assert_eq!(
            snap["last_unrecorded_dead_letter_unix"],
            unrecorded_at - 3600,
            "{snap}"
        );
    }
}
