//! The sensor-owned hourly request series. No event or packet is emitted.

#[cfg(feature = "postgres")]
use boss_core::request_latency::Sample;
use boss_core::request_latency::{Class, RecorderError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The jobs adapter owns these MatchedPath contracts; core timing knows no routes.
pub fn classify(method: &str, path: &str) -> Option<Class> {
    if method == "POST"
        && matches!(
            path,
            "/api/jobs/readiness"
                | "/api/jobs/repairs/step-plugin-version"
                // The clock rule's lease sweep: one batch of up to 64
                // expiries per POST (backlog 3aeaf08c).
                | "/api/jobs/resource-leases/sweep"
        )
    {
        return Some(Class::Bulk);
    }
    if method == "GET" && path == "/api/jobs/{id}" {
        return Some(Class::SmallRead);
    }
    if matches!(method, "POST" | "PUT" | "PATCH")
        // A person's ops request from the verb list is one packet
        // create, timed as the create it calls (backlog 7aef2405).
        && ((method == "POST" && matches!(path, "/api/jobs" | "/api/jobs/file-an-ops-request"))
            || path == "/api/jobs/{id}"
            || path.starts_with("/api/jobs/{id}/"))
    {
        return Some(Class::Write);
    }
    None
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Distribution {
    pub count: u64,
    pub max_ns: Option<u64>,
    pub buckets: std::collections::BTreeMap<u64, u64>,
}

impl Distribution {
    fn validate(&self) -> Result<(), RecorderError> {
        let mut count = 0_u64;
        for (&bucket, &n) in &self.buckets {
            let shift = (63 - bucket.max(1).leading_zeros()).saturating_sub(8);
            if n == 0 || bucket | ((1_u64 << shift) - 1) != bucket {
                return Err(RecorderError(
                    "invalid persisted distribution bucket".into(),
                ));
            }
            count = count.checked_add(n).ok_or_else(overflow)?;
        }
        if self.buckets.len() > 16384
            || count != self.count
            || (count == 0) != self.max_ns.is_none()
        {
            return Err(RecorderError(
                "invalid persisted distribution count/maximum".into(),
            ));
        }
        if let Some(max) = self.max_ns {
            let shift = (63 - max.max(1).leading_zeros()).saturating_sub(8);
            if self.buckets.last_key_value().map(|(k, _)| *k) != Some(max | ((1_u64 << shift) - 1))
            {
                return Err(RecorderError(
                    "persisted exact maximum disagrees with distribution".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn record(&mut self, ns: u64) -> Result<(), RecorderError> {
        // Exact below512ns; above that, an upper-bound bucket with <0.4%
        // relative width. At most64*256 keys even for a century-long request.
        let shift = (63 - ns.max(1).leading_zeros()).saturating_sub(8);
        let bucket = ns | ((1_u64 << shift) - 1);
        let count = self
            .count
            .checked_add(1)
            .ok_or_else(|| RecorderError("sample count overflow".into()))?;
        let bucket_count = self
            .buckets
            .get(&bucket)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| RecorderError("bucket count overflow".into()))?;
        self.count = count;
        self.buckets.insert(bucket, bucket_count);
        self.max_ns = Some(self.max_ns.map_or(ns, |old| old.max(ns)));
        Ok(())
    }
    pub fn merge(&mut self, other: &Self) -> Result<(), RecorderError> {
        self.validate()?;
        other.validate()?;
        let mut next = self.clone();
        next.count = next
            .count
            .checked_add(other.count)
            .ok_or_else(|| RecorderError("sample count overflow".into()))?;
        for (&bucket, &count) in &other.buckets {
            let total = next
                .buckets
                .get(&bucket)
                .copied()
                .unwrap_or(0)
                .checked_add(count)
                .ok_or_else(|| RecorderError("bucket count overflow".into()))?;
            next.buckets.insert(bucket, total);
        }
        next.max_ns = match (next.max_ns, other.max_ns) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        *self = next;
        Ok(())
    }
    pub fn p99_ns(&self) -> Option<u64> {
        if self.count == 0 {
            return None;
        }
        let rank = self.count - self.count / 100;
        let mut seen = 0_u64;
        for (&upper, &count) in &self.buckets {
            seen = seen.checked_add(count)?;
            if seen >= rank {
                return Some(upper.min(self.max_ns?));
            }
        }
        None
    }
}

#[cfg(feature = "postgres")]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Source {
    service: String,
    build: String,
    recording_started_at: DateTime<Utc>,
    first_recorded_at: DateTime<Utc>,
    last_recorded_at: DateTime<Utc>,
    #[serde(default)]
    recording_ended_at: Option<DateTime<Utc>>,
}

#[cfg(feature = "postgres")]
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Hour {
    writes: Metrics,
    small_reads: Metrics,
    #[serde(default)]
    bulk: Metrics,
    recorder_failures: u64,
    dropped_samples: u64,
    recording_gap_since: Option<DateTime<Utc>>,
    sources: std::collections::BTreeMap<String, Source>,
    source_roster_overflow: bool,
}

#[cfg(feature = "postgres")]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MaxOrigin {
    service: String,
    method: String,
    route_template: String,
    status: boss_core::request_latency::Status,
}

#[cfg(feature = "postgres")]
#[derive(Debug, Default, Clone, Serialize)]
struct Metrics {
    #[serde(flatten)]
    distribution: Distribution,
    /// Old stored maxima have no origin; missing remains unknown.
    #[serde(default)]
    max_origin: Option<MaxOrigin>,
}

#[cfg(feature = "postgres")]
impl<'de> Deserialize<'de> for Metrics {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // serde's flattened intermediate map does not parse JSON's numeric
        // object keys as u64. Decode the distribution through JSON itself,
        // retaining its original stored shape and legacy origin absence.
        let value = serde_json::Value::deserialize(deserializer)?;
        let distribution =
            serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?;
        let max_origin = serde_json::from_value(
            value
                .get("max_origin")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(serde::de::Error::custom)?;
        Ok(Self {
            distribution,
            max_origin,
        })
    }
}

#[cfg(feature = "postgres")]
impl Metrics {
    fn record(&mut self, sample: &Sample) -> Result<(), RecorderError> {
        if self
            .distribution
            .max_ns
            .is_none_or(|old| sample.duration_ns > old)
        {
            self.max_origin = if sample.method.is_empty() || sample.route_template.is_empty() {
                None
            } else {
                Some(MaxOrigin {
                    service: sample.service.clone(),
                    method: sample.method.clone(),
                    route_template: sample.route_template.clone(),
                    status: sample.status,
                })
            };
        }
        self.distribution.record(sample.duration_ns)
    }
    fn merge(&mut self, other: &Self) -> Result<(), RecorderError> {
        let replace = other
            .distribution
            .max_ns
            .is_some_and(|n| self.distribution.max_ns.is_none_or(|old| n > old));
        self.distribution.merge(&other.distribution)?;
        if replace {
            self.max_origin = other.max_origin.clone();
        }
        Ok(())
    }
    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"count":self.distribution.count,"max_ns":self.distribution.max_ns,
            "p99_ns":self.distribution.p99_ns(),"buckets":self.distribution.buckets,"max_origin":self.max_origin})
    }
}

fn overflow() -> RecorderError {
    RecorderError("hourly telemetry counter overflow; coverage unavailable".into())
}

#[cfg(feature = "postgres")]
impl Hour {
    fn validate(&self) -> Result<(), RecorderError> {
        self.writes.distribution.validate()?;
        self.small_reads.distribution.validate()?;
        self.bulk.distribution.validate()?;
        if self.sources.len() > 64
            || ((self.writes.distribution.count > 0
                || self.small_reads.distribution.count > 0
                || self.bulk.distribution.count > 0)
                && self.sources.is_empty())
        {
            return Err(RecorderError("persisted source roster unavailable".into()));
        }
        if self.sources.values().any(|source| {
            source.first_recorded_at < source.recording_started_at
                || source.last_recorded_at < source.first_recorded_at
                || source
                    .recording_ended_at
                    .is_some_and(|ended| ended < source.last_recorded_at)
        }) {
            return Err(RecorderError(
                "persisted epoch chronology unavailable".into(),
            ));
        }
        Ok(())
    }
    fn add(&mut self, sample: &Sample) -> Result<(), RecorderError> {
        match sample.class {
            Class::Write => self.writes.record(sample)?,
            Class::SmallRead => self.small_reads.record(sample)?,
            Class::Bulk => self.bulk.record(sample)?,
            Class::Recording => {}
        }
        self.recorder_failures = self
            .recorder_failures
            .checked_add(sample.recorder_failures)
            .ok_or_else(overflow)?;
        self.dropped_samples = self
            .dropped_samples
            .checked_add(sample.dropped_samples)
            .ok_or_else(overflow)?;
        if let Some(since) = sample.recording_gap_since {
            self.recording_gap_since =
                Some(self.recording_gap_since.map_or(since, |old| old.min(since)));
        }
        if let Some(held) = self.sources.get_mut(&sample.recorder_epoch) {
            if held.service != sample.service
                || held.build != sample.build
                || held.recording_started_at != sample.recording_started_at
            {
                return Err(RecorderError("recorder epoch changed identity".into()));
            }
            if held.recording_ended_at.is_some() {
                return Err(RecorderError(
                    "closed recording epoch cannot accept more observations".into(),
                ));
            }
            held.first_recorded_at = held.first_recorded_at.min(sample.completed_at);
            held.last_recorded_at = held.last_recorded_at.max(sample.completed_at);
            held.recording_ended_at = sample.recording_ended_at;
        } else if self.sources.len() < 64 {
            self.sources.insert(
                sample.recorder_epoch.clone(),
                Source {
                    service: sample.service.clone(),
                    build: sample.build.clone(),
                    recording_started_at: sample.recording_started_at,
                    first_recorded_at: sample.completed_at,
                    last_recorded_at: sample.completed_at,
                    recording_ended_at: sample.recording_ended_at,
                },
            );
        } else {
            self.source_roster_overflow = true;
        }
        Ok(())
    }

    fn snapshot(&self, hour: i64, observed: bool) -> serde_json::Value {
        serde_json::json!({"hour_epoch_seconds": hour, "observed": observed,
            "writes": self.writes.snapshot(), "small_reads": self.small_reads.snapshot(), "bulk":self.bulk.snapshot(),
            "recorder_failures": self.recorder_failures, "dropped_request_samples": self.dropped_samples,
            "recording_gap_since": self.recording_gap_since, "recording_epochs": self.sources,
            "source_roster_overflow": self.source_roster_overflow})
    }
}

#[cfg(feature = "postgres")]
fn hour_of(at: DateTime<Utc>) -> i64 {
    at.timestamp().div_euclid(3600) * 3600
}

/// Persistence and reads have their own port: neither opens work nor emits an event.
#[async_trait::async_trait]
pub trait Series: boss_core::request_latency::Recorder {
    async fn read(&self, now: DateTime<Utc>) -> Result<serde_json::Value, RecorderError>;
    async fn read_hours(
        &self,
        now: DateTime<Utc>,
        hours: u8,
    ) -> Result<serde_json::Value, RecorderError> {
        if hours == 24 {
            self.read(now).await
        } else {
            Err(RecorderError("requested sensor window unavailable".into()))
        }
    }
    /// The retained slow samples of the window, each with the moment it
    /// stalled in (d9b39685). An adapter that keeps none says so.
    async fn read_slow(
        &self,
        _now: DateTime<Utc>,
        _hours: u8,
    ) -> Result<serde_json::Value, RecorderError> {
        Err(RecorderError("slow request moments unavailable".into()))
    }
}

#[cfg(feature = "postgres")]
mod observer;
#[cfg(feature = "postgres")]
mod pg;
#[cfg(feature = "postgres")]
pub use observer::PgSlowObserver;
#[cfg(feature = "postgres")]
pub use pg::PgLatency;

pub fn router(store: std::sync::Arc<dyn Series>) -> axum::Router {
    use axum::{
        extract::{OriginalUri, Query, State},
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::get,
    };
    use boss_policy_client::CurrentUser;
    #[derive(serde::Deserialize)]
    struct Window {
        #[serde(default = "default_hours")]
        hours: u8,
        /// The retained slow samples and their moments instead of the
        /// hourly series (d9b39685); the default read is unchanged.
        #[serde(default)]
        slow: bool,
    }
    fn default_hours() -> u8 {
        24
    }
    // The returned as_of and window follow the reader pod clock. A reader
    // behind the database across an hour boundary may omit its newest hour;
    // this changes neither stored evidence nor database-clock retention.
    async fn read(
        CurrentUser(user): CurrentUser,
        State(store): State<std::sync::Arc<dyn Series>>,
        OriginalUri(uri): OriginalUri,
    ) -> Response {
        if !crate::trust::can_read(&user) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let window = match Query::<Window>::try_from_uri(&uri) {
            Ok(Query(window)) => window,
            Err(error) => return error.into_response(),
        };
        if !(1..=72).contains(&window.hours) {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let at = boss_clock_client::wall_now();
        let read = if window.slow {
            store.read_slow(at, window.hours).await
        } else {
            store.read_hours(at, window.hours).await
        };
        match read {
            Ok(series) => axum::Json(series).into_response(),
            Err(error) => (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response(),
        }
    }
    axum::Router::new()
        .route("/api/sensors/request-latency", get(read))
        .with_state(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn internal_bulk_posts_are_separate_from_ordinary_write_acceptance() {
        for path in [
            "/api/jobs/readiness",
            "/api/jobs/repairs/step-plugin-version",
            "/api/jobs/resource-leases/sweep",
        ] {
            assert_eq!(
                serde_json::to_value(classify("POST", path)).unwrap(),
                "bulk"
            );
        }
        assert_eq!(classify("POST", "/api/jobs"), Some(Class::Write));
    }
    #[test]
    fn every_jobs_write_shape_and_single_read_have_their_backend_class() {
        for (method, path, class) in [
            ("POST", "/api/jobs", Class::Write),
            ("PUT", "/api/jobs/{id}", Class::Write),
            ("PATCH", "/api/jobs/{id}/metadata", Class::Write),
            ("POST", "/api/jobs/{id}/steps", Class::Write),
            ("PUT", "/api/jobs/{id}/steps/{step_id}", Class::Write),
            (
                "PATCH",
                "/api/jobs/{id}/steps/{step_id}/metadata",
                Class::Write,
            ),
            (
                "POST",
                "/api/jobs/{id}/steps/{step_id}/sign-offs",
                Class::Write,
            ),
            (
                "POST",
                "/api/jobs/{id}/steps/{step_id}/metadata/records",
                Class::Write,
            ),
            (
                "POST",
                "/api/jobs/{id}/steps/{step_id}/complete-if",
                Class::Write,
            ),
            ("GET", "/api/jobs/{id}", Class::SmallRead),
        ] {
            assert_eq!(classify(method, path), Some(class), "{method} {path}");
        }
    }
    #[test]
    fn lists_health_streams_telemetry_and_other_services_are_not_jobs_small_reads() {
        for path in [
            "/api/jobs",
            "/api/jobs/health",
            "/api/jobs/summary",
            "/api/jobs/live",
            "/api/jobs/steps/{step_id}",
            "/api/jobs/{id}/stream",
            "/api/sensors/request-latency",
            "/api/yard/regions",
            "/api/messages/unread/{employee_id}",
        ] {
            assert_eq!(classify("GET", path), None, "{path}");
        }
    }
    #[test]
    fn exact_max_is_unclamped_and_missing_is_unknown() {
        let mut d = Distribution::default();
        assert_eq!(d.max_ns, None);
        assert_eq!(d.p99_ns(), None);
        d.record(61_123_456_789).unwrap();
        assert_eq!(d.count, 1);
        assert_eq!(d.max_ns, Some(61_123_456_789));
        assert_eq!(d.p99_ns(), Some(61_123_456_789));
    }
    #[test]
    fn quantiles_merge_samples_not_averages_of_quantiles() {
        let mut fast = Distribution::default();
        for _ in 0..1000 {
            fast.record(10).unwrap();
        }
        let mut slow = Distribution::default();
        slow.record(60_000_000_001).unwrap();
        fast.merge(&slow).unwrap();
        assert_eq!(fast.count, 1001);
        assert_eq!(fast.max_ns, Some(60_000_000_001));
        assert_eq!(fast.p99_ns(), Some(10));
    }
}
