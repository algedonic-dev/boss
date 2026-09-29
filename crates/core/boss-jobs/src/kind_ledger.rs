//! Every kind's packet ledger — the pure half of `GET /api/jobs/kinds`
//! (backlogs 112c0535 and 5eacf6db, page audit 9da74410, 2026-09-27).
//!
//! Per kind: every packet ever, the open ones by the workflow version
//! each is pinned to, and the newest terminal. /it/registry reads it to
//! say two things it could not: how many in-flight packets run a version
//! below the kind's active one (374 of 540 when measured), and which
//! kinds have never had a packet at all (16 of 64) — a kind absent from
//! the ledger has none the caller may read.
//!
//! The ledger is a function of the packets, so an adapter that answers
//! from its own SQL is checkable against [`from_jobs`] over the rows
//! `list_jobs` hands the same scope — one definition of the fold, which
//! [`fold`] is, and every adapter reaches it.

use std::collections::BTreeMap;

use boss_core::job::{Job, JobStatus};
use serde::Serialize;

use crate::department::readiness::NewestTerminal;

/// One kind's row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KindLedger {
    pub kind: String,
    /// Every packet of the kind, any status.
    pub packets: i64,
    /// Packets still open — the sum of `open_by_version`.
    pub open: i64,
    /// Open packets by the `workflow_version` each is pinned to. A
    /// version with none open is absent, not zero.
    pub open_by_version: BTreeMap<i32, i64>,
    /// The most recently closed packet, as the department readiness
    /// read reports it; `None` when no packet of the kind has closed.
    pub newest_terminal: Option<NewestTerminal>,
}

/// How many packets of `kind` pinned to `version` are open (`open`),
/// or at any other status (`!open`). The ledger needs no finer split:
/// packets ever counts every status, and only open ones are in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionCount {
    pub kind: String,
    pub version: i32,
    pub open: bool,
    pub count: i64,
}

/// The ledger from count rows and the newest closed packet of each
/// kind, sorted by kind. A kind appears when a count row names it; a
/// newest-closed packet whose kind no count names is a contradiction
/// between the two reads and is dropped rather than invented a count.
pub fn fold(counts: &[VersionCount], newest_closed: &[Job]) -> Vec<KindLedger> {
    let mut rows: BTreeMap<String, KindLedger> = BTreeMap::new();
    for c in counts {
        let row = rows.entry(c.kind.clone()).or_insert_with(|| KindLedger {
            kind: c.kind.clone(),
            packets: 0,
            open: 0,
            open_by_version: BTreeMap::new(),
            newest_terminal: None,
        });
        row.packets += c.count;
        if c.open {
            row.open += c.count;
            *row.open_by_version.entry(c.version).or_insert(0) += c.count;
        }
    }
    for job in newest_closed {
        if let Some(row) = rows.get_mut(&job.kind) {
            row.newest_terminal = Some(NewestTerminal::of(job));
        }
    }
    rows.into_values().collect()
}

/// The ledger of `jobs` — the definition every adapter's answer is held
/// to. The newest terminal is [`crate::port::newest_closed_from_jobs`]'s
/// choice, the same one `newest_closed_job` makes for one kind.
pub fn from_jobs(jobs: &[Job]) -> Vec<KindLedger> {
    let mut counts: BTreeMap<(String, i32, bool), i64> = BTreeMap::new();
    let mut closed: BTreeMap<String, Vec<Job>> = BTreeMap::new();
    for j in jobs {
        *counts
            .entry((
                j.kind.clone(),
                j.workflow_version,
                j.status == JobStatus::Open,
            ))
            .or_insert(0) += 1;
        if j.status == JobStatus::Closed {
            closed.entry(j.kind.clone()).or_default().push(j.clone());
        }
    }
    let counts: Vec<VersionCount> = counts
        .into_iter()
        .map(|((kind, version, open), count)| VersionCount {
            kind,
            version,
            open,
            count,
        })
        .collect();
    let newest: Vec<Job> = closed
        .into_values()
        .filter_map(crate::port::newest_closed_from_jobs)
        .collect();
    fold(&counts, &newest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{JobId, Priority, Subject};
    use chrono::NaiveDate;

    fn job(kind: &str, version: i32, status: JobStatus, closed_day: Option<u32>) -> Job {
        Job {
            id: JobId::new(),
            kind: kind.into(),
            workflow_version: version,
            subject: Subject::new("custom", "x"),
            title: format!("{kind} v{version}"),
            owner_id: "emp-a".into(),
            status,
            priority: Priority::Standard,
            opened_on: NaiveDate::from_ymd_opt(2026, 9, 1).expect("day"),
            opened_at: None,
            due_on: None,
            closed_on: closed_day.map(|d| NaiveDate::from_ymd_opt(2026, 9, d).expect("day")),
            metadata: serde_json::Value::Null,
            tags: vec![],
            partition: boss_core::partition::Partition::Real,
        }
    }

    #[test]
    fn open_packets_are_counted_by_the_version_they_are_pinned_to() {
        let jobs = [
            job("backlog-item", 11, JobStatus::Open, None),
            job("backlog-item", 12, JobStatus::Open, None),
            job("backlog-item", 12, JobStatus::Open, None),
            job("backlog-item", 13, JobStatus::Open, None),
            job("backlog-item", 12, JobStatus::Closed, Some(3)),
            job("backlog-item", 13, JobStatus::Cancelled, None),
        ];
        let rows = from_jobs(&jobs);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.packets, 6, "every status counts toward packets ever");
        assert_eq!(r.open, 4);
        assert_eq!(
            r.open_by_version,
            BTreeMap::from([(11, 1), (12, 2), (13, 1)]),
            "closed and cancelled packets are not in flight"
        );
    }

    #[test]
    fn the_newest_terminal_is_the_latest_closed_and_cancelled_is_not_one() {
        let older = job("sale", 1, JobStatus::Closed, Some(2));
        let newer = job("sale", 1, JobStatus::Closed, Some(9));
        let cancelled = job("sale", 1, JobStatus::Cancelled, None);
        let rows = from_jobs(&[older, newer.clone(), cancelled]);
        let t = rows[0].newest_terminal.as_ref().expect("a terminal");
        assert_eq!(t.id, newer.id.to_string());
    }

    #[test]
    fn a_kind_with_only_open_packets_has_no_terminal_and_rows_sort_by_kind() {
        let rows = from_jobs(&[
            job("zeta", 1, JobStatus::Open, None),
            job("alpha", 2, JobStatus::Closed, Some(1)),
        ]);
        let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds, ["alpha", "zeta"]);
        assert!(rows[1].newest_terminal.is_none());
        assert!(
            rows[0].open_by_version.is_empty(),
            "nothing open, no version"
        );
    }

    #[test]
    fn no_packets_is_an_empty_ledger() {
        assert!(from_jobs(&[]).is_empty());
    }

    #[test]
    fn a_terminal_for_a_kind_no_count_names_is_not_invented_a_row() {
        let stray = job("ghost", 1, JobStatus::Closed, Some(1));
        assert!(fold(&[], &[stray]).is_empty());
    }
}
