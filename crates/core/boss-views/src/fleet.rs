//! Fleet — every in-flight Job of a Workflow kind, projected onto the
//! Workflow's step shape.
//!
//! The Job page answers "where is *this* Job"; this view answers
//! "where is *everything* of this kind" — per-step depth (ready /
//! active), how much of it is unassigned and claimable, which
//! authority-role lens each pile belongs to, and how long the oldest
//! wait has run. A hot node here is a deep queue: this is the
//! algedonic depth signal from queue-visibility Q4 rendered as data
//! the overlay can draw (feedback `9fe2fe66`, scope change 1 — the
//! threshold/telemetry half is change 2, gated on the Q4 decision).
//!
//! Three deliberate choices, all pinned by `tests/fleet_pg.rs`:
//!
//! - **Group key is `COALESCE(NULLIF(spec_slug,''), title)`.** Steps
//!   materialized before migration 100 carry no slug; a Workflow
//!   authored without slugs never gets them. Those rows group under
//!   their title instead of silently vanishing — the client renders
//!   what it can match onto the DAG and shows the rest off-map.
//! - **Depth is the live set only**: in-flight steps (`ready` /
//!   `active`) of open Jobs of the requested kind. History never
//!   enters, so the query costs O(work-in-flight) — 0.15% of the
//!   steps table on the live playground when this shipped.
//! - **Oldest wait is wall-clock** (`audit_log.created_at` of the
//!   step's `step.ready.*` event), never the sim-authoritative
//!   `timestamp` — the same doctrine as `flow.rs`, for the same
//!   reason. A node with no audit row reports no age rather than a
//!   fabricated one.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Serialize;

use crate::error::ViewsError;

/// One step of the Workflow's shape, with the fleet's depth at it.
#[derive(Debug, Clone, Serialize)]
pub struct FleetNode {
    /// `spec_slug` when the steps carry one, else their title — the
    /// client matches slugs onto the Workflow DAG and buckets the
    /// rest off-map.
    pub slug: String,
    pub ready: i64,
    pub active: i64,
    /// In-flight steps with no assignee — the claimable pool.
    pub unassigned: i64,
    /// Depth per `authority_role` lens. Steps with no role gate are
    /// absent here (they are in `ready`/`active` but belong to no
    /// group lens).
    pub by_role: BTreeMap<String, i64>,
    /// Wall-clock instant the oldest still-ready step became ready
    /// (ISO-8601 UTC), from the audit log. `None` when no
    /// `step.ready.*` row exists for any of them.
    pub oldest_ready_wall: Option<String>,
}

/// The fleet of one Workflow kind.
#[derive(Debug, Clone, Serialize)]
pub struct Fleet {
    pub workflow_kind: String,
    /// Open Jobs of the kind — the denominator the depths sit over.
    pub open_jobs: i64,
    pub nodes: Vec<FleetNode>,
    /// Wall-clock instant of the read (ISO-8601 UTC), so the client
    /// computes ages against the server's clock, not the browser's.
    pub as_of: String,
}

/// Read one Workflow kind's fleet.
#[async_trait]
pub trait FleetRepo: Send + Sync {
    async fn fleet(&self, workflow_kind: &str) -> Result<Fleet, ViewsError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape is what the overlay page is built on; a rename
    /// here is a breaking change and should read like one.
    #[test]
    fn fleet_serialises_with_the_field_names_the_page_reads() {
        let fleet = Fleet {
            workflow_kind: "wholesale-keg-order".into(),
            open_jobs: 2,
            nodes: vec![FleetNode {
                slug: "brew".into(),
                ready: 2,
                active: 0,
                unassigned: 1,
                by_role: BTreeMap::from([("brewer".to_string(), 2)]),
                oldest_ready_wall: Some("2026-08-08T21:00:00Z".into()),
            }],
            as_of: "2026-08-08T22:00:00Z".into(),
        };
        let v = serde_json::to_value(&fleet).expect("serialises");
        assert_eq!(v["workflow_kind"], "wholesale-keg-order");
        assert_eq!(v["open_jobs"], 2);
        assert_eq!(v["nodes"][0]["slug"], "brew");
        assert_eq!(v["nodes"][0]["ready"], 2);
        assert_eq!(v["nodes"][0]["unassigned"], 1);
        assert_eq!(v["nodes"][0]["by_role"]["brewer"], 2);
        assert_eq!(v["nodes"][0]["oldest_ready_wall"], "2026-08-08T21:00:00Z");
        assert_eq!(v["as_of"], "2026-08-08T22:00:00Z");
    }
}
