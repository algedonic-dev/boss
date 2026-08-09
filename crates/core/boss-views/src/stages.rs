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

#[async_trait]
pub trait StageDurationsRepo: Send + Sync {
    async fn stage_durations(
        &self,
        workflow_kind: &str,
        window_days: i64,
    ) -> Result<StageDurations, ViewsError>;
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
}
