//! HTTP client port for reaching the `boss-jobs` service.
//!
//! Defines tenant-neutral, question-shaped methods other services
//! need to ask Jobs: "what's the phase distribution right now?" and
//! "what Jobs match these filters?". Tenant-shape projections
//! (refurb-pipeline WIP, field-service history per device) build on
//! top of these in modules-tier crates — keeping this trait
//! generic over JobKind / StepType vocabulary.

use std::collections::BTreeMap;

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Service-name marker for the shared [`HttpClientError`]. Keeps the
/// `Display` text reading `"jobs service unreachable: …"`.
#[derive(Debug)]
pub struct Jobs;
impl ServiceLabel for Jobs {
    const NAME: &'static str = "jobs";
}

/// Transport error for the Jobs client. Alias of the shared
/// [`HttpClientError`] so existing constructors and matches keep
/// compiling.
pub type JobsClientError = HttpClientError<Jobs>;

/// Generic phase-distribution response keyed by JobKind. The
/// `tiers` map carries per-step-tier counts (and the synthetic
/// `-1` bucket for "every step terminal but Job still open").
///
/// Tenant-shape projections (e.g. mapping refurb-used tiers to
/// warehouse stages) live in the consuming module crate.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhaseDistribution {
    pub by_kind: BTreeMap<String, PhaseKindCounts>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhaseKindCounts {
    /// Per-tier counts. Tier `-1` is "all steps terminal, Job still open".
    pub tiers: BTreeMap<String, i64>,
}

/// One row from `/api/jobs`. Generic across JobKinds — callers
/// filter by `kind` and project to their own row shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSummary {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub priority: String,
    pub status: String,
    pub opened_on: NaiveDate,
    pub closed_on: Option<NaiveDate>,
}

#[async_trait]
pub trait JobsClient: Send + Sync {
    /// Per-kind, per-tier distribution of Jobs in the system.
    /// `status` filters to e.g. `Some("open")` for in-flight only.
    async fn phase_distribution(
        &self,
        status: Option<&str>,
    ) -> Result<PhaseDistribution, JobsClientError>;

    /// List Jobs matching the given filters. `kind` selects a
    /// JobKind; `subject_id` filters by the Job subject id;
    /// `limit` caps the response.
    async fn list_jobs(
        &self,
        kind: Option<&str>,
        subject_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JobSummary>, JobsClientError>;
}

/// Production `JobsClient` that calls the jobs HTTP API over reqwest.
/// 5-second timeout matches the `AssetsClient` convention — an
/// unresponsive jobs service shouldn't wedge the warehouse dashboard
/// indefinitely.
pub struct ReqwestJobsClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestJobsClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl JobsClient for ReqwestJobsClient {
    async fn phase_distribution(
        &self,
        status: Option<&str>,
    ) -> Result<PhaseDistribution, JobsClientError> {
        let url = match status {
            Some(s) => format!("{}/api/jobs/phase-distribution?status={s}", self.base_url),
            None => format!("{}/api/jobs/phase-distribution", self.base_url),
        };
        let body: serde_json::Value = http_client::get_json(&self.http, &url).await?;
        parse_phase_distribution(&body)
    }

    async fn list_jobs(
        &self,
        kind: Option<&str>,
        subject_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JobSummary>, JobsClientError> {
        // List endpoint's query param for "filter by subject" is
        // `subject_id` (see `ListJobsQuery` in boss-jobs/src/http.rs).
        let mut url = format!("{}/api/jobs?limit={limit}", self.base_url);
        if let Some(k) = kind {
            url.push_str(&format!("&kind={k}"));
        }
        if let Some(sid) = subject_id {
            url.push_str(&format!("&subject_id={sid}"));
        }
        let body: serde_json::Value = http_client::get_json(&self.http, &url).await?;
        let data = body.get("data").and_then(|v| v.as_array()).ok_or_else(|| {
            JobsClientError::MalformedBody(format!("missing data array in {body}"))
        })?;
        Ok(project_job_summaries(data))
    }
}

/// Pure projector: `/api/jobs` data array → JobSummary rows.
/// Rows missing `opened_on` are skipped (the field is non-nullable
/// on the server side; absence indicates malformed data).
pub fn project_job_summaries(rows: &[serde_json::Value]) -> Vec<JobSummary> {
    rows.iter()
        .filter_map(|r| {
            Some(JobSummary {
                id: r.get("id").and_then(|v| v.as_str())?.to_string(),
                kind: r
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                title: r
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                priority: r
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .unwrap_or("standard")
                    .to_string(),
                status: r
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("open")
                    .to_string(),
                opened_on: r
                    .get("opened_on")
                    .and_then(|v| v.as_str())
                    .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())?,
                closed_on: r
                    .get("closed_on")
                    .and_then(|v| v.as_str())
                    .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()),
            })
        })
        .collect()
}

/// Pure parser: raw HTTP body → PhaseDistribution. Extracted so
/// tests can pin the parse without a reqwest stack.
pub fn parse_phase_distribution(
    body: &serde_json::Value,
) -> Result<PhaseDistribution, JobsClientError> {
    let by_kind_obj = body
        .get("by_kind")
        .and_then(|v| v.as_object())
        .ok_or_else(|| JobsClientError::MalformedBody(format!("missing by_kind in {body}")))?;

    let mut by_kind = BTreeMap::new();
    for (kind, v) in by_kind_obj {
        let mut tiers = BTreeMap::new();
        if let Some(t) = v.get("tiers").and_then(|t| t.as_object()) {
            for (tier_str, count_v) in t {
                let count = count_v.as_i64().unwrap_or(0);
                tiers.insert(tier_str.clone(), count);
            }
        }
        by_kind.insert(kind.clone(), PhaseKindCounts { tiers });
    }
    Ok(PhaseDistribution { by_kind })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_phase_distribution_from_wire_shape() {
        let body = json!({
            "by_kind": {
                "refurb-used": { "tiers": { "0": 3, "1": 5, "-1": 4 } },
                "sale":        { "tiers": { "0": 50 } }
            }
        });
        let dist = parse_phase_distribution(&body).unwrap();
        assert_eq!(dist.by_kind.len(), 2);
        let refurb = &dist.by_kind["refurb-used"];
        assert_eq!(refurb.tiers["0"], 3);
        assert_eq!(refurb.tiers["-1"], 4);
    }

    #[test]
    fn phase_distribution_with_missing_by_kind_errors() {
        let body = json!({ "wrong_key": {} });
        let result = parse_phase_distribution(&body);
        assert!(matches!(result, Err(JobsClientError::MalformedBody(_))));
    }

    #[test]
    fn project_job_summaries_reads_dates_and_fields() {
        let rows = vec![
            json!({
                "id": "J-1",
                "kind": "field-service",
                "title": "On-site repair — heat sink",
                "priority": "urgent",
                "status": "closed",
                "opened_on": "2026-01-05",
                "closed_on": "2026-01-07"
            }),
            json!({
                "id": "J-2",
                "kind": "field-service",
                "title": "Calibration follow-up",
                "priority": "standard",
                "status": "open",
                "opened_on": "2026-03-20"
            }),
        ];
        let projected = project_job_summaries(&rows);
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].id, "J-1");
        assert_eq!(projected[0].priority, "urgent");
        assert_eq!(
            projected[0].closed_on,
            Some(NaiveDate::from_ymd_opt(2026, 1, 7).unwrap())
        );
        assert!(projected[1].closed_on.is_none());
    }

    #[test]
    fn project_job_summaries_skips_rows_without_opened_on() {
        let rows = vec![
            json!({ "id": "ok", "opened_on": "2026-04-01" }),
            json!({ "id": "bad" }),
        ];
        let projected = project_job_summaries(&rows);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].id, "ok");
    }
}
