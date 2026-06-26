//! Business calendars — named sets of days the sim treats as
//! "non-working" for delay-shifting and demand-multiplier purposes.
//! Counterparty + Periodic engines use these to model "pay clears in
//! 2 business days" rather than "pay clears in 2 days"; the
//! shape-driven sampler uses the `us-banking` calendar to suppress
//! weekday production on federal holidays.
//!
//! Calendars are **DATA**, not code: every value here is a
//! [`boss_core::calendar::BusinessCalendar`] (Sat/Sun `weekend` +
//! concrete `closed` dates), the single source of truth shared with
//! the dispatcher's timing triggers and the boss-calendar service.
//! The daemon fetches them from boss-calendar via
//! `boss-calendar-client`; tests build them inline via
//! [`CalendarRegistry::for_tests`].
//!
//! Unknown calendar names fall back to an all-business calendar
//! (every day is a business day) — a permissive default so a typo in
//! tenant.toml doesn't silently swallow events.

pub mod registry;

// Re-export the canonical data type so callers can refer to
// `crate::calendar::BusinessCalendar` without depending on boss-core
// directly.
pub use boss_core::calendar::BusinessCalendar;
pub use registry::CalendarRegistry;
