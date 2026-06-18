//! Sim-time scheduler — a min-by-sim-time event heap.
//!
//! See `docs/design/scheduler-shaped-sim-engine.md` for the full
//! design. This module owns the heap + the `ScheduledEvent` type:
//! events are pushed with a target sim instant and popped in
//! sim-time order, so a dispatch loop can wait for clock-api to
//! reach each event's instant before firing it. The dispatch loop
//! itself is not wired into the engine yet.
//!
//! ## Invariants
//!
//! 1. **Sim-time monotonicity**: `pop()` returns events in
//!    non-decreasing `sim_time` order. Ties broken by insertion
//!    order so determinism survives.
//! 2. **No emit before wake**: a popped event's dispatch must
//!    not run until `clock.now() >= event.sim_time`.

use std::collections::BinaryHeap;

use chrono::{DateTime, Utc};

/// A future event the scheduler dispatches at `sim_time`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduledEvent {
    /// A Step transitioned from Pending / Ready to Active. Scheduler
    /// emits the `step.active` event and queues the matching
    /// `CompleteStep` at
    /// `sim_time + StepType.typical_duration_hours`.
    StartStep { job_id: String, step_id: String },
    /// A Step transitioned from Active to Completed. Scheduler
    /// runs the side-effect handler (sync), emits the
    /// `step.completed` event, and may queue follow-on
    /// `StartStep` events for now-unblocked next-tier steps.
    CompleteStep { job_id: String, step_id: String },
}

/// Heap entry: `(sim_time, insertion_seq, event)`. Ordered by
/// sim_time ascending (we store as `Reverse` since `BinaryHeap`
/// is a max-heap), with insertion_seq tiebreak so concurrent
/// emits at the same sim instant dispatch in deterministic
/// order — matters for replay determinism.
#[derive(Debug, Clone, Eq, PartialEq)]
struct HeapEntry {
    sim_time: DateTime<Utc>,
    insertion_seq: u64,
    event: ScheduledEvent,
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse the natural ordering so BinaryHeap (max-heap)
        // pops the EARLIEST sim_time first.
        other
            .sim_time
            .cmp(&self.sim_time)
            .then_with(|| other.insertion_seq.cmp(&self.insertion_seq))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The sim-time scheduler. Owns the heap + the insertion counter.
#[derive(Debug, Default)]
pub struct SimScheduler {
    heap: BinaryHeap<HeapEntry>,
    next_seq: u64,
}

impl SimScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an event for dispatch at `sim_time`. Ties dispatch
    /// in insertion order.
    pub fn push(&mut self, sim_time: DateTime<Utc>, event: ScheduledEvent) {
        let entry = HeapEntry {
            sim_time,
            insertion_seq: self.next_seq,
            event,
        };
        self.next_seq += 1;
        self.heap.push(entry);
    }

    /// Peek the next event's sim_time without removing it. None
    /// when the heap is empty.
    pub fn peek_sim_time(&self) -> Option<DateTime<Utc>> {
        self.heap.peek().map(|e| e.sim_time)
    }

    /// Pop the next event (earliest sim_time). None when empty.
    pub fn pop(&mut self) -> Option<(DateTime<Utc>, ScheduledEvent)> {
        self.heap.pop().map(|e| (e.sim_time, e.event))
    }

    /// Number of pending events.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// True when the heap is empty.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap()
            .and_utc()
    }

    fn step(job: &str, step: &str) -> ScheduledEvent {
        ScheduledEvent::CompleteStep {
            job_id: job.into(),
            step_id: step.into(),
        }
    }

    #[test]
    fn empty_pop_returns_none() {
        let mut s = SimScheduler::new();
        assert!(s.is_empty());
        assert_eq!(s.peek_sim_time(), None);
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn pops_in_sim_time_order_regardless_of_push_order() {
        let mut s = SimScheduler::new();
        s.push(dt(2025, 4, 5, 12), step("job-c", "step-c"));
        s.push(dt(2025, 4, 1, 6), step("job-a", "step-a"));
        s.push(dt(2025, 4, 3, 9), step("job-b", "step-b"));

        let (t1, e1) = s.pop().unwrap();
        assert_eq!(t1, dt(2025, 4, 1, 6));
        assert_eq!(e1, step("job-a", "step-a"));

        let (t2, e2) = s.pop().unwrap();
        assert_eq!(t2, dt(2025, 4, 3, 9));
        assert_eq!(e2, step("job-b", "step-b"));

        let (t3, e3) = s.pop().unwrap();
        assert_eq!(t3, dt(2025, 4, 5, 12));
        assert_eq!(e3, step("job-c", "step-c"));

        assert!(s.is_empty());
    }

    #[test]
    fn ties_dispatch_in_insertion_order() {
        // Determinism contract: same sim_time, FIFO order.
        let mut s = SimScheduler::new();
        let t = dt(2025, 4, 1, 9);
        s.push(t, step("first", "step-1"));
        s.push(t, step("second", "step-2"));
        s.push(t, step("third", "step-3"));

        assert_eq!(s.pop().unwrap().1, step("first", "step-1"));
        assert_eq!(s.pop().unwrap().1, step("second", "step-2"));
        assert_eq!(s.pop().unwrap().1, step("third", "step-3"));
    }

    #[test]
    fn peek_does_not_consume() {
        let mut s = SimScheduler::new();
        let t = dt(2025, 4, 1, 9);
        s.push(t, step("j", "s"));
        assert_eq!(s.peek_sim_time(), Some(t));
        assert_eq!(s.peek_sim_time(), Some(t));
        assert_eq!(s.len(), 1);
    }
}
