//! `CalendarClient` — port every caller talks to. Implementations:
//! `InMemoryCalendar` (tests / sim) and `PgCalendar` (production).
//!
//! The trait is deliberately small: reserve, list, cancel. Everything
//! else (find-a-window queries, recurrence, drag-to-reschedule) is
//! deliberately out of scope at v1.

use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_core::publish::{FieldChange, KeptRow, PublishMode, UpdatedRow};
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

    /// Every business calendar the store holds, sorted by `code`, each
    /// with its full closed-day set — the batch's own input shape, so
    /// `boss tenant export` (design e187198f car 3, backlog e618f3ac)
    /// can write `seeds/business_calendars.json` back from the
    /// instance. Until 2026-09-18 the only read was per code, so an
    /// export could not learn which codes the instance held.
    async fn list_business_calendars(&self) -> Result<Vec<BusinessCalendar>, CalendarError>;

    /// Publish business calendars, insert-if-absent by `code` (design
    /// e187198f: THE INSTANCE IS THE TRUTH). A code the table holds is
    /// KEPT and the outcome names the declared fields it differs on
    /// (`name`, `weekend`, `closed`); only [`PublishMode::Take`]
    /// replaces it — header and closed-day set wholesale — naming each
    /// change from → to. Until 2026-09-18 this was an unconditional
    /// upsert plus a DELETE-and-reinsert of the closed days, and the
    /// tenant publish runs at every boot, so an operator's edit to a
    /// calendar lived until the next converge.
    ///
    /// Each code the call CHANGES records one [`published_fact`] signed
    /// by `stamp` — the actor the door's policy ladder resolved — in the
    /// write's own transaction; a code it leaves as it was is neither
    /// written nor recorded (backlog 05f61acf).
    async fn publish_business_calendars(
        &self,
        calendars: &[BusinessCalendar],
        mode: PublishMode,
        stamp: &EventStamp,
    ) -> Result<BusinessCalendarsOutcome, CalendarError>;
}

/// The fact publishing `declared` leaves, given what the store `held`
/// for its code — and so whether the adapter writes at all: `None` is
/// "nothing changes, write nothing" (a kept row under the default, or a
/// take that restates the held row). One builder for both adapters, so
/// the in-memory double records exactly what the Pg adapter stages.
///
/// WHY (backlog 05f61acf, 2026-09-28). The batch door asked policy and
/// dropped the actor the ladder resolved, and the publish staged no
/// event (06590554), so a `?mode=take` that replaced a held calendar's
/// closed-day set — the days the dispatcher's timing triggers count
/// business days from — left no row, event or log line naming who did
/// it. The payload carries the row as written (value-primary: a
/// rebuilder needs nothing else), `mode`, and the signer under
/// `declared_by` / `updated_by`, read from the stamp so it is the value
/// `_actor` carries.
pub fn published_fact(
    stamp: &EventStamp,
    held: Option<&BusinessCalendar>,
    declared: &BusinessCalendar,
    mode: PublishMode,
) -> Result<Option<boss_core::event::Event>, CalendarError> {
    let row = |extra: serde_json::Value| -> Result<serde_json::Value, CalendarError> {
        let mut payload =
            serde_json::to_value(declared).map_err(|e| CalendarError::Storage(e.to_string()))?;
        if let (serde_json::Value::Object(map), serde_json::Value::Object(more)) =
            (&mut payload, extra)
        {
            map.insert("mode".into(), mode.as_str().into());
            map.extend(more);
        }
        Ok(payload)
    };
    let signer = stamp.actor().to_string();
    match held {
        None => Ok(Some(stamp.event(
            crate::events::BUSINESS_CALENDAR_DECLARED,
            row(serde_json::json!({ "declared_by": signer }))?,
        ))),
        Some(held) if mode.is_take() => {
            let changes = calendar_changes(held, declared);
            if changes.is_empty() {
                return Ok(None);
            }
            Ok(Some(stamp.event(
                crate::events::BUSINESS_CALENDAR_UPDATED,
                row(serde_json::json!({ "changes": changes, "updated_by": signer }))?,
            )))
        }
        Some(_) => Ok(None),
    }
}

/// What a business-calendar batch did: rows received, rows inserted,
/// rows kept with their differing fields named, rows a take updated
/// with each change named, and rows already as declared.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct BusinessCalendarsOutcome {
    pub received: usize,
    pub inserted: usize,
    pub kept: Vec<KeptRow>,
    pub updated: Vec<UpdatedRow>,
    pub unchanged: usize,
}

/// The declared fields `declared` disagrees with the held row on, by
/// the file's field names — the pure comparison both adapters answer
/// a kept row with.
pub fn calendar_differs(held: &BusinessCalendar, declared: &BusinessCalendar) -> Vec<String> {
    let mut out = Vec::new();
    if held.name != declared.name {
        out.push("name".to_string());
    }
    if held.weekend != declared.weekend {
        out.push("weekend".to_string());
    }
    if held.closed != declared.closed {
        out.push("closed".to_string());
    }
    out
}

/// The same disagreement as changes from `held` to `declared` — what a
/// take reports after replacing the row.
pub fn calendar_changes(held: &BusinessCalendar, declared: &BusinessCalendar) -> Vec<FieldChange> {
    let mut out = Vec::new();
    if held.name != declared.name {
        out.push(FieldChange::new("name", &held.name, &declared.name));
    }
    if held.weekend != declared.weekend {
        out.push(FieldChange::new(
            "weekend",
            &held.weekend,
            &declared.weekend,
        ));
    }
    if held.closed != declared.closed {
        out.push(FieldChange::new("closed", &held.closed, &declared.closed));
    }
    out
}

/// One row's contribution to the outcome, shared by both adapters:
/// `held` is what the table had (`None` = inserted), and the adapter
/// has already written what `mode` asked for.
pub fn account_for(
    out: &mut BusinessCalendarsOutcome,
    held: Option<&BusinessCalendar>,
    declared: &BusinessCalendar,
    mode: PublishMode,
) {
    let Some(held) = held else {
        out.inserted += 1;
        return;
    };
    let differs = calendar_differs(held, declared);
    if differs.is_empty() {
        out.unchanged += 1;
    } else if mode.is_take() {
        out.updated.push(UpdatedRow {
            id: declared.code.clone(),
            changes: calendar_changes(held, declared),
        });
    } else {
        out.kept.push(KeptRow {
            id: declared.code.clone(),
            differs,
        });
    }
}
