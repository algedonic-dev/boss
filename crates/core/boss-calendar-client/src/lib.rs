//! HTTP client port for reaching the `boss-calendar` service.
//!
//! Other services (boss-jobs, boss-people for PTO, future Meeting
//! modules) use this trait to reserve resources, list reservations,
//! and cascade-cancel by reason. Same shape as
//! `boss-locations-client` / `boss-classes-client` — the trait + a
//! Reqwest impl for production + a Fake impl for tests.
//!
//! Wire types are `boss_core::calendar::*` so callers don't have
//! to take a dep on the server crate (`boss-calendar`).

use async_trait::async_trait;

use boss_core::calendar::{Reservation, ReservationId, ReservationRequest, TimeWindow};
use boss_core::job::Subject;

#[derive(Debug, thiserror::Error)]
pub enum CalendarClientError {
    /// Hard reservation conflict — the calendar service refused
    /// the write because an existing hard reservation overlaps.
    /// Body carries the conflicting rows so the caller can render
    /// "this overlaps Job-12345" without a second round-trip.
    #[error("hard-reservation conflict ({} existing rows)", existing.len())]
    Conflict { existing: Vec<Reservation> },

    /// Reservation id passed to `cancel` doesn't exist on the
    /// service.
    #[error("reservation not found")]
    NotFound,

    /// Caller's request was rejected as malformed (bad window,
    /// unknown enum value, etc.).
    #[error("invalid request: {0}")]
    Invalid(String),

    /// Network / DNS / timeout. Service may be down.
    #[error("calendar service unreachable: {0}")]
    Unreachable(String),

    /// Service responded with an unexpected status — neither the
    /// success path nor a recognised error.
    #[error("calendar returned unexpected status: {0}")]
    UnexpectedStatus(u16),

    /// Body parsed but didn't match the expected shape.
    #[error("calendar returned malformed body: {0}")]
    MalformedBody(String),
}

#[async_trait]
pub trait CalendarClient: Send + Sync {
    async fn reserve(&self, req: ReservationRequest) -> Result<ReservationId, CalendarClientError>;

    async fn list(
        &self,
        subject: &Subject,
        window: TimeWindow,
    ) -> Result<Vec<Reservation>, CalendarClientError>;

    /// Soft-cancel one reservation. Idempotent on the service side,
    /// so the client doesn't need to track whether a cancel already
    /// happened.
    async fn cancel(&self, id: ReservationId, actor: &str) -> Result<(), CalendarClientError>;

    /// Cascade-cancel every reservation with the matching
    /// `(reason_kind, reason_ref_id)`. Returns the count cancelled.
    async fn cancel_by_reason(
        &self,
        reason_kind: &str,
        reason_ref_id: &str,
        actor: &str,
    ) -> Result<usize, CalendarClientError>;
}

/// Production client over `reqwest`. 5-second per-call timeout so a
/// stuck calendar service can't wedge a write indefinitely.
pub struct ReqwestCalendarClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestCalendarClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("building reqwest client"),
        }
    }
}

#[async_trait]
impl CalendarClient for ReqwestCalendarClient {
    async fn reserve(&self, req: ReservationRequest) -> Result<ReservationId, CalendarClientError> {
        let url = format!("{}/api/calendar/reservations", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(|e| CalendarClientError::Unreachable(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::CREATED {
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| CalendarClientError::MalformedBody(e.to_string()))?;
            let id_str = body.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                CalendarClientError::MalformedBody(format!("missing `id` in {body}"))
            })?;
            let uuid = uuid_parse(id_str)?;
            return Ok(ReservationId::from_uuid(uuid));
        }
        if status == reqwest::StatusCode::CONFLICT {
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| CalendarClientError::MalformedBody(e.to_string()))?;
            let existing: Vec<Reservation> = serde_json::from_value(
                body.get("existing")
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(vec![])),
            )
            .map_err(|e| CalendarClientError::MalformedBody(e.to_string()))?;
            return Err(CalendarClientError::Conflict { existing });
        }
        if status == reqwest::StatusCode::BAD_REQUEST {
            let msg = resp.text().await.unwrap_or_default();
            return Err(CalendarClientError::Invalid(msg));
        }
        Err(CalendarClientError::UnexpectedStatus(status.as_u16()))
    }

    async fn list(
        &self,
        subject: &Subject,
        window: TimeWindow,
    ) -> Result<Vec<Reservation>, CalendarClientError> {
        let url = format!(
            "{}/api/calendar/reservations?resource_kind={}&resource_id={}&start={}&end={}",
            self.base_url,
            subject.kind,
            // Re-encode the id since it's user-controlled. URL-safe
            // chars only for this v1; tighter encoding lands when
            // an id contains something needing escaping.
            urlencode_minimal(&subject.id),
            // Datetime carries `+` for UTC offset; encode it.
            window.start.to_rfc3339().replace('+', "%2B"),
            window.end.to_rfc3339().replace('+', "%2B"),
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| CalendarClientError::Unreachable(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(CalendarClientError::UnexpectedStatus(
                resp.status().as_u16(),
            ));
        }
        resp.json::<Vec<Reservation>>()
            .await
            .map_err(|e| CalendarClientError::MalformedBody(e.to_string()))
    }

    async fn cancel(&self, id: ReservationId, actor: &str) -> Result<(), CalendarClientError> {
        let url = format!(
            "{}/api/calendar/reservations/{}?actor={}",
            self.base_url,
            id,
            urlencode_minimal(actor),
        );
        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .map_err(|e| CalendarClientError::Unreachable(e.to_string()))?;
        match resp.status() {
            reqwest::StatusCode::NO_CONTENT => Ok(()),
            reqwest::StatusCode::NOT_FOUND => Err(CalendarClientError::NotFound),
            other => Err(CalendarClientError::UnexpectedStatus(other.as_u16())),
        }
    }

    async fn cancel_by_reason(
        &self,
        reason_kind: &str,
        reason_ref_id: &str,
        actor: &str,
    ) -> Result<usize, CalendarClientError> {
        let url = format!("{}/api/calendar/cancel-by-reason", self.base_url);
        let body = serde_json::json!({
            "kind": reason_kind,
            "ref_id": reason_ref_id,
            "actor": actor,
        });
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| CalendarClientError::Unreachable(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(CalendarClientError::UnexpectedStatus(
                resp.status().as_u16(),
            ));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| CalendarClientError::MalformedBody(e.to_string()))?;
        body.get("cancelled")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .ok_or_else(|| {
                CalendarClientError::MalformedBody(format!("missing `cancelled` in {body}"))
            })
    }
}

fn uuid_parse(s: &str) -> Result<uuid::Uuid, CalendarClientError> {
    s.parse::<uuid::Uuid>()
        .map_err(|e| CalendarClientError::MalformedBody(format!("bad UUID: {e}")))
}

/// Minimal URL-encoder for the small set of chars that matter in
/// path/query segments boss-calendar reads. Keeps the dep graph
/// from pulling in a full `urlencoding` crate.
fn urlencode_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            ' ' => out.push_str("%20"),
            '+' => out.push_str("%2B"),
            '/' => out.push_str("%2F"),
            '?' => out.push_str("%3F"),
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            '#' => out.push_str("%23"),
            other => {
                // Fallback: percent-encode each UTF-8 byte. Rare in
                // practice (entity ids are kebab-case ascii).
                let mut buf = [0u8; 4];
                for b in other.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}

/// Test fake — collects calls in memory + lets the test pre-stage
/// outcomes. Distinct from `boss-calendar::InMemoryCalendar`
/// (which is a real implementation) — this one lets tests assert
/// "boss-jobs called reserve once with these args" without setting
/// up the full server.
pub struct FakeCalendarClient {
    inner: std::sync::Mutex<FakeState>,
}

struct FakeState {
    /// Calls observed, in order. Tests inspect this.
    pub calls: Vec<FakeCall>,
    /// Pre-staged conflict response — when set, the next reserve()
    /// returns Conflict carrying these rows.
    pub stage_conflict: Option<Vec<Reservation>>,
    /// Pre-staged Unreachable — when set, the next call returns
    /// it. Cleared after one use.
    pub stage_unreachable: bool,
    /// Cancel-by-reason returns this number on each call.
    pub cancel_by_reason_count: usize,
    /// Counter for synthesising reservation ids on successful reserves.
    next_id: u32,
}

#[derive(Debug, Clone)]
pub enum FakeCall {
    Reserve(ReservationRequest),
    List(Subject, TimeWindow),
    Cancel(ReservationId, String),
    CancelByReason(String, String, String),
}

impl Default for FakeCalendarClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeCalendarClient {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(FakeState {
                calls: Vec::new(),
                stage_conflict: None,
                stage_unreachable: false,
                cancel_by_reason_count: 0,
                next_id: 0,
            }),
        }
    }

    /// Pre-stage the next `reserve` to return Conflict with these
    /// rows. Cleared after one use.
    pub fn stage_conflict(&self, existing: Vec<Reservation>) {
        self.inner.lock().unwrap().stage_conflict = Some(existing);
    }

    pub fn stage_unreachable(&self) {
        self.inner.lock().unwrap().stage_unreachable = true;
    }

    pub fn set_cancel_by_reason_count(&self, n: usize) {
        self.inner.lock().unwrap().cancel_by_reason_count = n;
    }

    pub fn calls(&self) -> Vec<FakeCall> {
        self.inner.lock().unwrap().calls.clone()
    }
}

#[async_trait]
impl CalendarClient for FakeCalendarClient {
    async fn reserve(&self, req: ReservationRequest) -> Result<ReservationId, CalendarClientError> {
        let mut state = self.inner.lock().unwrap();
        state.calls.push(FakeCall::Reserve(req));
        if state.stage_unreachable {
            state.stage_unreachable = false;
            return Err(CalendarClientError::Unreachable("fake".into()));
        }
        if let Some(existing) = state.stage_conflict.take() {
            return Err(CalendarClientError::Conflict { existing });
        }
        state.next_id += 1;
        Ok(ReservationId::new())
    }

    async fn list(
        &self,
        subject: &Subject,
        window: TimeWindow,
    ) -> Result<Vec<Reservation>, CalendarClientError> {
        let mut state = self.inner.lock().unwrap();
        state.calls.push(FakeCall::List(subject.clone(), window));
        Ok(Vec::new())
    }

    async fn cancel(&self, id: ReservationId, actor: &str) -> Result<(), CalendarClientError> {
        let mut state = self.inner.lock().unwrap();
        state.calls.push(FakeCall::Cancel(id, actor.to_string()));
        Ok(())
    }

    async fn cancel_by_reason(
        &self,
        reason_kind: &str,
        reason_ref_id: &str,
        actor: &str,
    ) -> Result<usize, CalendarClientError> {
        let mut state = self.inner.lock().unwrap();
        state.calls.push(FakeCall::CancelByReason(
            reason_kind.to_string(),
            reason_ref_id.to_string(),
            actor.to_string(),
        ));
        Ok(state.cancel_by_reason_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::calendar::{ReservationStrength, reason};
    use chrono::{TimeZone, Utc};

    fn t(h: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 27, h, 0, 0).unwrap()
    }

    fn req() -> ReservationRequest {
        ReservationRequest {
            subject: Subject::new("employee", "emp-1"),
            window: TimeWindow::new(t(10), t(12)).unwrap(),
            reason_kind: reason::JOB_STEP.to_string(),
            reason_ref_id: "stp-1".into(),
            strength: ReservationStrength::Hard,
            notes: None,
            created_by: "test".into(),
        }
    }

    #[tokio::test]
    async fn fake_records_reserve_call() {
        let c = FakeCalendarClient::new();
        let id = c.reserve(req()).await.unwrap();
        assert!(!id.to_string().is_empty());
        let calls = c.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            FakeCall::Reserve(r) => assert_eq!(r.reason_ref_id, "stp-1"),
            other => panic!("expected Reserve, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fake_returns_staged_conflict_then_clears_it() {
        let c = FakeCalendarClient::new();
        c.stage_conflict(vec![]);
        let err = c.reserve(req()).await.unwrap_err();
        assert!(matches!(err, CalendarClientError::Conflict { .. }));
        // Next call succeeds — staging is one-shot.
        c.reserve(req()).await.unwrap();
    }

    #[tokio::test]
    async fn fake_returns_staged_unreachable_then_clears_it() {
        let c = FakeCalendarClient::new();
        c.stage_unreachable();
        let err = c.reserve(req()).await.unwrap_err();
        assert!(matches!(err, CalendarClientError::Unreachable(_)));
        c.reserve(req()).await.unwrap();
    }

    #[tokio::test]
    async fn fake_cancel_by_reason_returns_staged_count() {
        let c = FakeCalendarClient::new();
        c.set_cancel_by_reason_count(3);
        let n = c
            .cancel_by_reason(reason::JOB_STEP, "stp-1", "test")
            .await
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn urlencode_passes_safe_chars_through() {
        assert_eq!(urlencode_minimal("emp-001"), "emp-001");
        assert_eq!(urlencode_minimal("loc-brewery_hq.1"), "loc-brewery_hq.1");
    }

    #[test]
    fn urlencode_escapes_query_specials() {
        assert_eq!(urlencode_minimal("a+b"), "a%2Bb");
        assert_eq!(urlencode_minimal("a&b"), "a%26b");
        assert_eq!(urlencode_minimal("a b"), "a%20b");
    }
}
