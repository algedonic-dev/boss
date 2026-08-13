//! Station queue evaluation — the pure half of the station registry
//! (stations.md, Q1/Q2 ratified): membership is DERIVED (a station is
//! a predicate over packet state, recomputed at read time — no
//! mutable current-station field), and ordering is a data-declared
//! discipline (`priority, then age` default).
//!
//! Everything in this module is a pure function of `(StationSpec
//! fields, packets)` — no I/O, no clock — so the queue an operator
//! sees is reproducible from the projection alone. The HTTP handler
//! (`http/stations.rs`) supplies the open-Job universe and renders
//! the [`StationQueue`] envelope.

use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Predicate
// ---------------------------------------------------------------------------

/// The queue-membership predicate over packets: a CONJUNCTION of
/// optional clauses (an omitted clause always matches). Deliberately
/// a small documented JSON shape rather than a general expression
/// language: `boss-expr` / `ready_when` predicates bind flat
/// identifiers from a single event payload or a Job's own step set —
/// neither evaluates "does some step of this Job look like X" over a
/// job+steps aggregate, which is the whole vocabulary a station
/// needs (kind / status / step-state / tags / metadata presence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StationPredicate {
    /// Exact match on the Job's Workflow kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Match on the Job's status (kebab-case on the wire). The
    /// evaluation universe is open Jobs, so this only narrows
    /// further; it exists so a predicate can say what it means
    /// instead of relying on the universe's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<JobStatus>,
    /// At least one of these tags is present on the Job.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags_any: Vec<String>,
    /// Metadata keys that must exist and be non-null.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_present: Vec<String>,
    /// Metadata keys that must be missing or null.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_absent: Vec<String>,
    /// Some step of the Job matches every given field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepMatch>,
}

/// The step clause of a [`StationPredicate`]: a Job matches when ANY
/// of its steps satisfies every field given here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StepMatch {
    /// The step's `spec_slug` (the Workflow StepSpec's stable slug).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// The step's kind (StepType registry vocabulary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The step's status is one of these (kebab-case on the wire).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_in: Vec<StepStatus>,
    /// The step is assigned to exactly this executor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
}

impl StepMatch {
    fn matches(&self, step: &Step) -> bool {
        if let Some(slug) = &self.slug
            && step.spec_slug.as_deref() != Some(slug.as_str())
        {
            return false;
        }
        if let Some(kind) = &self.kind
            && &step.kind != kind
        {
            return false;
        }
        if !self.status_in.is_empty() && !self.status_in.contains(&step.status) {
            return false;
        }
        if let Some(assignee) = &self.assignee_id
            && step.assignee_id.as_deref() != Some(assignee.as_str())
        {
            return false;
        }
        true
    }
}

impl StationPredicate {
    /// Whether `job` (with its `steps`) is a member of this station's
    /// queue. Pure; conjunction of every declared clause.
    pub fn matches(&self, job: &Job, steps: &[Step]) -> bool {
        if let Some(kind) = &self.kind
            && &job.kind != kind
        {
            return false;
        }
        if let Some(status) = &self.status
            && &job.status != status
        {
            return false;
        }
        if !self.tags_any.is_empty() && !self.tags_any.iter().any(|t| job.tags.contains(t)) {
            return false;
        }
        for key in &self.metadata_present {
            if job.metadata.get(key).is_none_or(|v| v.is_null()) {
                return false;
            }
        }
        for key in &self.metadata_absent {
            if job.metadata.get(key).is_some_and(|v| !v.is_null()) {
                return false;
            }
        }
        if let Some(step_match) = &self.step
            && !steps.iter().any(|s| step_match.matches(s))
        {
            return false;
        }
        true
    }

    /// Whether evaluating this predicate needs the Job's steps at
    /// all — lets the handler skip the per-job steps fetch for
    /// step-less predicates.
    pub fn needs_steps(&self) -> bool {
        self.step.is_some()
    }
}

// ---------------------------------------------------------------------------
// Discipline
// ---------------------------------------------------------------------------

/// One key of a station's ordering discipline. A discipline is an
/// array of these, applied lexicographically; ties beyond the
/// declared keys break on the job id so the order is deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisciplineKey {
    /// Packet priority, emergency first.
    Priority,
    /// Oldest `opened_on` first.
    Age,
    /// Earliest `due_on` first; undated packets last.
    Due,
}

/// The ratified default: `priority, then age`.
pub fn default_discipline() -> Vec<DisciplineKey> {
    vec![DisciplineKey::Priority, DisciplineKey::Age]
}

/// Priority's queue rank — emergency drains first. Kept here (not an
/// `Ord` on the enum) so the ordering stays a station-discipline
/// concern, not an accidental global.
fn priority_rank(p: Priority) -> u8 {
    match p {
        Priority::Emergency => 0,
        Priority::Urgent => 1,
        Priority::Standard => 2,
        Priority::Scheduled => 3,
    }
}

fn compare_by_key(key: DisciplineKey, a: &Job, b: &Job) -> std::cmp::Ordering {
    match key {
        DisciplineKey::Priority => priority_rank(a.priority).cmp(&priority_rank(b.priority)),
        DisciplineKey::Age => a.opened_on.cmp(&b.opened_on),
        DisciplineKey::Due => match (a.due_on, b.due_on) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        },
    }
}

/// Order `jobs` by the discipline, in place. An empty discipline
/// falls back to the ratified default. Deterministic: after every
/// declared key ties break on the job id's string form.
pub fn order_by_discipline(discipline: &[DisciplineKey], jobs: &mut [Job]) {
    let effective: Vec<DisciplineKey> = if discipline.is_empty() {
        default_discipline()
    } else {
        discipline.to_vec()
    };
    jobs.sort_by(|a, b| {
        for key in &effective {
            let ord = compare_by_key(*key, a, b);
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        a.id.to_string().cmp(&b.id.to_string())
    });
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// The evaluated queue of one station: ordered packets plus the
/// discipline that ordered them (an operator should never wonder why
/// the queue is in this order) and the advisory bandwidth verdict.
#[derive(Debug, Clone, Serialize)]
pub struct StationQueue {
    pub station: String,
    pub kind: crate::stations::StationKind,
    pub discipline: Vec<DisciplineKey>,
    pub wip_limit: Option<i32>,
    /// Advisory (Q3): true when the queue holds more packets than
    /// `wip_limit`. Never enforced here — lenses warn, telemetry
    /// reads it.
    pub over_limit: bool,
    pub total: usize,
    pub data: Vec<Job>,
}

/// Evaluate a station over a packet universe: filter by the
/// predicate, order by the discipline, wrap in the envelope. Pure.
///
/// `packets` is `(job, steps)`; pass an empty step slice when the
/// predicate doesn't need steps ([`StationPredicate::needs_steps`]).
pub fn evaluate_station(
    spec: &crate::stations::StationSpec,
    packets: Vec<(Job, Vec<Step>)>,
) -> StationQueue {
    let mut members: Vec<Job> = packets
        .into_iter()
        .filter(|(job, steps)| spec.predicate.matches(job, steps))
        .map(|(job, _)| job)
        .collect();
    order_by_discipline(&spec.discipline, &mut members);
    let discipline = if spec.discipline.is_empty() {
        default_discipline()
    } else {
        spec.discipline.clone()
    };
    StationQueue {
        station: spec.name.clone(),
        kind: spec.kind,
        discipline,
        wip_limit: spec.wip_limit,
        over_limit: spec
            .wip_limit
            .is_some_and(|limit| members.len() as i64 > i64::from(limit)),
        total: members.len(),
        data: members,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stations::{StationKind, StationSpec};
    use boss_core::job::{JobId, Subject};
    use chrono::NaiveDate;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, d).unwrap()
    }

    fn job(kind: &str, priority: Priority, opened: u32) -> Job {
        let mut j = Job::new(
            kind,
            Subject::new("custom", "x"),
            "t",
            "emp-1",
            priority,
            day(opened),
        );
        j.status = JobStatus::Open;
        j
    }

    fn step(slug: &str, status: StepStatus) -> Step {
        let mut s = Step::new(JobId::new(), "task", slug, 0);
        s.spec_slug = Some(slug.to_string());
        s.status = status;
        s
    }

    // ------------------------------------------------------------
    // Predicate matching
    // ------------------------------------------------------------

    #[test]
    fn empty_predicate_matches_everything() {
        let p = StationPredicate::default();
        assert!(p.matches(&job("anything", Priority::Standard, 1), &[]));
        assert!(!p.needs_steps());
    }

    #[test]
    fn kind_and_status_clauses_conjoin() {
        let p = StationPredicate {
            kind: Some("ship-a-change".into()),
            status: Some(JobStatus::Open),
            ..Default::default()
        };
        assert!(p.matches(&job("ship-a-change", Priority::Standard, 1), &[]));
        assert!(!p.matches(&job("pr-train", Priority::Standard, 1), &[]));
        let mut closed = job("ship-a-change", Priority::Standard, 1);
        closed.status = JobStatus::Closed;
        assert!(!p.matches(&closed, &[]));
    }

    #[test]
    fn tags_any_matches_any_declared_tag() {
        let p = StationPredicate {
            tags_any: vec!["hotfix".into(), "urgent-lane".into()],
            ..Default::default()
        };
        let tagged = job("k", Priority::Standard, 1).with_tags(vec!["urgent-lane".into()]);
        assert!(p.matches(&tagged, &[]));
        let other = job("k", Priority::Standard, 1).with_tags(vec!["routine".into()]);
        assert!(!p.matches(&other, &[]));
        let untagged = job("k", Priority::Standard, 1);
        assert!(!p.matches(&untagged, &[]));
    }

    #[test]
    fn metadata_presence_and_absence() {
        // The loading-dock shape: branch present, train absent.
        let p = StationPredicate {
            metadata_present: vec!["branch".into()],
            metadata_absent: vec!["train".into()],
            ..Default::default()
        };
        let parked =
            job("k", Priority::Standard, 1).with_metadata(serde_json::json!({"branch": "feat/a"}));
        assert!(p.matches(&parked, &[]));
        let boarded = job("k", Priority::Standard, 1)
            .with_metadata(serde_json::json!({"branch": "feat/b", "train": "t1"}));
        assert!(!p.matches(&boarded, &[]));
        let branchless = job("k", Priority::Standard, 1);
        assert!(!p.matches(&branchless, &[]));
        // A null value counts as absent for `present` and as absent
        // for `absent` — null is not a value.
        let null_branch =
            job("k", Priority::Standard, 1).with_metadata(serde_json::json!({"branch": null}));
        assert!(!p.matches(&null_branch, &[]));
        let null_train = job("k", Priority::Standard, 1)
            .with_metadata(serde_json::json!({"branch": "feat/c", "train": null}));
        assert!(p.matches(&null_train, &[]));
    }

    #[test]
    fn step_clause_needs_some_step_matching_every_field() {
        let p = StationPredicate {
            step: Some(StepMatch {
                slug: Some("review".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(p.needs_steps());
        let j = job("k", Priority::Standard, 1);
        assert!(p.matches(&j, &[step("review", StepStatus::Ready)]));
        assert!(p.matches(
            &j,
            &[
                step("pr", StepStatus::Ready),
                step("review", StepStatus::Active)
            ]
        ));
        // Wrong status, wrong slug, or no steps at all: not a member.
        assert!(!p.matches(&j, &[step("review", StepStatus::Completed)]));
        assert!(!p.matches(&j, &[step("pr", StepStatus::Ready)]));
        assert!(!p.matches(&j, &[]));
    }

    #[test]
    fn step_clause_matches_assignee() {
        let p = StationPredicate {
            step: Some(StepMatch {
                assignee_id: Some("emp-7".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut mine = step("work", StepStatus::Active);
        mine.assignee_id = Some("emp-7".into());
        assert!(p.matches(&job("k", Priority::Standard, 1), &[mine]));
        let mut theirs = step("work", StepStatus::Active);
        theirs.assignee_id = Some("emp-9".into());
        assert!(!p.matches(&job("k", Priority::Standard, 1), &[theirs]));
    }

    #[test]
    fn unknown_predicate_fields_are_rejected_not_ignored() {
        // deny_unknown_fields: a typo'd clause must fail loudly at
        // parse time, not silently match everything.
        let bad = serde_json::json!({"kinds": "ship-a-change"});
        assert!(serde_json::from_value::<StationPredicate>(bad).is_err());
    }

    // ------------------------------------------------------------
    // Discipline ordering
    // ------------------------------------------------------------

    #[test]
    fn default_discipline_is_priority_then_age() {
        let mut jobs = vec![
            job("k", Priority::Standard, 1), // old but standard
            job("k", Priority::Urgent, 5),   // newer urgent
            job("k", Priority::Urgent, 2),   // older urgent
            job("k", Priority::Emergency, 9),
        ];
        order_by_discipline(&default_discipline(), &mut jobs);
        let got: Vec<(Priority, NaiveDate)> =
            jobs.iter().map(|j| (j.priority, j.opened_on)).collect();
        assert_eq!(
            got,
            vec![
                (Priority::Emergency, day(9)),
                (Priority::Urgent, day(2)),
                (Priority::Urgent, day(5)),
                (Priority::Standard, day(1)),
            ]
        );
    }

    #[test]
    fn empty_discipline_falls_back_to_the_default() {
        let mut jobs = vec![
            job("k", Priority::Standard, 1),
            job("k", Priority::Emergency, 2),
        ];
        order_by_discipline(&[], &mut jobs);
        assert_eq!(jobs[0].priority, Priority::Emergency);
    }

    #[test]
    fn due_discipline_puts_undated_last() {
        let mut a = job("k", Priority::Standard, 1);
        a.due_on = Some(day(20));
        let mut b = job("k", Priority::Standard, 1);
        b.due_on = Some(day(12));
        let c = job("k", Priority::Standard, 1); // undated
        let mut jobs = vec![c.clone(), a.clone(), b.clone()];
        order_by_discipline(&[DisciplineKey::Due], &mut jobs);
        assert_eq!(
            jobs.iter().map(|j| j.due_on).collect::<Vec<_>>(),
            vec![Some(day(12)), Some(day(20)), None]
        );
    }

    #[test]
    fn full_ties_break_on_job_id_for_determinism() {
        let a = job("k", Priority::Standard, 1);
        let b = job("k", Priority::Standard, 1);
        let mut forward = vec![a.clone(), b.clone()];
        let mut backward = vec![b, a];
        order_by_discipline(&default_discipline(), &mut forward);
        order_by_discipline(&default_discipline(), &mut backward);
        let f: Vec<String> = forward.iter().map(|j| j.id.to_string()).collect();
        let r: Vec<String> = backward.iter().map(|j| j.id.to_string()).collect();
        assert_eq!(f, r, "same set, same order, whatever the input order");
    }

    #[test]
    fn discipline_keys_are_kebab_on_the_wire() {
        let keys: Vec<DisciplineKey> =
            serde_json::from_value(serde_json::json!(["priority", "age", "due"])).unwrap();
        assert_eq!(
            keys,
            vec![
                DisciplineKey::Priority,
                DisciplineKey::Age,
                DisciplineKey::Due
            ]
        );
    }

    // ------------------------------------------------------------
    // Envelope
    // ------------------------------------------------------------

    fn dock_spec(wip_limit: Option<i32>) -> StationSpec {
        let mut s = StationSpec::draft(
            "loading-dock",
            "Loading dock",
            StationKind::Batch,
            StationPredicate {
                kind: Some("ship-a-change".into()),
                ..Default::default()
            },
        );
        s.wip_limit = wip_limit;
        s
    }

    #[test]
    fn evaluate_filters_orders_and_reports_the_discipline() {
        let spec = dock_spec(None);
        let packets = vec![
            (job("ship-a-change", Priority::Standard, 3), vec![]),
            (job("pr-train", Priority::Emergency, 1), vec![]),
            (job("ship-a-change", Priority::Urgent, 5), vec![]),
        ];
        let q = evaluate_station(&spec, packets);
        assert_eq!(q.station, "loading-dock");
        assert_eq!(q.total, 2, "the pr-train packet is not a member");
        assert_eq!(q.data[0].priority, Priority::Urgent);
        assert_eq!(q.discipline, default_discipline());
        assert!(!q.over_limit, "no wip_limit declared -> never over");
    }

    #[test]
    fn wip_limit_is_advisory_over_limit_flag() {
        let spec = dock_spec(Some(1));
        let packets = vec![
            (job("ship-a-change", Priority::Standard, 1), vec![]),
            (job("ship-a-change", Priority::Standard, 2), vec![]),
        ];
        let q = evaluate_station(&spec, packets);
        assert_eq!(q.total, 2, "advisory: nothing is dropped");
        assert!(q.over_limit);

        let under = evaluate_station(
            &dock_spec(Some(5)),
            vec![(job("ship-a-change", Priority::Standard, 1), vec![])],
        );
        assert!(!under.over_limit);
    }
}
