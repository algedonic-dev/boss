//! Shared types for the global-calendar primitive.
//!
//! A calendar reservation claims a time window on a **Subject** — a
//! reservation is always on a subject (the employee, asset, account,
//! … being scheduled). Which subject kinds may be reserved is data:
//! a `calendar_reservable` flag on the subject_kinds registry, not a
//! closed type here. The load-bearing "no two hard reservations
//! overlap on one subject" invariant is enforced by a Postgres GIST
//! exclusion constraint keyed on `(subject_kind, subject_id, window)`.
//!
//! Lives in `boss-core` because every domain crate needs to build a
//! `ReservationRequest` without taking a dep on `boss-calendar`.
//!
//! Decision record: `docs/architecture-decisions.md` §Calendar.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::define_id;
use crate::job::Subject;

define_id!(ReservationId);

/// Stable composite key for a reserved subject — `<subject_kind>:<id>`.
/// Postgres builds the same string for the exclusion-constraint key, so
/// in-memory adapters use this for their own collision checks. (The
/// "what can be reserved" question is data — the `calendar_reservable`
/// flag on the subject_kinds registry — not a closed type.)
pub fn reservation_key(subject: &Subject) -> String {
    format!("{}:{}", subject.kind, subject.id)
}

/// Conventional `reason_kind` tags — the reasons BOSS itself emits.
/// `reason_kind` is a free-form string on the reservation, so a tenant
/// can use its own reason without a core change (what the old `Custom`
/// variant existed to allow — now just "any other string"). These
/// consts keep the well-known set spelled one way across the callers,
/// the seed data, and the SPA's reason labels.
pub mod reason {
    pub const JOB_STEP: &str = "job-step";
    pub const PREVENTIVE_MAINTENANCE_VISIT: &str = "preventive-maintenance-visit";
    pub const TRAINING: &str = "training";
    pub const PTO: &str = "pto";
    pub const MEETING: &str = "meeting";
    pub const TRAVEL: &str = "travel";
}

/// Hard reservations participate in the exclusion constraint —
/// Postgres refuses a conflicting INSERT. Soft reservations can
/// overlap each other and overlap hards (warning at the UI, not a
/// 409). See Q2 in the design doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReservationStrength {
    Hard,
    Soft,
}

impl ReservationStrength {
    pub fn db_value(&self) -> &'static str {
        match self {
            ReservationStrength::Hard => "hard",
            ReservationStrength::Soft => "soft",
        }
    }
}

/// Half-open time window `[start, end)`. Stored UTC per Q1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl TimeWindow {
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Self, &'static str> {
        if end <= start {
            return Err("TimeWindow end must be strictly after start");
        }
        Ok(Self { start, end })
    }

    /// True iff the two windows share any point. Half-open means
    /// `[10:00, 11:00)` and `[11:00, 12:00)` do *not* overlap.
    pub fn overlaps(&self, other: &TimeWindow) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn duration_seconds(&self) -> i64 {
        (self.end - self.start).num_seconds()
    }
}

/// Input for `CalendarClient::reserve`. The implementation assigns
/// a new `ReservationId` and a `created_at` timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationRequest {
    /// The subject being reserved. Its kind must be
    /// `calendar_reservable` in the subject_kinds registry (enforced by
    /// the calendar on reserve); any individual subject can hold only
    /// one hard reservation per overlapping window.
    pub subject: Subject,
    pub window: TimeWindow,
    /// Free-form reason tag — see the `reason` module for the
    /// conventional values. Any string is valid (the old `Custom`
    /// escape hatch is now just "any other string").
    pub reason_kind: String,
    /// Stable identifier of the thing this reservation is for —
    /// a JobId, a PmScheduleId, a TrainingSessionId, etc. Used
    /// for cancellation cascade (delete every reservation whose
    /// `reason_ref_id` equals X) and for UI rendering ("this is
    /// blocking your tech because of Job-12345").
    pub reason_ref_id: String,
    pub strength: ReservationStrength,
    /// Free-form context shown to humans. Optional.
    #[serde(default)]
    pub notes: Option<String>,
    /// Actor making the reservation — employee id, "system-cron",
    /// "boss-jobs-api", etc. Recorded as `created_by`.
    pub created_by: String,
}

/// One row from `calendar_reservations`. What `CalendarClient::list`
/// returns and what conflict errors carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub id: ReservationId,
    pub subject: Subject,
    pub window: TimeWindow,
    /// Free-form reason tag — see the `reason` module for the
    /// conventional values. Any string is valid (the old `Custom`
    /// escape hatch is now just "any other string").
    pub reason_kind: String,
    pub reason_ref_id: String,
    pub strength: ReservationStrength,
    pub notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 27, h, m, 0).unwrap()
    }

    #[test]
    fn reservation_key_is_subject_kind_colon_id() {
        assert_eq!(
            reservation_key(&Subject::new("employee", "emp-001")),
            "employee:emp-001"
        );
        assert_eq!(
            reservation_key(&Subject::new("asset", "sys-001")),
            "asset:sys-001"
        );
        assert_eq!(
            reservation_key(&Subject::new("account", "acc-mercy")),
            "account:acc-mercy"
        );
    }

    #[test]
    fn time_window_rejects_zero_or_negative_duration() {
        assert!(TimeWindow::new(t(10, 0), t(10, 0)).is_err());
        assert!(TimeWindow::new(t(11, 0), t(10, 0)).is_err());
        assert!(TimeWindow::new(t(10, 0), t(11, 0)).is_ok());
    }

    #[test]
    fn time_window_overlap_is_strict_half_open() {
        let a = TimeWindow::new(t(10, 0), t(11, 0)).unwrap();
        let b = TimeWindow::new(t(11, 0), t(12, 0)).unwrap();
        // [10, 11) and [11, 12) touch but don't overlap.
        assert!(!a.overlaps(&b));
        assert!(!b.overlaps(&a));
        // [10:30, 11:30) and [11, 12) do overlap.
        let c = TimeWindow::new(t(10, 30), t(11, 30)).unwrap();
        assert!(c.overlaps(&b));
        assert!(b.overlaps(&c));
        // Containment is overlap.
        let outer = TimeWindow::new(t(9, 0), t(13, 0)).unwrap();
        let inner = TimeWindow::new(t(10, 0), t(11, 0)).unwrap();
        assert!(outer.overlaps(&inner));
        assert!(inner.overlaps(&outer));
    }

    #[test]
    fn reservation_request_round_trips_through_json() {
        let req = ReservationRequest {
            subject: Subject::new("employee", "emp-042"),
            window: TimeWindow::new(t(14, 0), t(16, 0)).unwrap(),
            reason_kind: reason::JOB_STEP.to_string(),
            reason_ref_id: "stp-xyz".into(),
            strength: ReservationStrength::Hard,
            notes: Some("urgent repair".into()),
            created_by: "emp-svc-mgr".into(),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: ReservationRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.reason_ref_id, "stp-xyz");
        assert_eq!(back.subject.kind, "employee");
    }
}
