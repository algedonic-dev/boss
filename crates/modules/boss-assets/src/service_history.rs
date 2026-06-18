//! Service-history projection: prior `field-service` Jobs on a
//! device serial.
//!
//! Lives here rather than in Tier 1 `boss-jobs-client`: `field-service`
//! is a used-device-shop JobKind, not a generic state-machine OS
//! concept, so the projection sits next to its sole consumer
//! (`asset_insights.rs`).

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use boss_jobs_client::JobSummary;

/// One row in the device service-history projection — a prior
/// `field-service` Job on the same serial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHistoryRow {
    pub job_id: String,
    pub title: String,
    pub priority: String,
    pub status: String,
    pub opened_on: NaiveDate,
    pub closed_on: Option<NaiveDate>,
}

/// Project a list of generic `JobSummary` rows into the
/// service-history wire shape. The caller is expected to have
/// filtered by `kind=field-service` and the target asset already;
/// this is a pure shape transformation.
pub fn project_service_history(jobs: &[JobSummary]) -> Vec<ServiceHistoryRow> {
    jobs.iter()
        .map(|j| ServiceHistoryRow {
            job_id: j.id.clone(),
            title: j.title.clone(),
            priority: j.priority.clone(),
            status: j.status.clone(),
            opened_on: j.opened_on,
            closed_on: j.closed_on,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, opened: NaiveDate, closed: Option<NaiveDate>) -> JobSummary {
        JobSummary {
            id: id.to_string(),
            kind: "field-service".to_string(),
            title: format!("Job {id}"),
            priority: "standard".to_string(),
            status: "open".to_string(),
            opened_on: opened,
            closed_on: closed,
        }
    }

    #[test]
    fn projects_id_dates_and_status() {
        let jobs = vec![
            job(
                "J-1",
                NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                Some(NaiveDate::from_ymd_opt(2026, 1, 7).unwrap()),
            ),
            job("J-2", NaiveDate::from_ymd_opt(2026, 3, 20).unwrap(), None),
        ];
        let projected = project_service_history(&jobs);
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].job_id, "J-1");
        assert_eq!(
            projected[0].closed_on,
            Some(NaiveDate::from_ymd_opt(2026, 1, 7).unwrap())
        );
        assert!(projected[1].closed_on.is_none());
    }

    #[test]
    fn empty_input_returns_empty_output() {
        let projected = project_service_history(&[]);
        assert!(projected.is_empty());
    }
}
