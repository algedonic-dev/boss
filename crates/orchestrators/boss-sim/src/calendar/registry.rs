//! `CalendarRegistry` — a lookup map of calendar `code` →
//! [`boss_core::calendar::BusinessCalendar`] DATA. Counterparty +
//! Periodic engines hold a registry and consult it whenever a delay
//! or cadence is qualified by a calendar code; the shape-driven
//! sampler consults the `us-banking` calendar for its
//! weekday/weekend/holiday demand multipliers.
//!
//! The calendars are not hardcoded here — they are seeded into the
//! boss-calendar service and fetched at daemon startup via
//! `boss-calendar-client`, so there is one source of truth. Lookup
//! misses fall back to a permissive all-business calendar (every day
//! is a business day) so a typo in tenant.toml doesn't silently
//! swallow events.

use std::collections::HashMap;

use boss_core::calendar::BusinessCalendar;

/// Lookup map of calendar code → [`BusinessCalendar`] data. Engines
/// read this to resolve a tenant.toml `business_calendar = "..."`
/// reference into the concrete non-business-day set.
pub struct CalendarRegistry {
    calendars: HashMap<String, BusinessCalendar>,
    /// Permissive fallback: every day is a business day. Returned on
    /// a lookup miss (unknown code, or a calendar that failed to
    /// fetch) so a missing calendar degrades to "no closures" rather
    /// than dropping events.
    fallback: BusinessCalendar,
}

impl CalendarRegistry {
    /// Build the permissive all-business fallback calendar — empty
    /// weekend, empty closed set, so `is_business_day` is always
    /// true. Built explicitly (not via `BusinessCalendar::new`, which
    /// defaults to a Sat+Sun weekend).
    fn all_business() -> BusinessCalendar {
        BusinessCalendar {
            code: String::new(),
            name: String::new(),
            weekend: std::collections::BTreeSet::new(),
            closed: std::collections::BTreeSet::new(),
        }
    }

    /// Registry holding the supplied calendar data. The map is keyed
    /// by each calendar's `code`; the fallback is the all-business
    /// calendar.
    pub fn from_data(cals: Vec<BusinessCalendar>) -> Self {
        let calendars = cals.into_iter().map(|c| (c.code.clone(), c)).collect();
        Self {
            calendars,
            fallback: Self::all_business(),
        }
    }

    /// Empty registry — no calendars; every lookup falls through to
    /// the all-business fallback. Used when no calendar service is
    /// reachable; engines still run, just without closures.
    pub fn empty() -> Self {
        Self::from_data(Vec::new())
    }

    /// Test registry — `us-banking` + `us-tax` + `weekdays-only`
    /// built inline as DATA, with enough closed dates to satisfy the
    /// engine + sampler unit tests. Production fetches these from the
    /// boss-calendar service instead.
    pub fn for_tests() -> Self {
        Self::from_data(vec![
            us_banking_for_tests(),
            us_tax_for_tests(),
            weekdays_only_for_tests(),
        ])
    }

    /// Resolve by code; falls back to the all-business calendar on a
    /// miss (unknown code or `None`).
    pub fn get(&self, code: Option<&str>) -> &BusinessCalendar {
        match code {
            Some(c) => self.calendars.get(c).unwrap_or(&self.fallback),
            None => &self.fallback,
        }
    }
}

impl Default for CalendarRegistry {
    /// Empty (all-business) by default. Real deployments call
    /// [`CalendarRegistry::from_data`] with calendars fetched from
    /// boss-calendar; tests call [`CalendarRegistry::for_tests`].
    fn default() -> Self {
        Self::empty()
    }
}

/// `us-banking` test calendar: Sat+Sun weekend plus the observed US
/// federal-bank holidays for 2024–2026. Carries the dates the engine
/// + sampler unit tests assert against (e.g. MLK Day 2024-01-15, the
/// 2026-07-03 observed Independence Day, 2026-12-25 Christmas) plus
/// the weekend-snapping cases. Sat-falling holidays roll back to
/// Friday, Sun-falling roll forward to Monday — baked into the list.
///
/// Production fetches the equivalent data from the boss-calendar
/// service; this inline copy exists only so tests + non-daemon paths
/// behave correctly without a fetch.
///
/// `pub(crate)` so `ShapeDrivenState::default` can seed the same
/// us-banking calendar the sampler tests expect when no fetched
/// calendar has been injected.
pub(crate) fn us_banking_for_tests() -> BusinessCalendar {
    use chrono::NaiveDate;
    let d = |y: i32, m: u32, day: u32| NaiveDate::from_ymd_opt(y, m, day).unwrap();
    BusinessCalendar::new("us-banking", "US Banking").with_closed([
        // 2024
        d(2024, 1, 1),   // New Year's Day
        d(2024, 1, 15),  // MLK Day
        d(2024, 2, 19),  // Presidents Day
        d(2024, 5, 27),  // Memorial Day
        d(2024, 6, 19),  // Juneteenth
        d(2024, 7, 4),   // Independence Day
        d(2024, 9, 2),   // Labor Day
        d(2024, 10, 14), // Columbus Day
        d(2024, 11, 11), // Veterans Day
        d(2024, 11, 28), // Thanksgiving
        d(2024, 12, 25), // Christmas
        // 2025
        d(2025, 1, 1),
        d(2025, 1, 20),
        d(2025, 2, 17),
        d(2025, 5, 26),
        d(2025, 6, 19),
        d(2025, 7, 4),
        d(2025, 9, 1),
        d(2025, 10, 13),
        d(2025, 11, 11),
        d(2025, 11, 27),
        d(2025, 12, 25),
        // 2026
        d(2026, 1, 1),
        d(2026, 1, 19),
        d(2026, 2, 16),
        d(2026, 5, 25),
        d(2026, 6, 19),
        d(2026, 7, 3), // 7/4 is Saturday → Friday observed
        d(2026, 9, 7),
        d(2026, 10, 12),
        d(2026, 11, 11),
        d(2026, 11, 26),
        d(2026, 12, 25),
    ])
}

/// `us-tax` test calendar: the `us-banking` baseline plus the
/// Apr 12-19 filing-surge window expanded to concrete closed dates.
fn us_tax_for_tests() -> BusinessCalendar {
    use chrono::NaiveDate;
    let surge = (12..=19).map(|day| NaiveDate::from_ymd_opt(2026, 4, day).unwrap());
    let mut cal = us_banking_for_tests();
    cal.code = "us-tax".to_string();
    cal.name = "US Tax".to_string();
    cal.closed.extend(surge);
    cal
}

/// `weekdays-only` test calendar: Sat+Sun weekend, no holidays. Used
/// by the periodic engine's coarse-cadence postponement test.
fn weekdays_only_for_tests() -> BusinessCalendar {
    BusinessCalendar::new("weekdays-only", "Weekdays Only")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn fallback_is_all_business() {
        let r = CalendarRegistry::empty();
        // No calendars → every day, including Saturday, is a business
        // day under the fallback.
        assert!(r.get(Some("nope")).is_business_day(d(2026, 4, 25))); // Sat
        assert!(r.get(None).is_business_day(d(2026, 4, 26))); // Sun
    }

    #[test]
    fn from_data_resolves_by_code() {
        let r = CalendarRegistry::from_data(vec![us_banking_for_tests()]);
        let cal = r.get(Some("us-banking"));
        assert_eq!(cal.code, "us-banking");
        // Saturday + the observed Independence Day are non-business.
        assert!(!cal.is_business_day(d(2026, 4, 25))); // Sat
        assert!(!cal.is_business_day(d(2026, 7, 3))); // observed July 4
    }

    #[test]
    fn unknown_code_falls_back_to_all_business() {
        let r = CalendarRegistry::for_tests();
        // EveryDay-style fallback treats Saturday as a business day.
        assert!(
            r.get(Some("not-a-real-calendar"))
                .is_business_day(d(2026, 4, 25))
        );
    }

    #[test]
    fn for_tests_loads_the_three_builtins() {
        let r = CalendarRegistry::for_tests();
        assert_eq!(r.get(Some("us-banking")).code, "us-banking");
        assert_eq!(r.get(Some("us-tax")).code, "us-tax");
        assert_eq!(r.get(Some("weekdays-only")).code, "weekdays-only");
        // us-tax inherits the banking closures AND the surge window.
        assert!(!r.get(Some("us-tax")).is_business_day(d(2026, 12, 25))); // banking
        assert!(!r.get(Some("us-tax")).is_business_day(d(2026, 4, 15))); // surge
        // weekdays-only has no holidays — Christmas is a business day.
        assert!(
            r.get(Some("weekdays-only"))
                .is_business_day(d(2026, 12, 25))
        );
    }
}
