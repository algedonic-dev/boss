//! A STATION'S FLOW IS COUNTED OVER THE STEP RECORD, NOT REPLAYED
//! (design 7b8ed079, "Station membership is read, not replayed", car 1).
//!
//! Every station membership projection went stale on 2026-10-30 (urgent
//! b3572a26) and five cars into the replay found five different bounds.
//! The design's answer: a station's history comes from timestamps the
//! record already holds. A step carries two — `became_ready_at` (the
//! ready flip, schema 202609021000) and `completed_at` (the completion,
//! schema 202609081700) — and those two are the whole of a step's stay
//! at a station. So:
//!
//! - an ARRIVAL is a step that became ready at the station in the window;
//! - a DEPARTURE is a step that completed there in the window, which is
//!   also what the station served (a skipped step records no completion
//!   and is neither);
//! - WAITING is the current queue, and AGE the oldest standing step's
//!   ready flip — both read off the open packets by the caller, not here.
//!
//! "At the station" is the station's own predicate (the registry stays the
//! definition, CLAUDE.md §9), asked of the packet AS ITS STEPS STOOD at
//! the instant, which the two stamps decide exactly: a step is completed
//! from `completed_at`, ready or active from `became_ready_at` until then,
//! and pending before. Three readings follow from the predicate's shape:
//!
//! - a STEP CLAUSE naming live work (no status, or ready/active among its
//!   statuses) puts the step it matches there, so that step's own ready
//!   flip and completion are the arrival and the departure;
//! - a step clause naming ONLY finished work (`design-decided`: a review
//!   step that is `completed`) holds the packet from that completion on,
//!   so every step of the packet that becomes ready or completes while it
//!   is held there moves through it;
//! - NO step clause (`gates`: an open gate-run) holds the whole packet,
//!   so every step of it does.
//!
//! WHAT IS READ AS IT STANDS NOW, stated rather than implied: the Job's
//! own fields — kind, tags, metadata. A station whose Job-level metadata
//! clause flips after a step completes counts that step against what the
//! packet says now. The Job STATUS clause is read as satisfied: the step
//! that closes a packet completed while it was open, and that departure
//! must count. No station the map judges is known to need more; the
//! design's rule if one does is a column the step records at completion,
//! never a replay.

use boss_core::job::{Job, JobId, Step, StepId, StepStatus};
use chrono::{DateTime, Utc};

use crate::station_queue::{StationPredicate, StepMatch};

/// One step as the window reads it: the row and its recorded ready flip,
/// `None` where the record holds none (a step that predates the stamp, or
/// one that never became ready).
#[derive(Debug, Clone, PartialEq)]
pub struct StampedStep {
    pub step: Step,
    pub ready_at: Option<DateTime<Utc>>,
}

/// A packet the window touched — one of its steps became ready or
/// completed inside it — with EVERY one of its steps, because whether a
/// packet stood at a station is a question about all of them.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowPacket {
    pub job: Job,
    pub steps: Vec<StampedStep>,
}

/// One station's counts in one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowFlow {
    /// Steps that became ready at the station in the window.
    pub arrived: u64,
    /// Steps that completed at the station in the window — what it served.
    pub departed: u64,
}

/// The packets as the adapters read them: each Job with its steps, each
/// step stamped with `ready_at`'s answer for it. A step whose Job was not
/// read is dropped with it — the scope that admitted the step's packet is
/// the scope that reads the packet. Steps keep their order.
pub fn packets_of(
    jobs: Vec<Job>,
    steps: Vec<Step>,
    ready_at: impl Fn(&StepId) -> Option<DateTime<Utc>>,
) -> Vec<WindowPacket> {
    let mut by_job: std::collections::HashMap<JobId, Vec<StampedStep>> =
        std::collections::HashMap::new();
    for step in steps {
        let ready_at = ready_at(&step.id);
        by_job
            .entry(step.job_id)
            .or_default()
            .push(StampedStep { step, ready_at });
    }
    jobs.into_iter()
        .map(|job| WindowPacket {
            steps: by_job.remove(&job.id).unwrap_or_default(),
            job,
        })
        .collect()
}

/// The counts of ONE station (its predicate already bound to the caller,
/// [`StationPredicate::bind_self`]) over the packets the window touched,
/// for the instants `in_window` accepts.
pub fn station_window_flow(
    predicate: &StationPredicate,
    packets: &[WindowPacket],
    in_window: impl Fn(DateTime<Utc>) -> bool,
) -> WindowFlow {
    packets
        .iter()
        .flat_map(|packet| packet.steps.iter().map(move |moving| (packet, moving)))
        .fold(WindowFlow::default(), |flow, (packet, moving)| {
            let arrived = moving
                .ready_at
                .filter(|t| in_window(*t))
                .is_some_and(|t| there(predicate, packet, moving, t, Edge::After));
            let departed = moving
                .step
                .completed_at
                .filter(|t| in_window(*t))
                .is_some_and(|t| there(predicate, packet, moving, t, Edge::Before));
            WindowFlow {
                arrived: flow.arrived + u64::from(arrived),
                departed: flow.departed + u64::from(departed),
            }
        })
}

/// Which side of an instant a packet is read on: just AFTER a step became
/// ready (it is then ready, and a step that completed at that same
/// instant — the predecessor that released it — is completed), or just
/// BEFORE a step completed (it is then still working, and so is any step
/// completing at that same instant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    After,
    Before,
}

/// Whether `moving` was at the station at `t`, read from `edge` of it.
fn there(
    predicate: &StationPredicate,
    packet: &WindowPacket,
    moving: &StampedStep,
    t: DateTime<Utc>,
    edge: Edge,
) -> bool {
    let job = &packet.job;
    // The status clause is read as satisfied (module header): the step
    // that closes a packet completed while it was open.
    let status = predicate.status.unwrap_or(job.status);
    if !predicate.job_fields_match(&job.kind, &status, &job.tags, &job.metadata) {
        return false;
    }
    match &predicate.step {
        None => true,
        Some(clause) if holds_finished_work(clause) => packet
            .steps
            .iter()
            .flat_map(|s| as_of(s, t, edge))
            .any(|s| clause.matches(&s)),
        // The moving step itself is working at both edges; which of
        // ready or active it was the record does not say, so either one
        // the clause names is enough.
        Some(clause) => working(&moving.step).any(|s| clause.matches(&s)),
    }
}

/// A clause that names statuses, none of them live work: it holds a
/// packet from a step's completion on (`design-decided`).
fn holds_finished_work(clause: &StepMatch) -> bool {
    !clause.status_in.is_empty()
        && !clause
            .status_in
            .iter()
            .any(|s| matches!(s, StepStatus::Ready | StepStatus::Active))
}

/// The step as working: ready, then active.
fn working(step: &Step) -> impl Iterator<Item = Step> + '_ {
    [StepStatus::Ready, StepStatus::Active]
        .into_iter()
        .map(move |status| Step {
            status,
            ..step.clone()
        })
}

/// The step as it stood at `t`, read from `edge` — every status the stamps
/// leave open (ready or active while working). A step with neither stamp
/// keeps the status it holds now: the record has nothing earlier to say.
fn as_of(stamped: &StampedStep, t: DateTime<Utc>, edge: Edge) -> Vec<Step> {
    let reached = |at: DateTime<Utc>| match edge {
        Edge::After => at <= t,
        Edge::Before => at < t,
    };
    let step = &stamped.step;
    let with = |status| Step {
        status,
        ..step.clone()
    };
    match (stamped.ready_at, step.completed_at) {
        (_, Some(done)) if reached(done) => vec![with(StepStatus::Completed)],
        (Some(ready), _) if reached(ready) => working(step).collect(),
        (Some(_), _) | (None, Some(_)) => vec![with(StepStatus::Pending)],
        (None, None) => vec![step.clone()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{JobId, JobStatus, Priority, Subject};
    use std::collections::BTreeMap;

    fn at(hour: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_800_000_000, 0).unwrap() + chrono::Duration::hours(hour)
    }

    fn packet(kind: &str, status: JobStatus, steps: Vec<StampedStep>) -> WindowPacket {
        let mut job = Job::new(
            kind,
            Subject::new("custom", "x"),
            "t",
            "emp-1",
            Priority::Standard,
            at(0).date_naive(),
        );
        job.status = status;
        let steps = steps
            .into_iter()
            .map(|mut s| {
                s.step.job_id = job.id;
                s
            })
            .collect();
        WindowPacket { job, steps }
    }

    /// A step of `kind` with `slug`, ready at `ready` and completed at
    /// `done`, its status what those two say it is now.
    fn stamped(slug: &str, kind: &str, ready: Option<i64>, done: Option<i64>) -> StampedStep {
        let mut step = Step::new(JobId::new(), kind, slug, 0);
        step.spec_slug = Some(slug.to_string());
        step.metadata = serde_json::json!({"authority_role": "platform-admin"});
        step.status = match (ready, done) {
            (_, Some(_)) => StepStatus::Completed,
            (Some(_), None) => StepStatus::Ready,
            (None, None) => StepStatus::Pending,
        };
        step.completed_at = done.map(at);
        StampedStep {
            step,
            ready_at: ready.map(at),
        }
    }

    fn live_clause(kind: &str) -> StationPredicate {
        StationPredicate {
            status: Some(JobStatus::Open),
            step: Some(StepMatch {
                kind: Some(kind.into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                metadata_equals: BTreeMap::from([(
                    "authority_role".into(),
                    "platform-admin".into(),
                )]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn window(from: i64, to: i64) -> impl Fn(DateTime<Utc>) -> bool {
        move |t| t >= at(from) && t <= at(to)
    }

    /// The live-work station counts its own steps: the one that became
    /// ready in the window arrived, the one that completed departed, and
    /// a step of another kind on the same packet is neither — even the
    /// one whose completion CLOSED the packet, which the open-status
    /// clause must not hide.
    #[test]
    fn a_live_work_station_counts_the_steps_its_clause_names() {
        let open = packet(
            "backlog-item",
            JobStatus::Open,
            vec![
                stamped("triage", "task", Some(1), Some(3)),
                stamped("build", "task", Some(3), None),
                stamped("filed", "trigger", Some(0), Some(1)),
            ],
        );
        let closed = packet(
            "backlog-item",
            JobStatus::Closed,
            vec![stamped("verify", "task", Some(2), Some(4))],
        );
        let flow = station_window_flow(&live_clause("task"), &[open, closed], window(2, 5));
        assert_eq!(
            flow,
            WindowFlow {
                arrived: 2,
                departed: 2
            }
        );
    }

    /// A station that holds FINISHED work (`design-decided`: the review
    /// step completed) holds the packet from that completion on, so the
    /// steps that become ready and complete after it move through it —
    /// and the review step's own completion is the packet ARRIVING, not
    /// leaving.
    #[test]
    fn a_station_holding_finished_work_counts_what_moves_while_it_holds_the_packet() {
        let decided = StationPredicate {
            kind: Some("design-doc".into()),
            step: Some(StepMatch {
                slug: Some("review".into()),
                status_in: vec![StepStatus::Completed],
                ..Default::default()
            }),
            ..Default::default()
        };
        let doc = packet(
            "design-doc",
            JobStatus::Open,
            vec![
                stamped("review", "review-design", Some(1), Some(3)),
                stamped("fold", "task", Some(3), Some(4)),
                stamped("published", "outcome", Some(4), None),
            ],
        );
        let in_review = packet(
            "design-doc",
            JobStatus::Open,
            vec![
                stamped("review", "review-design", Some(3), None),
                stamped("fold", "task", None, None),
            ],
        );
        let flow = station_window_flow(&decided, &[doc, in_review], window(2, 5));
        assert_eq!(
            flow,
            WindowFlow {
                arrived: 2,
                departed: 1
            }
        );
    }

    /// No step clause holds the whole packet: every step of a member
    /// moves through it, and a packet of another kind is not there.
    #[test]
    fn a_station_with_no_step_clause_counts_every_step_of_its_packets() {
        let gates = StationPredicate {
            kind: Some("gate-run".into()),
            status: Some(JobStatus::Open),
            ..Default::default()
        };
        let run = packet(
            "gate-run",
            JobStatus::Closed,
            vec![
                stamped("launched", "trigger", Some(1), Some(2)),
                stamped("record-verdict", "task", Some(2), Some(3)),
            ],
        );
        let other = packet(
            "pr-train",
            JobStatus::Open,
            vec![stamped("ci", "task", Some(2), Some(3))],
        );
        let flow = station_window_flow(&gates, &[run, other], window(2, 5));
        assert_eq!(
            flow,
            WindowFlow {
                arrived: 1,
                departed: 2
            }
        );
    }

    /// The window is the caller's: the previous window counts what this
    /// one does not, and an instant outside both counts in neither.
    #[test]
    fn the_window_decides_which_instants_count() {
        let p = packet(
            "backlog-item",
            JobStatus::Open,
            vec![
                stamped("a", "task", Some(1), Some(2)),
                stamped("b", "task", Some(6), Some(7)),
            ],
        );
        let station = live_clause("task");
        let packets = [p];
        assert_eq!(
            station_window_flow(&station, &packets, window(0, 3)),
            WindowFlow {
                arrived: 1,
                departed: 1
            }
        );
        assert_eq!(
            station_window_flow(&station, &packets, window(5, 8)),
            WindowFlow {
                arrived: 1,
                departed: 1
            }
        );
        assert_eq!(
            station_window_flow(&station, &packets, window(3, 5)),
            WindowFlow::default()
        );
    }

    /// A Job-level clause still decides: a packet whose metadata the
    /// station refuses moves through nothing there.
    #[test]
    fn a_job_level_clause_still_refuses_a_packet() {
        let mut station = live_clause("task");
        station.metadata_absent = vec!["proof_probe".into()];
        let mut p = packet(
            "ship-a-change",
            JobStatus::Open,
            vec![stamped("a", "task", Some(1), Some(2))],
        );
        p.job.metadata = serde_json::json!({"proof_probe": "true"});
        assert_eq!(
            station_window_flow(&station, &[p], window(0, 3)),
            WindowFlow::default()
        );
    }
}
