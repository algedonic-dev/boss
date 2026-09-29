//! Domain event subjects for calendar operations.
//!
//! Per `docs/design/projection-rebuilders.md`: state events carry
//! the full `Reservation` row state so the rebuild path can
//! reconstruct `calendar_reservations` from the event log alone.
//!
//! - `reserved` — full `Reservation` payload (post-INSERT row state).
//! - `cancelled` — full `Reservation` payload (post-cancel row state
//!   with `cancelled_at` set), one event per row a cancel-by-reason
//!   cascade affected.

pub const RESERVATION_RESERVED: &str = "calendar.reservation.reserved";
pub const RESERVATION_CANCELLED: &str = "calendar.reservation.cancelled";

/// The fact a business-calendar batch leaves for a code it INSERTED:
/// the row as written, the batch's `mode`, and `declared_by` — the
/// actor the request signed with (backlog 05f61acf, 2026-09-28; the
/// calendar half of 06590554). One per inserted code, none for a kept
/// one, none per batch. Named for the policy resource
/// (`business-calendar`), not under `calendar.reservation.`, so the
/// reservation rebuilder's `calendar.reservation.%` read never sees it.
pub const BUSINESS_CALENDAR_DECLARED: &str = "business-calendar.declared";

/// The fact a `?mode=take` leaves for a held code it REPLACED: the row
/// as written, `mode`, each change from → to (so the closed-day set a
/// take overwrote is in the log, not only in the answer the caller
/// read), and `updated_by`. None for a take that restates the held row.
pub const BUSINESS_CALENDAR_UPDATED: &str = "business-calendar.updated";
