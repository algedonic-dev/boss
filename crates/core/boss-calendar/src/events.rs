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
