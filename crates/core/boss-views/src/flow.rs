//! Flow — how much work the IT team is getting through, in real time.
//!
//! Every other instrument on the IT app reports the *system*: what the
//! machine is doing under load. This reports the *team*: what was
//! filed, what got routed, what closed, and how long a person waited
//! for an answer.
//!
//! ## Why this reads `audit_log` and not `event_facts`
//!
//! Every other view reads `event_facts`, which is indexed for exactly
//! this access pattern. Flow cannot, and the reason is the whole
//! design of this module.
//!
//! Event time in BOSS is clock-authoritative, and on a demo
//! deployment the authoritative clock is the simulator's. So a real
//! person filing feedback at 00:04 on a Friday gets an event stamped
//! `2025-07-24` — a date in the simulated brewery's calendar. The
//! `jobs` and `steps` projections inherit that stamp, and
//! `event_facts.occurred_at` carries it too. There is no wall clock
//! anywhere in the projection tier.
//!
//! That makes elapsed time unrecoverable from the usual sources: the
//! epoch runs 366 sim-days in about nine real hours, so sim time moves
//! roughly a thousand times faster than the wall, and a ten-minute
//! triage reads as a week. Measuring team responsiveness on those
//! stamps would not be approximate, it would be nonsense.
//!
//! `audit_log` keeps both: `timestamp` is the authoritative (sim)
//! time, and `created_at` is when the row was actually written.
//! `created_at` is a plain `now()` default that nothing overwrites,
//! and the epoch trim DELETEs simulated rows rather than rewriting
//! surviving ones, so a real Job's wall-clock history is intact across
//! laps. That column is the only honest source for "how long did
//! someone wait", so this module reads the log.
//!
//! ## What counts as the team's work
//!
//! JobKinds that declare an `owner_role` this app owns — read from the
//! registry, never a list in code. Adding an IT workflow to the
//! registry puts it on this page with no change here (CLAUDE.md §9).
//!
//! It also does the right thing by accident, which is worth saying out
//! loud because it is load-bearing: a dispatcher bug marked 85
//! simulated restock Jobs as real, and they are permanent until a
//! regen. They carry no `owner_role`, so they never enter this view.
//! Selecting "not simulated" alone would have silently folded a
//! brewery's restocking into the IT team's throughput.
//!
//! ## Why the rows are raw
//!
//! This returns one row per step, not a computed disposition. Which
//! step carries the decision is a registry question — the fork step is
//! the one bearing the JobKind's enum field — and that rule already
//! exists once, in the client (`apps/web/src/jobs/fork.ts`). It has
//! drifted before, between the board and the terminal queue reader,
//! and reporting a freshly filed item as already triaged is the worst
//! way for a queue reader to be wrong. So the server stays dumb: it
//! reports what happened and when, and the one place that knows how to
//! read a fork reads it. CLAUDE.md §9a.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::ViewsError;

/// A step's recorded history on a team Job, in wall-clock time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowStep {
    pub step_id: String,
    pub status: String,
    /// The step's own metadata — carries the disposition when this is
    /// the fork step. The client decides which step that is.
    pub metadata: serde_json::Value,
    /// Field names declared on the step, so the client can find the
    /// fork by the field it bears rather than by step kind.
    pub field_names: Vec<String>,
    /// When this step was last written, wall clock. `None` if the step
    /// has no event in the surviving log.
    pub last_written_at: Option<String>,
}

/// One Job the team owns, with the wall-clock timeline of its work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowJob {
    pub job_id: String,
    pub kind: String,
    pub title: String,
    pub status: String,
    /// The role the JobKind names as owner — why this Job is here.
    pub owner_role: String,
    /// Wall clock, from the create event's `created_at`.
    pub filed_at: Option<String>,
    /// Wall clock of the newest event on the Job — when the team last
    /// touched it.
    pub last_activity_at: Option<String>,
    pub steps: Vec<FlowStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flow {
    /// The roles whose JobKinds are counted, so the surface can say
    /// whose throughput this is rather than asserting it.
    pub owner_roles: Vec<String>,
    /// JobKinds included, for the same reason.
    pub kinds: Vec<String>,
    pub jobs: Vec<FlowJob>,
    /// Wall-clock instant the read was taken, so a client can compute
    /// "open for N hours" against the server's clock rather than the
    /// browser's.
    pub as_of: String,
}

/// Read the team's flow.
#[async_trait]
pub trait FlowRepo: Send + Sync {
    /// Jobs of every kind whose JobKind declares one of `owner_roles`.
    /// Simulated Jobs are excluded: this measures a team of people.
    async fn flow(&self, owner_roles: &[String], limit: i64) -> Result<Flow, ViewsError>;
}

/// Enough Jobs to cover a long backlog without an unbounded scan. The
/// team's queue is small by nature; if this ever truncates, the count
/// is visible in `jobs.len()` against the limit rather than silent.
pub const DEFAULT_LIMIT: i64 = 2_000;

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape is what the page is built on, so a rename here
    /// is a breaking change and should read like one.
    #[test]
    fn flow_serialises_with_the_field_names_the_page_reads() {
        let flow = Flow {
            owner_roles: vec!["platform-admin".into()],
            kinds: vec!["user-feedback".into()],
            as_of: "2026-08-07T15:42:00Z".into(),
            jobs: vec![FlowJob {
                job_id: "j1".into(),
                kind: "user-feedback".into(),
                title: "Feedback on /system/feedback".into(),
                status: "open".into(),
                owner_role: "platform-admin".into(),
                filed_at: Some("2026-08-07T00:04:50Z".into()),
                last_activity_at: Some("2026-08-07T05:18:14Z".into()),
                steps: vec![FlowStep {
                    step_id: "s1".into(),
                    status: "completed".into(),
                    metadata: serde_json::json!({ "disposition": "reproduce" }),
                    field_names: vec!["disposition".into()],
                    last_written_at: Some("2026-08-07T05:18:14Z".into()),
                }],
            }],
        };
        let v = serde_json::to_value(&flow).expect("serialises");
        assert_eq!(v["jobs"][0]["filed_at"], "2026-08-07T00:04:50Z");
        assert_eq!(v["jobs"][0]["steps"][0]["field_names"][0], "disposition");
        assert_eq!(v["owner_roles"][0], "platform-admin");
    }
}
