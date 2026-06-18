//! `us-banking` — US Federal Reserve holiday calendar plus weekends.
//! Source: Federal Reserve System holiday schedule. List covers
//! 2024–2030 hand-curated; extend as the sim window grows.
//!
//! When a federal holiday falls on Saturday, banks observe the
//! preceding Friday; when it falls on Sunday, the following Monday.
//! Both are baked into the list — no Sat/Sun roll math at runtime.

use chrono::{Datelike, NaiveDate, Weekday};

use crate::calendar::registry::BusinessCalendar;

pub struct UsBanking;

impl BusinessCalendar for UsBanking {
    fn name(&self) -> &str {
        "us-banking"
    }

    fn is_business_day(&self, day: NaiveDate) -> bool {
        if matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            return false;
        }
        !is_us_federal_holiday(day)
    }
}

/// True if `day` is on the US Federal Reserve holiday list — `false`
/// for Saturdays and Sundays even though those are also non-business
/// days (the weekend check is already in the rate sampler). Exposed
/// so the JobRate sampler can apply weekend-multiplier semantics on
/// holidays without importing the trait.
pub fn is_us_federal_holiday(day: NaiveDate) -> bool {
    HOLIDAYS
        .iter()
        .any(|(y, m, d)| *y == day.year() && *m == day.month() && *d == day.day())
}

/// (year, month, day) tuples for federal-bank holidays observed
/// 2024–2030. New Year's Day, MLK Day, Presidents Day, Memorial Day,
/// Juneteenth, Independence Day, Labor Day, Columbus Day, Veterans
/// Day, Thanksgiving, Christmas. Sat-falling holidays roll back to
/// Friday; Sun-falling holidays roll forward to Monday.
const HOLIDAYS: &[(i32, u32, u32)] = &[
    // 2024
    (2024, 1, 1),   // New Year's Day
    (2024, 1, 15),  // MLK Day
    (2024, 2, 19),  // Presidents Day
    (2024, 5, 27),  // Memorial Day
    (2024, 6, 19),  // Juneteenth
    (2024, 7, 4),   // Independence Day
    (2024, 9, 2),   // Labor Day
    (2024, 10, 14), // Columbus Day
    (2024, 11, 11), // Veterans Day
    (2024, 11, 28), // Thanksgiving
    (2024, 12, 25), // Christmas
    // 2025
    (2025, 1, 1),
    (2025, 1, 20),
    (2025, 2, 17),
    (2025, 5, 26),
    (2025, 6, 19),
    (2025, 7, 4),
    (2025, 9, 1),
    (2025, 10, 13),
    (2025, 11, 11),
    (2025, 11, 27),
    (2025, 12, 25),
    // 2026
    (2026, 1, 1),
    (2026, 1, 19),
    (2026, 2, 16),
    (2026, 5, 25),
    (2026, 6, 19),
    (2026, 7, 3), // 7/4 is Saturday → Friday observed
    (2026, 9, 7),
    (2026, 10, 12),
    (2026, 11, 11),
    (2026, 11, 26),
    (2026, 12, 25),
    // 2027
    (2027, 1, 1),
    (2027, 1, 18),
    (2027, 2, 15),
    (2027, 5, 31),
    (2027, 6, 18), // 6/19 is Saturday → Friday observed
    (2027, 7, 5),  // 7/4 is Sunday → Monday observed
    (2027, 9, 6),
    (2027, 10, 11),
    (2027, 11, 11),
    (2027, 11, 25),
    (2027, 12, 24), // 12/25 is Saturday → Friday observed
    // 2028
    (2028, 1, 17),
    (2028, 2, 21),
    (2028, 5, 29),
    (2028, 6, 19),
    (2028, 7, 4),
    (2028, 9, 4),
    (2028, 10, 9),
    (2028, 11, 10), // 11/11 is Saturday → Friday observed
    (2028, 11, 23),
    (2028, 12, 25),
    // 2029
    (2029, 1, 1),
    (2029, 1, 15),
    (2029, 2, 19),
    (2029, 5, 28),
    (2029, 6, 19),
    (2029, 7, 4),
    (2029, 9, 3),
    (2029, 10, 8),
    (2029, 11, 12), // 11/11 is Sunday → Monday observed
    (2029, 11, 22),
    (2029, 12, 25),
    // 2030
    (2030, 1, 1),
    (2030, 1, 21),
    (2030, 2, 18),
    (2030, 5, 27),
    (2030, 6, 19),
    (2030, 7, 4),
    (2030, 9, 2),
    (2030, 10, 14),
    (2030, 11, 11),
    (2030, 11, 28),
    (2030, 12, 25),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn weekends_are_not_business_days() {
        let cal = UsBanking;
        assert!(!cal.is_business_day(d(2026, 4, 25))); // Sat
        assert!(!cal.is_business_day(d(2026, 4, 26))); // Sun
    }

    #[test]
    fn fixed_holidays_are_not_business_days() {
        let cal = UsBanking;
        assert!(!cal.is_business_day(d(2026, 1, 1))); // New Year's
        assert!(!cal.is_business_day(d(2026, 7, 3))); // Independence Day observed
        assert!(!cal.is_business_day(d(2026, 12, 25))); // Christmas
    }

    #[test]
    fn ordinary_weekdays_are_business_days() {
        let cal = UsBanking;
        assert!(cal.is_business_day(d(2026, 4, 27))); // Mon
        assert!(cal.is_business_day(d(2026, 4, 28))); // Tue
        assert!(cal.is_business_day(d(2026, 4, 29))); // Wed
    }

    #[test]
    fn add_business_days_skips_holiday_and_weekend() {
        let cal = UsBanking;
        // Thursday 2026-12-24 + 1 business day → 2026-12-28 (skip
        // Christmas Friday + weekend).
        assert_eq!(cal.add_business_days(d(2026, 12, 24), 1), d(2026, 12, 28));
    }
}
