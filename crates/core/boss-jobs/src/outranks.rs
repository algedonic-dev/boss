//! WHAT OUTRANKS REGULAR ORDER — the top board's first row (backlog
//! 74569e94, design ea906603 Q1, decided 2026-09-27).
//!
//! David, 2026-09-26: show urgent jobs on the top board. The data was
//! on every job row (measured that morning: 17 of 460 open packets were
//! urgent, every one a backlog-item) and no read answered it — `GET
//! /api/jobs` has no priority parameter, and the HUD is one read by
//! design. So it is a field on `GET /api/yard/regions`, computed here
//! once, and `boss orient` prints the same payload.
//!
//! WHICH PRIORITIES OUTRANK is the station discipline's own rank
//! ([`crate::station_queue::priority_rank`]): a priority that drains
//! before `standard` outranks regular order — `emergency` and `urgent`.
//! `scheduled` ranks AFTER standard, so it is not on the board although
//! it is "not standard": a packet scheduled for later does not jump
//! the queue, and a board that said it did would be the queue
//! disagreeing with itself.
//!
//! Pure: the handler reads the open packets of those priorities (real
//! partition only — a simulated emergency is the sim's, not the
//! operator's) with their steps, and the stations they stand at come
//! from the same pass the marshalling region is judged on.

use boss_core::job::{Job, Priority, Step, StepStatus};
use serde::{Deserialize, Serialize};

use crate::regions::StationReading;

/// Every priority, in the station discipline's order. The rank is an
/// exhaustive match, so a new variant stops the build there; this list
/// is held to it by a test.
const PRIORITIES: [Priority; 4] = [
    Priority::Emergency,
    Priority::Urgent,
    Priority::Standard,
    Priority::Scheduled,
];

/// Whether a packet of this priority drains before regular order.
pub fn outranks_regular_order(p: Priority) -> bool {
    crate::station_queue::priority_rank(p) < crate::station_queue::priority_rank(Priority::Standard)
}

/// The priorities the board lists — the ones the handler narrows its
/// read to, one query each.
pub fn outranking_priorities() -> Vec<Priority> {
    PRIORITIES
        .into_iter()
        .filter(|p| outranks_regular_order(*p))
        .collect()
}

/// Where a packet stands: its first ready-or-active step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct At {
    /// The step's spec slug, where the step has one.
    pub slug: Option<String>,
    pub title: String,
    pub status: StepStatus,
    /// Who holds it, when anyone does.
    pub assignee_id: Option<String>,
}

/// One packet that outranks regular order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outrank {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub priority: Priority,
    /// The admission instant: the `opened_at` column, else the
    /// `opened_at` metadata stamp, else `opened_on` at midnight —
    /// coarser, and still inside the right day.
    pub opened_at: chrono::DateTime<chrono::Utc>,
    /// Whole minutes from `opened_at` to the server's `now`, so every
    /// reader states one age rather than each clock its own.
    pub age_minutes: i64,
    /// Its first ready-or-active step, or `None` when no step is
    /// workable — a packet between steps says so rather than naming a
    /// step it is not at.
    pub at: Option<At>,
    /// The marshalling stations it stands at, by registry name.
    /// `None` when the stations could not be read — never an empty
    /// list, which would say it stands nowhere.
    pub stations: Option<Vec<String>>,
}

/// The board's row: every OPEN packet in `packets` whose priority
/// outranks regular order, oldest first (ties by id, so two reads of
/// one yard answer in one order). A packet of another priority or
/// status is dropped here too, so the answer never rests on the
/// handler's query alone.
pub fn outranks(
    packets: &[(Job, Vec<Step>)],
    stations: Option<&[StationReading]>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Outrank> {
    let mut board: Vec<Outrank> = packets
        .iter()
        .filter(|(job, _)| {
            job.status == boss_core::job::JobStatus::Open && outranks_regular_order(job.priority)
        })
        .map(|(job, steps)| {
            let opened_at = admitted_at(job);
            let id = job.id.to_string();
            Outrank {
                stations: stations.map(|all| {
                    all.iter()
                        .filter(|s| s.members.contains(&id))
                        .map(|s| s.name.clone())
                        .collect()
                }),
                id,
                kind: job.kind.clone(),
                title: job.title.clone(),
                priority: job.priority,
                opened_at,
                age_minutes: (now - opened_at).num_minutes(),
                at: at(steps),
            }
        })
        .collect();
    board.sort_by(|a, b| a.opened_at.cmp(&b.opened_at).then_with(|| a.id.cmp(&b.id)));
    board
}

/// The admission instant, from the finest source the row carries.
fn admitted_at(job: &Job) -> chrono::DateTime<chrono::Utc> {
    job.opened_at
        .or_else(|| crate::regions::opened_at(job))
        .unwrap_or_else(|| {
            chrono::DateTime::from_naive_utc_and_offset(
                job.opened_on.and_time(chrono::NaiveTime::MIN),
                chrono::Utc,
            )
        })
}

/// The first ready-or-active step by the protocol's order.
fn at(steps: &[Step]) -> Option<At> {
    steps
        .iter()
        .filter(|s| matches!(s.status, StepStatus::Ready | StepStatus::Active))
        .min_by_key(|s| s.sort_order)
        .map(|s| At {
            slug: s.spec_slug.clone(),
            title: s.title.clone(),
            status: s.status,
            assignee_id: s.assignee_id.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{JobId, JobStatus, StepId, Subject};
    use serde_json::json;

    fn t(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    const NOW: &str = "2026-09-27T12:00:00Z";

    fn job(title: &str, priority: Priority, opened_at: Option<&str>) -> Job {
        Job {
            id: JobId::new(),
            kind: "backlog-item".into(),
            workflow_version: 1,
            subject: Subject::new("custom", "s"),
            title: title.into(),
            owner_id: "emp-david".into(),
            status: JobStatus::Open,
            priority,
            opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
            opened_at: opened_at.map(t),
            due_on: None,
            closed_on: None,
            metadata: json!({}),
            tags: vec![],
            partition: boss_core::partition::Partition::Real,
        }
    }

    fn step(job: &Job, slug: &str, order: i32, status: StepStatus) -> Step {
        let mut s = Step::new(job.id, "task", slug, order);
        s.id = StepId::new();
        s.spec_slug = Some(slug.into());
        s.title = format!("Do {slug}");
        s.status = status;
        s
    }

    #[test]
    fn emergency_and_urgent_outrank_regular_order_and_scheduled_does_not() {
        assert_eq!(
            outranking_priorities(),
            vec![Priority::Emergency, Priority::Urgent]
        );
        assert!(!outranks_regular_order(Priority::Standard));
        assert!(!outranks_regular_order(Priority::Scheduled));
    }

    /// The variant list is held to the rank's exhaustive match: every
    /// variant appears once, in rank order.
    #[test]
    fn the_priority_list_is_every_variant_in_rank_order() {
        let ranks: Vec<u8> = PRIORITIES
            .iter()
            .map(|p| crate::station_queue::priority_rank(*p))
            .collect();
        assert_eq!(ranks, vec![0, 1, 2, 3]);
    }

    #[test]
    fn the_board_lists_only_what_outranks_oldest_first_with_where_it_stands() {
        let old = job(
            "the old urgent",
            Priority::Urgent,
            Some("2026-09-25T09:00:00Z"),
        );
        let fire = job(
            "the fire",
            Priority::Emergency,
            Some("2026-09-27T11:00:00Z"),
        );
        let routine = job("routine", Priority::Standard, Some("2026-09-20T09:00:00Z"));
        let later = job("later", Priority::Scheduled, Some("2026-09-20T09:00:00Z"));
        let mut closed = job("done", Priority::Urgent, Some("2026-09-19T09:00:00Z"));
        closed.status = JobStatus::Closed;
        let mut held = step(&old, "build", 2, StepStatus::Active);
        held.assignee_id = Some("agent-claude".into());
        let packets = vec![
            (
                fire.clone(),
                vec![step(&fire, "triage", 1, StepStatus::Ready)],
            ),
            (routine.clone(), vec![]),
            (later.clone(), vec![]),
            (closed.clone(), vec![]),
            (
                old.clone(),
                vec![
                    step(&old, "filed", 0, StepStatus::Completed),
                    held,
                    step(&old, "proven", 3, StepStatus::Pending),
                ],
            ),
        ];
        let stations = vec![StationReading {
            name: "backlog".into(),
            members: vec![old.id.to_string(), routine.id.to_string()],
            ..Default::default()
        }];
        let board = outranks(&packets, Some(&stations), t(NOW));

        let titles: Vec<&str> = board.iter().map(|o| o.title.as_str()).collect();
        assert_eq!(titles, ["the old urgent", "the fire"], "{board:?}");

        let o = &board[0];
        assert_eq!(o.kind, "backlog-item");
        assert_eq!(o.priority, Priority::Urgent);
        assert_eq!(o.age_minutes, 2 * 24 * 60 + 3 * 60);
        let at = o.at.as_ref().expect("a workable step");
        assert_eq!(at.slug.as_deref(), Some("build"));
        assert_eq!(at.title, "Do build");
        assert_eq!(at.status, StepStatus::Active);
        assert_eq!(at.assignee_id.as_deref(), Some("agent-claude"));
        assert_eq!(o.stations.as_deref(), Some(&["backlog".to_string()][..]));

        assert_eq!(board[1].age_minutes, 60);
        assert_eq!(board[1].stations.as_deref(), Some(&[][..]));
    }

    /// A packet with no workable step says so, and stations nobody
    /// could read are unread — never "stands nowhere".
    #[test]
    fn no_workable_step_is_none_and_unread_stations_are_null() {
        let j = job("waiting", Priority::Urgent, None);
        let board = outranks(
            &[(j.clone(), vec![step(&j, "later", 0, StepStatus::Pending)])],
            None,
            t(NOW),
        );
        assert_eq!(board.len(), 1);
        assert_eq!(board[0].at, None);
        assert_eq!(board[0].stations, None);
        // No instant on the row: the admission day at midnight.
        assert_eq!(board[0].opened_at, t("2026-09-25T00:00:00Z"));
        let wire = serde_json::to_value(&board[0]).unwrap();
        assert_eq!(wire["stations"], serde_json::Value::Null);
        assert_eq!(wire["priority"], json!("urgent"));
    }

    /// With nothing outranking, the board is an empty list — the
    /// answer, read — which a reader says in words.
    #[test]
    fn nothing_outranking_is_an_empty_list() {
        let j = job("routine", Priority::Standard, None);
        assert!(outranks(&[(j, vec![])], Some(&[]), t(NOW)).is_empty());
    }
}
