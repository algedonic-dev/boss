//! Stage durations — how long each step of a Workflow kind actually
//! takes, wall-clock (backlog `a5096c8f`; the department-dashboards
//! doc's per-hop latency, served).
//!
//! Duration is `step.done.*` minus `step.ready.*` per step, from
//! `audit_log.created_at` — never the sim-authoritative `timestamp`
//! (flow.rs owns the doctrine and the incident). Grouped by
//! COALESCE(spec_slug, title), the fleet key, so latencies land on
//! the same nodes the depth badges do. Completed hops only: a step
//! still waiting has an age (the fleet's oldest-wait), not a
//! duration.

use async_trait::async_trait;
use serde::Serialize;

use crate::error::ViewsError;

#[derive(Debug, Clone, Serialize)]
pub struct Stage {
    pub slug: String,
    /// Completed hops inside the window.
    pub completed: i64,
    pub p50_seconds: f64,
    pub p90_seconds: f64,
    pub max_seconds: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageDurations {
    pub workflow_kind: String,
    pub window_days: i64,
    pub stages: Vec<Stage>,
    /// Wall-clock read instant (ISO-8601 UTC).
    pub as_of: String,
}

/// One step of one run: duration when the hop completed, None while
/// it is still waiting (an age belongs to the fleet view, not here).
#[derive(Debug, Clone, Serialize)]
pub struct RunStage {
    pub slug: String,
    pub seconds: Option<f64>,
}

/// One Job of the kind, its stages in spec order — a row of the
/// "last N runs" table (backlog `a5096c8f`: even a table of the last
/// N trains with their stage durations answers "where does a change
/// wait longest").
#[derive(Debug, Clone, Serialize)]
pub struct StageRun {
    pub job_id: String,
    pub title: String,
    /// Wall-clock creation instant (`jobs.created_at`, ISO-8601 UTC) —
    /// NOT the sim-calendar `opened_on`.
    pub created_at: String,
    pub status: String,
    pub stages: Vec<RunStage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageRuns {
    pub workflow_kind: String,
    pub limit: i64,
    /// Newest first.
    pub runs: Vec<StageRun>,
    pub as_of: String,
}

#[async_trait]
pub trait StageDurationsRepo: Send + Sync {
    async fn stage_durations(
        &self,
        workflow_kind: &str,
        window_days: i64,
    ) -> Result<StageDurations, ViewsError>;

    /// The last `limit` Jobs of the kind with per-step durations —
    /// the per-run rows the aggregate above summarises.
    async fn stage_runs(&self, workflow_kind: &str, limit: i64) -> Result<StageRuns, ViewsError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape a dashboard is built on — a rename is breaking.
    #[test]
    fn stage_durations_serialise_with_the_field_names_the_page_reads() {
        let out = StageDurations {
            workflow_kind: "pr-train".into(),
            window_days: 7,
            stages: vec![Stage {
                slug: "ci".into(),
                completed: 2,
                p50_seconds: 900.0,
                p90_seconds: 2400.0,
                max_seconds: 2400.0,
            }],
            as_of: "2026-08-09T05:40:00Z".into(),
        };
        let v = serde_json::to_value(&out).expect("serialises");
        assert_eq!(v["workflow_kind"], "pr-train");
        assert_eq!(v["window_days"], 7);
        assert_eq!(v["stages"][0]["slug"], "ci");
        assert_eq!(v["stages"][0]["completed"], 2);
        assert_eq!(v["stages"][0]["p50_seconds"], 900.0);
    }

    /// The per-run wire shape: a still-waiting hop serialises as
    /// `"seconds": null`, not as an absent key — the table renders the
    /// column either way and a dropped key would read as "no stage".
    #[test]
    fn stage_runs_serialise_with_null_for_a_waiting_hop() {
        let out = StageRuns {
            workflow_kind: "pr-train".into(),
            limit: 10,
            runs: vec![StageRun {
                job_id: "j1".into(),
                title: "PR train 2026-08-09 PM".into(),
                created_at: "2026-08-09T20:26:30Z".into(),
                status: "open".into(),
                stages: vec![
                    RunStage {
                        slug: "ci".into(),
                        seconds: Some(900.0),
                    },
                    RunStage {
                        slug: "merged".into(),
                        seconds: None,
                    },
                ],
            }],
            as_of: "2026-08-09T21:00:00Z".into(),
        };
        let v = serde_json::to_value(&out).expect("serialises");
        assert_eq!(v["runs"][0]["stages"][0]["seconds"], 900.0);
        assert!(v["runs"][0]["stages"][1]["seconds"].is_null());
        assert_eq!(v["runs"][0]["created_at"], "2026-08-09T20:26:30Z");
    }
}
