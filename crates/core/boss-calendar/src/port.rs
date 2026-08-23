//! `CalendarClient` — port every caller talks to. Implementations:
//! `InMemoryCalendar` (tests / sim) and `PgCalendar` (production).
//!
//! The trait is deliberately small: reserve, list, cancel. Everything
//! else (find-a-window queries, recurrence, drag-to-reschedule) is
//! deliberately out of scope at v1.

use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};

use boss_core::calendar::{
    BusinessCalendar, Reservation, ReservationId, ReservationRequest, TimeWindow,
};
use boss_core::job::Subject;

#[derive(Debug, thiserror::Error)]
pub enum CalendarError {
    /// One or more existing hard reservations overlap the requested
    /// window on the same resource. Body includes the conflicting
    /// rows so the UI can render "this overlaps Job-12345" without a
    /// second round-trip.
    #[error("hard-reservation conflict on resource (existing rows: {})", existing.len())]
    Conflict { existing: Vec<Reservation> },

    /// The reservation id passed to `cancel` doesn't exist.
    #[error("reservation not found: {0}")]
    NotFound(ReservationId),

    /// Backing store is unhappy. String for opacity at the trait
    /// boundary; postgres impl turns sqlx::Error into this.
    #[error("storage failure: {0}")]
    Storage(String),

    /// Caller's request is malformed (zero-duration window, etc.).
    #[error("invalid request: {0}")]
    Invalid(String),
}

/// OUTBOX (phase 2): every mutation records its domain event on the
/// transactional outbox INSIDE the adapter transaction via the stamp
/// (`boss_events::outbox::record_event_in_tx`); boss-event-relay
/// delivers to audit_log + NATS post-commit. Idempotency guards sit
/// AHEAD of the recording, so a no-op cancel records nothing.
#[async_trait]
pub trait CalendarClient: Send + Sync {
    /// Try to reserve `req.resource` for `req.window`. Returns the
    /// new reservation id on success, or `Conflict` carrying the
    /// existing rows on collision.
    ///
    /// The implementation generates the id; the convenience
    /// overload stamps `created_at = Utc::now()`. Handlers that
    /// emit a domain event use `reserve_at` so the projection write
    /// and the event share one timestamp — required for the
    /// audit_log → projection rebuild path. See
    /// `docs/design/projection-rebuilders.md`.
    async fn reserve(&self, req: ReservationRequest) -> Result<ReservationId, CalendarError> {
        let stamp = EventStamp::new("calendar", ActorId::Automation("platform".into()));
        self.reserve_at(req, stamp.timestamp, &stamp).await
    }
    /// Records `calendar.reservation.reserved` (the full post-INSERT
    /// row state) in-tx.
    async fn reserve_at(
        &self,
        req: ReservationRequest,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<ReservationId, CalendarError>;

    /// List active (non-cancelled) reservations on `resource` whose
    /// window intersects `window`. Caller-side filters (reason_kind,
    /// strength, etc.) live above this trait.
    async fn list(
        &self,
        subject: &Subject,
        window: TimeWindow,
    ) -> Result<Vec<Reservation>, CalendarError>;

    /// Fetch a single reservation by id, regardless of cancellation
    /// state. Used by handlers that need to read back the post-write
    /// row state for event emission.
    async fn get(&self, id: ReservationId) -> Result<Option<Reservation>, CalendarError>;

    /// Soft-delete a reservation. Idempotent — calling twice is the
    /// same as calling once. `actor` is recorded for the audit trail
    /// but has no effect on the row's `created_by`.
    async fn cancel(&self, id: ReservationId, actor: &str) -> Result<(), CalendarError> {
        let stamp = EventStamp::new("calendar", ActorId::Automation("platform".into()));
        self.cancel_at(id, actor, stamp.timestamp, &stamp).await
    }
    /// Records `calendar.reservation.cancelled` (the full post-cancel
    /// row state) in-tx — and ONLY when the cancel actually flipped
    /// the row. An already-cancelled reservation is a full no-op
    /// (before, the handler's fetch-back emitted a DUPLICATE
    /// cancelled event on every repeat cancel).
    async fn cancel_at(
        &self,
        id: ReservationId,
        actor: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), CalendarError>;

    /// Cancel every reservation tied to a given `(reason_kind,
    /// reason_ref_id)` pair. The cascade primitive — boss-jobs calls
    /// this when a step cancels so every reservation for that step
    /// goes away in one call.
    ///
    /// Returns the number of reservations cancelled (0 is fine —
    /// the originating thing might have had no reservations yet).
    async fn cancel_by_reason(
        &self,
        reason_kind: &str,
        reason_ref_id: &str,
        actor: &str,
    ) -> Result<usize, CalendarError> {
        let stamp = EventStamp::new("calendar", ActorId::Automation("platform".into()));
        self.cancel_by_reason_at(reason_kind, reason_ref_id, actor, stamp.timestamp, &stamp)
            .await
    }
    /// Records one `calendar.reservation.cancelled` (full post-cancel
    /// row state) in-tx PER ROW the cascade flipped — replacing the
    /// racy list-then-emit the HTTP handler used to do around the
    /// UPDATE (a row cancelled between the list and the UPDATE got an
    /// event without a fact, and vice versa).
    async fn cancel_by_reason_at(
        &self,
        reason_kind: &str,
        reason_ref_id: &str,
        actor: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<usize, CalendarError>;

    /// Fetch a named business calendar (`us-banking`, `us-tax`, …) with
    /// its full `closed`-day set. `None` if no calendar with that code
    /// exists. Callers run the business-day math locally via the
    /// `boss_core::calendar::BusinessCalendar` methods.
    async fn get_business_calendar(
        &self,
        code: &str,
    ) -> Result<Option<BusinessCalendar>, CalendarError>;

    /// Seed/replace business calendars. Each calendar is upserted by
    /// `code` and its `closed`-day set is replaced wholesale. Returns the
    /// number of calendars upserted.
    async fn upsert_business_calendars(
        &self,
        calendars: &[BusinessCalendar],
    ) -> Result<usize, CalendarError>;
}
