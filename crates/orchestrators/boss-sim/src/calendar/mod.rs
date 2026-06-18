//! Business calendars — named sets of days the sim treats as
//! "non-working" for delay-shifting purposes. Counterparty engines
//! use these to model "pay clears in 2 business days" rather than
//! "pay clears in 2 days".
//!
//! Built-in calendars: `us-banking` (federal holidays + weekends)
//! and `us-tax` (banking + 4 quarterly tax-filing windows where
//! state authorities historically slow down).
//!
//! Tenants can register additional calendars at engine-construction
//! time. Unknown calendar names fall back to "every day is a
//! business day" — same permissive policy as the rest of the sim.

pub mod registry;
pub mod us_banking;
pub mod us_tax;

pub use registry::{BusinessCalendar, CalendarRegistry};
