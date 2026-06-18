//! Clock primitives: the `Clock` trait plus `WallClock` / `SimClock`
//! / `FixedClock` impls.
//!
//! Services do **not** read time from here. They hold a `ClockClient`
//! and call `clock.now()` against the authoritative clock service
//! (`boss-clock`), which decides per deployment whether time is sim or
//! wall. These impls exist for the sim engine — which drives its own
//! `SimClock` tick by tick — and for deterministic tests.
//!
//! Events carry a `simulated: bool` so the audit log records, forever,
//! whether a row came from a sim or a real run.
//!
//! `RequestContext` / `request_context` / `request_now` resolve time
//! from a per-request `X-Sim-Time` header; they are superseded by the
//! clock service and have no callers.

use chrono::{DateTime, NaiveDate, Utc};
use std::sync::{Arc, RwLock};

/// Header name reserved for sim-time propagation. Per-request scope,
/// lowercase dash-separated. The simulator stamps every outbound
/// POST/PUT with this header carrying an RFC-3339 instant. Real
/// callers don't send it. Presence of the header is the SIM tag.
pub const SIM_TIME_HEADER: &str = "x-sim-time";

/// Resolved request context: the effective `now` for this handler
/// plus the SIM marker. Handlers extract a `RequestContext` and (1)
/// use `now` for any `happened_on` / `recorded_at` they stamp, (2)
/// include `simulated` in event payloads so the audit log carries
/// the tag forever.
#[derive(Debug, Clone, Copy)]
pub struct RequestContext {
    pub now: DateTime<Utc>,
    pub simulated: bool,
}

/// Resolve a request's `(now, simulated)` from its `X-Sim-Time`
/// header value. Caller does the header extraction so this stays
/// axum-agnostic.
///
/// - Header present + parseable → `now` is the parsed instant,
///   `simulated = true`.
/// - Header present but unparseable → `now` is wallclock,
///   `simulated = true`. (Garbage on the header still indicates a
///   sim intent; treating it as live would hide a sim bug.)
/// - Header absent → `now` is wallclock, `simulated = false`.
pub fn request_context(sim_time_header: Option<&str>) -> RequestContext {
    let simulated = sim_time_header.is_some();
    let now = sim_time_header
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    RequestContext { now, simulated }
}

/// Shortcut for handlers that only need `now`. Equivalent to
/// `request_context(header).now`. Use this when the simulated
/// marker isn't going into any event payload from this handler.
pub fn request_now(sim_time_header: Option<&str>) -> DateTime<Utc> {
    request_context(sim_time_header).now
}

/// What time is it. Implementations decide. Used by the sim engine
/// (which drives its own SimClock) and by tests that need
/// deterministic dates. NOT plumbed into service state — services
/// read `now` from the clock service via `ClockClient`.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    fn today(&self) -> NaiveDate {
        self.now().date_naive()
    }
}

/// Reads the system clock at every `now()` call.
#[derive(Debug, Default, Clone, Copy)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Reads a stored instant. The sim engine calls `advance_to(...)`
/// per tick to step its clock forward. Concurrent readers see the
/// most-recently-set value.
#[derive(Debug, Clone)]
pub struct SimClock {
    current: Arc<RwLock<DateTime<Utc>>>,
}

impl SimClock {
    pub fn new(initial: DateTime<Utc>) -> Self {
        Self {
            current: Arc::new(RwLock::new(initial)),
        }
    }

    pub fn advance_to(&self, when: DateTime<Utc>) {
        if let Ok(mut guard) = self.current.write() {
            *guard = when;
        }
    }
}

impl Clock for SimClock {
    fn now(&self) -> DateTime<Utc> {
        match self.current.read() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

/// Reads a constant instant. Tests instantiate this so asserted
/// dates don't drift with wallclock.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    at: DateTime<Utc>,
}

impl FixedClock {
    pub fn new(at: DateTime<Utc>) -> Self {
        Self { at }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_context_header_absent_is_wallclock_not_simulated() {
        let ctx = request_context(None);
        assert!(!ctx.simulated);
        // `now` is wallclock, within a small window of "right now":
        let drift = (Utc::now() - ctx.now).num_seconds().abs();
        assert!(drift < 5, "wallclock fallback should track Utc::now");
    }

    #[test]
    fn request_context_header_parses_and_marks_simulated() {
        let ctx = request_context(Some("2025-08-15T00:00:00Z"));
        let expected = chrono::DateTime::parse_from_rfc3339("2025-08-15T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(ctx.now, expected);
        assert!(ctx.simulated);
    }

    #[test]
    fn request_context_garbage_header_falls_to_wallclock_but_marks_simulated() {
        let ctx = request_context(Some("not a date"));
        assert!(ctx.simulated, "any header presence is a sim signal");
        // `now` is wallclock fallback (we don't want garbage to
        // silently mis-date events at some far-future instant).
        let drift = (Utc::now() - ctx.now).num_seconds().abs();
        assert!(drift < 5);
    }

    #[test]
    fn request_now_matches_request_context_now() {
        let h = Some("2026-01-15T08:30:00Z");
        assert_eq!(request_now(h), request_context(h).now);
    }

    #[test]
    fn wallclock_reads_system_time() {
        let c = WallClock;
        let a = c.now();
        let b = c.now();
        assert!(b >= a, "wallclock should be monotonic at this granularity");
    }

    #[test]
    fn sim_clock_advance_applies_to_subsequent_reads() {
        let start = chrono::DateTime::parse_from_rfc3339("2025-04-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = chrono::DateTime::parse_from_rfc3339("2025-04-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let c = SimClock::new(start);
        assert_eq!(c.now(), start);
        c.advance_to(next);
        assert_eq!(c.now(), next);
    }

    #[test]
    fn fixed_clock_never_drifts() {
        let when = chrono::DateTime::parse_from_rfc3339("2026-01-15T08:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let c = FixedClock::new(when);
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert_eq!(c.now(), when);
    }
}
