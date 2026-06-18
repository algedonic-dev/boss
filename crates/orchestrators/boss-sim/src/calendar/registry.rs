//! `BusinessCalendar` trait + lookup registry. Counterparty + Periodic
//! engines hold a `CalendarRegistry` and consult it whenever a delay
//! or cadence is qualified by a calendar name. Lookup misses fall
//! back to a permissive "every day is a business day" calendar so a
//! typo in tenant.toml doesn't silently swallow events.

use std::collections::HashMap;

use chrono::{Datelike, NaiveDate, Weekday};

/// Predicate trait. Implementations decide whether a given day is a
/// business day for their domain.
pub trait BusinessCalendar: Send + Sync {
    /// Stable identifier (e.g. `"us-banking"`, `"us-tax"`,
    /// `"uk-banking"`).
    fn name(&self) -> &str;

    /// Returns true iff `day` is a business day under this calendar.
    fn is_business_day(&self, day: NaiveDate) -> bool;

    /// Walk forward from `day` until a business day is found. Returns
    /// `day` itself when it's already a business day. Used to
    /// "round up" delay-queue dates so a Saturday-due settlement
    /// fires on Monday.
    fn next_business_day(&self, day: NaiveDate) -> NaiveDate {
        let mut d = day;
        while !self.is_business_day(d) {
            d = d.succ_opt().expect("date sequence overflow");
        }
        d
    }

    /// Add `n` business days to `day`. `n=0` returns `day` (or the
    /// next business day if `day` itself is non-business). Negative
    /// `n` walks backward.
    fn add_business_days(&self, day: NaiveDate, n: i64) -> NaiveDate {
        if n == 0 {
            return self.next_business_day(day);
        }
        let mut d = day;
        let mut remaining = n.unsigned_abs();
        let step = if n > 0 { 1 } else { -1 };
        while remaining > 0 {
            d = if step > 0 {
                d.succ_opt().expect("date sequence overflow")
            } else {
                d.pred_opt().expect("date sequence underflow")
            };
            if self.is_business_day(d) {
                remaining -= 1;
            }
        }
        d
    }
}

/// Permissive fallback: every day is a business day. Used when a
/// counterparty/periodic spec names a calendar that nobody has
/// registered (typo, tenant-side calendar shipped later, etc.).
pub struct EveryDay;

impl BusinessCalendar for EveryDay {
    fn name(&self) -> &str {
        "every-day"
    }
    fn is_business_day(&self, _day: NaiveDate) -> bool {
        true
    }
}

/// Skips Sat/Sun only. Useful as a parent calendar for the built-ins.
pub struct WeekdaysOnly;

impl BusinessCalendar for WeekdaysOnly {
    fn name(&self) -> &str {
        "weekdays-only"
    }
    fn is_business_day(&self, day: NaiveDate) -> bool {
        !matches!(day.weekday(), Weekday::Sat | Weekday::Sun)
    }
}

/// Lookup map of calendar name → BusinessCalendar. Engines read this
/// when they need to resolve a tenant.toml `business_calendar = "..."`
/// reference.
pub struct CalendarRegistry {
    calendars: HashMap<String, Box<dyn BusinessCalendar>>,
    fallback: Box<dyn BusinessCalendar>,
}

impl CalendarRegistry {
    /// New registry with no calendars; lookups fall through to
    /// `EveryDay`. Callers usually want `with_builtins()` instead.
    pub fn empty() -> Self {
        Self {
            calendars: HashMap::new(),
            fallback: Box::new(EveryDay),
        }
    }

    /// Pre-loaded with `us-banking` and `us-tax`.
    pub fn with_builtins() -> Self {
        let mut r = Self::empty();
        r.register(Box::new(super::us_banking::UsBanking));
        r.register(Box::new(super::us_tax::UsTax));
        r.register(Box::new(WeekdaysOnly));
        r
    }

    pub fn register(&mut self, cal: Box<dyn BusinessCalendar>) {
        self.calendars.insert(cal.name().to_string(), cal);
    }

    /// Resolve by name; falls back to `EveryDay` on miss.
    pub fn get<'a>(&'a self, name: Option<&str>) -> &'a dyn BusinessCalendar {
        match name {
            Some(n) => self
                .calendars
                .get(n)
                .map(|b| b.as_ref())
                .unwrap_or_else(|| self.fallback.as_ref()),
            None => self.fallback.as_ref(),
        }
    }
}

impl Default for CalendarRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn weekdays_only_skips_weekends() {
        let cal = WeekdaysOnly;
        // 2026-04-25 is a Saturday.
        assert!(!cal.is_business_day(d(2026, 4, 25)));
        assert!(!cal.is_business_day(d(2026, 4, 26)));
        // 2026-04-27 is Monday.
        assert!(cal.is_business_day(d(2026, 4, 27)));
    }

    #[test]
    fn next_business_day_rounds_to_monday_from_friday_evening() {
        let cal = WeekdaysOnly;
        // Saturday → Monday.
        assert_eq!(cal.next_business_day(d(2026, 4, 25)), d(2026, 4, 27));
        // Sunday → Monday.
        assert_eq!(cal.next_business_day(d(2026, 4, 26)), d(2026, 4, 27));
        // Friday → Friday (already business day).
        assert_eq!(cal.next_business_day(d(2026, 4, 24)), d(2026, 4, 24));
    }

    #[test]
    fn add_business_days_skips_weekends() {
        let cal = WeekdaysOnly;
        // Friday + 1 → Monday.
        assert_eq!(cal.add_business_days(d(2026, 4, 24), 1), d(2026, 4, 27));
        // Friday + 5 → Friday next week.
        assert_eq!(cal.add_business_days(d(2026, 4, 24), 5), d(2026, 5, 1));
        // Monday - 1 → previous Friday.
        assert_eq!(cal.add_business_days(d(2026, 4, 27), -1), d(2026, 4, 24));
    }

    #[test]
    fn add_zero_business_days_rounds_to_next_business() {
        let cal = WeekdaysOnly;
        // Saturday + 0 → Monday (next business).
        assert_eq!(cal.add_business_days(d(2026, 4, 25), 0), d(2026, 4, 27));
        // Friday + 0 → Friday (already business).
        assert_eq!(cal.add_business_days(d(2026, 4, 24), 0), d(2026, 4, 24));
    }

    #[test]
    fn registry_falls_back_to_every_day_for_unknown_name() {
        let r = CalendarRegistry::with_builtins();
        let cal = r.get(Some("not-a-real-calendar"));
        // EveryDay treats Saturday as a business day.
        assert!(cal.is_business_day(d(2026, 4, 25)));
    }

    #[test]
    fn registry_returns_us_banking_when_named() {
        let r = CalendarRegistry::with_builtins();
        let cal = r.get(Some("us-banking"));
        assert_eq!(cal.name(), "us-banking");
    }

    #[test]
    fn registry_default_loads_builtins() {
        let r = CalendarRegistry::default();
        assert_eq!(r.get(Some("us-banking")).name(), "us-banking");
        assert_eq!(r.get(Some("us-tax")).name(), "us-tax");
    }
}
