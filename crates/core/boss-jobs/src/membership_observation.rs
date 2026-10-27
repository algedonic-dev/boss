//! Fixed query-cost observations, never packet payloads or a flow verdict.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A native observation is bounded independently of the history it reads.
pub const MAX_REPORT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Observation {
    pub state: String,
    pub why: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub audit_cutoff: Option<i64>,
    pub window_cut: Option<i64>,
    pub carried_packets: usize,
    pub excluded_groups: usize,
    pub phases: Vec<Phase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_timeout: Option<SeedTimeout>,
}

/// Diagnostic facts at the original seed cut, not a successful seed.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SeedTimeout {
    pub phase: String,
    pub sqlstate: String,
    /// None means unavailable or absent; the named phase distinguishes them.
    pub order_index_valid: Option<bool>,
    /// None is unavailable; Some(empty) is a measured empty roster.
    pub largest_packets: Option<Vec<PacketHistoryCount>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PacketHistoryCount {
    pub packet_id: String,
    pub historical_rows: u64,
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
