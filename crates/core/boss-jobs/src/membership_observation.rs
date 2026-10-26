//! Fixed query-cost observations, never packet payloads or a flow verdict.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A native observation is bounded independently of the history it reads.
pub const MAX_REPORT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Observation {
    pub state: String,
    pub why: Option<String>,
    pub until: Option<DateTime<Utc>>,
    pub audit_cutoff: Option<i64>,
    pub window_cut: Option<i64>,
    pub carried_packets: usize,
    pub excluded_groups: usize,
    pub phases: Vec<Phase>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    pub elapsed_ms: u64,
    pub sql_sha256: Option<String>,
    pub binds_sha256: Option<String>,
    pub plan: Option<serde_json::Value>,
    pub rows: Option<u64>,
    pub why: Option<String>,
}
