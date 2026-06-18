//! `us-tax` — US Federal banking calendar plus a small set of
//! tax-deadline windows where state revenue authorities historically
//! delay processing. Used by tax-authority counterparties to model
//! the "ack arrived 5 business days late around the April-15 surge"
//! pattern without per-jurisdiction calendars.
//!
//! v1 carries only the federal-banking baseline + April-15 surge
//! window. Per-state quirks land as separate registered calendars.

use chrono::NaiveDate;

use crate::calendar::registry::BusinessCalendar;
use crate::calendar::us_banking::UsBanking;

pub struct UsTax;

impl BusinessCalendar for UsTax {
    fn name(&self) -> &str {
        "us-tax"
    }

    fn is_business_day(&self, day: NaiveDate) -> bool {
        // First the banking baseline: weekend or federal holiday →
        // not a business day.
        if !UsBanking.is_business_day(day) {
            return false;
        }
        // April-15 surge: the week leading up to and following
        // April 15 each year, state revenue depts are slow. Treating
        // those days as non-business (so deferred filings push out)
        // matches observed behavior — couriers + e-filing portals
        // consistently take longer that week.
        if day.format("%m-%d").to_string().as_str() >= "04-12"
            && day.format("%m-%d").to_string().as_str() <= "04-19"
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn april_15_window_is_non_business() {
        let cal = UsTax;
        // Surge window 4/12 - 4/19 each year.
        assert!(!cal.is_business_day(d(2026, 4, 13))); // Mon during surge
        assert!(!cal.is_business_day(d(2026, 4, 15))); // Apr 15 itself
        assert!(!cal.is_business_day(d(2026, 4, 17))); // Fri during surge
    }

    #[test]
    fn outside_surge_inherits_us_banking() {
        let cal = UsTax;
        assert!(cal.is_business_day(d(2026, 4, 27))); // Mon, no surge
        assert!(!cal.is_business_day(d(2026, 12, 25))); // Christmas — banking ban inherited
        assert!(!cal.is_business_day(d(2026, 4, 25))); // Sat — weekend inherited
    }
}
