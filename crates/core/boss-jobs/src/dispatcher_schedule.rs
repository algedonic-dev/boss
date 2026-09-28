//! The dispatcher's schedule, read for the top board's NEXT UP row
//! (backlog 74569e94, design ea906603 Q3, car 3).
//!
//! The dispatcher serves `GET /api/dispatcher/schedule` (car 2): each
//! scheduled rule it enforces, with its last firing and when its runner
//! next fires it. The jobs API folds the next-due half into
//! `next_up` on `GET /api/yard/regions`, so this is a port, a reqwest
//! adapter and a fake — the `department::rules` shape. The rule table
//! and the runner's schedule math belong to the dispatcher; reading
//! either from here would be a second reader of a sibling's schema and
//! a second copy of its arithmetic.
//!
//! THE VIEWER IS FORWARDED. The schedule is the one dispatcher read that
//! asks policy (a caller whose scope reads no packets is told it is
//! withheld, backlog e5f7b51e's rule), so the adapter sends the caller's
//! own identity as `x-boss-user` — the way `owner_resolution` asks the
//! people service — and the dispatcher judges the viewer, never the
//! jobs API standing in for them.

use async_trait::async_trait;
use boss_policy_client::User;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One scheduled rule as the dispatcher's schedule read serves it — the
/// fields NEXT UP reads. The payload carries more (`version`,
/// `anchor_date`, `last_fired`); an unknown field is ignored, so a
/// richer dispatcher never breaks this reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledRule {
    pub name: String,
    /// The cadence token (`daily`, `hourly`, `every-5-minutes`, …),
    /// parsed by `boss_core::calendar::Cadence::parse` where it matters.
    pub cadence: String,
    /// When the runner next fires it. `None` beside `next_due_why`: the
    /// dispatcher could not compute it and said why.
    #[serde(default)]
    pub next_due: Option<DateTime<Utc>>,
    #[serde(default)]
    pub next_due_why: Option<String>,
    /// The rule's guard, when it has one — a due firing it may decline.
    #[serde(default)]
    pub when: Option<String>,
}

#[async_trait]
pub trait DispatcherSchedule: Send + Sync {
    /// The scheduled rules, soonest first, read AS `viewer`. `Err` is
    /// the read not answering — the dispatcher dark, an older dispatcher
    /// without the read, or the schedule withheld from this viewer —
    /// with the reason, which the board shows as unread. Never an empty
    /// list in its place: that would read as "nothing is scheduled".
    async fn schedule(&self, viewer: &User) -> Result<Vec<ScheduledRule>, String>;
}

/// `GET {base}/api/dispatcher/schedule`, the viewer in `x-boss-user`.
pub struct ReqwestDispatcherSchedule {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestDispatcherSchedule {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = boss_core::http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl DispatcherSchedule for ReqwestDispatcherSchedule {
    async fn schedule(&self, viewer: &User) -> Result<Vec<ScheduledRule>, String> {
        let url = format!("{}/api/dispatcher/schedule", self.base_url);
        let who = serde_json::to_string(viewer).map_err(|e| format!("GET {url}: {e}"))?;
        let who = reqwest::header::HeaderValue::from_str(&who)
            .map_err(|e| format!("GET {url}: the viewer cannot ride a header ({e})"))?;
        let resp = self
            .http
            .get(&url)
            .header("x-boss-user", who)
            .send()
            .await
            .map_err(|e| format!("GET {url}: the dispatcher did not answer ({e})"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!(
                "GET {url}: HTTP {status} (a dispatcher older than design ea906603 serves \
                 no schedule)"
            ));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| format!("GET {url}: the body is not JSON ({e})"))?;
        rules_of(&body).map_err(|why| format!("GET {url}: {why}"))
    }
}

/// The rows of a schedule payload, or why there are none. Pure, so the
/// three shapes the dispatcher answers with are pinned without a server:
/// rows; `schedule: null` beside `schedule_error` (withheld, or its own
/// rule table would not load); and a body with neither, which is not an
/// empty schedule either.
pub fn rules_of(body: &Value) -> Result<Vec<ScheduledRule>, String> {
    match body.get("schedule") {
        Some(Value::Array(_)) => serde_json::from_value(body["schedule"].clone())
            .map_err(|e| format!("a schedule row did not parse ({e})")),
        _ => Err(body
            .get("schedule_error")
            .and_then(Value::as_str)
            .map_or_else(
                || "the dispatcher answered without a schedule or a reason".to_string(),
                |e| format!("the dispatcher withheld the schedule: {e}"),
            )),
    }
}

/// A fixed answer, for tests and the in-memory path.
pub struct FakeDispatcherSchedule(pub Result<Vec<ScheduledRule>, String>);

#[async_trait]
impl DispatcherSchedule for FakeDispatcherSchedule {
    async fn schedule(&self, _viewer: &User) -> Result<Vec<ScheduledRule>, String> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The rows car 2's read serves, extra fields and all.
    #[test]
    fn a_schedule_payload_reads_as_its_rows() {
        let rows = rules_of(&json!({
            "now": "2026-09-27T14:32:10Z",
            "schedule": [
                {"name": "sensors-poll", "version": 3, "cadence": "every-5-minutes",
                 "anchor_date": "2026-09-17", "when": null, "last_fired": null,
                 "next_due": "2026-09-27T14:35:00Z", "next_due_why": null},
                {"name": "bank-sweep", "cadence": "daily", "next_due": null,
                 "next_due_why": "business calendar `us-banking` could not be read"}
            ],
            "schedule_error": null
        }))
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "sensors-poll");
        assert_eq!(
            rows[0].next_due.map(|t| t.to_rfc3339()),
            Some("2026-09-27T14:35:00+00:00".to_string())
        );
        assert_eq!(rows[1].next_due, None);
        assert!(
            rows[1]
                .next_due_why
                .as_deref()
                .unwrap()
                .contains("us-banking")
        );
    }

    /// Withheld, or a table that would not load: the reason, never an
    /// empty schedule.
    #[test]
    fn a_withheld_schedule_is_its_reason_never_an_empty_list() {
        let err = rules_of(&json!({
            "schedule": null,
            "schedule_error": "this caller's policy scope reads no packets"
        }))
        .unwrap_err();
        assert!(err.contains("reads no packets"), "{err}");
        let err = rules_of(&json!({"rules": []})).unwrap_err();
        assert!(err.contains("without a schedule"), "{err}");
    }
}
