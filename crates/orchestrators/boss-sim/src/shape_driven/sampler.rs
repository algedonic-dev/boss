//! Pure rate-sampling functions for the shape-driven sim.
//!
//! Two halves:
//!
//! 1. `effective_rate_for_day` — given a `JobRate` and a calendar
//!    day, return the active jobs/day rate after walking ramps and
//!    applying weekday/weekend multipliers. Pure; no RNG.
//!
//! 2. `count_jobs_for_day` — Poisson-sample that rate to get an
//!    integer count of Jobs to create. Threads the Rng so tests can
//!    pin sequences.

use boss_core::calendar::BusinessCalendar;
use chrono::{Datelike, NaiveDate};

use crate::engines::Tick;
use crate::rng::{Rng, poisson_sample};
use crate::shape_driven::tenant::JobRate;

/// Resolve the active jobs/day rate for a JobKind on a given day.
///
/// Walks `JobRate.ramp` to find the latest ramp whose `date <=
/// today` (falls back to `JobRate.rate` if no ramps apply or none
/// have started yet). Then multiplies by the weekday or weekend
/// multiplier and the month-of-year multiplier, defaulting to 1.0
/// when unspecified.
///
/// Non-business days (weekend or holiday, per the supplied
/// `us_banking` calendar DATA) use `weekend_multiplier`. The brewery's
/// weekday-only flows (morning-brew, ingredient-restock,
/// equipment-preventive-maintenance, brewery-hire) all set `weekend_multiplier = 0.0`,
/// so this single rule correctly suppresses production / HR /
/// vendor-receiving on Memorial Day, July 4th, Christmas, etc.
/// without per-rate holiday lists. Tap-launch and seasonal-release,
/// which set `weekend_multiplier >= 1.0`, naturally fire on those
/// holidays — appropriate, since holidays are big bar-traffic days.
///
/// `us_banking` is the `us-banking` calendar fetched from
/// boss-calendar (the single source of truth); "non-business day" is
/// `!us_banking.is_business_day(today)`.
///
/// Returns 0.0 if no ramps apply and `rate` is 0.0 — caller can
/// short-circuit Poisson sampling.
pub fn effective_rate_for_day(
    jr: &JobRate,
    today: NaiveDate,
    us_banking: &BusinessCalendar,
) -> f64 {
    let base = jr
        .ramp
        .iter()
        .filter(|r| r.date <= today)
        .max_by_key(|r| r.date)
        .map(|r| r.rate)
        .unwrap_or(jr.rate);

    let weekday_mult = if !us_banking.is_business_day(today) {
        jr.weekend_multiplier.unwrap_or(1.0)
    } else {
        jr.weekday_multiplier.unwrap_or(1.0)
    };

    let month_mult = jr
        .month_multipliers
        .get(&today.month().to_string())
        .copied()
        .unwrap_or(1.0);

    (base * weekday_mult * month_mult).max(0.0)
}

/// Poisson-sample the day's effective rate to get a Job count.
/// Convenience over `effective_rate_for_day` + `poisson_sample`.
///
/// Equivalent to `count_jobs_for_tick(jr, today, &Tick::day(), rng)`;
/// kept as a free function for the older call sites that haven't
/// migrated to tick-aware sampling.
pub fn count_jobs_for_day(
    jr: &JobRate,
    today: NaiveDate,
    us_banking: &BusinessCalendar,
    rng: &mut Rng,
) -> u32 {
    count_jobs_for_tick(jr, today, &Tick::day(), us_banking, rng)
}

/// Tick-aware Poisson sample. Scales the per-day effective rate by
/// `tick.day_fraction()` so per-day expected volume stays invariant
/// across tick granularities (see `engines::tick::Tick` for the
/// math). At `Tick::day()` granularity this is identical to
/// `count_jobs_for_day`.
///
/// Phase A of the sub-day-tick rollout — TODO.md "Hourly tick
/// granularity for the live sim."
///
/// When `jr.deterministic` is set the Poisson draw is replaced by a
/// fixed daily count — see [`deterministic_count_for_tick`]. This is
/// the production-review path: a brew is reviewed every working day,
/// so a low-rate kind can never drift into a multi-day cold-start
/// drought the way a Poisson(λ) draw can (it's 0 with probability
/// `e^−λ`).
pub fn count_jobs_for_tick(
    jr: &JobRate,
    today: NaiveDate,
    tick: &Tick,
    us_banking: &BusinessCalendar,
    rng: &mut Rng,
) -> u32 {
    if jr.deterministic {
        return deterministic_count_for_tick(jr, today, tick, us_banking);
    }
    let lambda = effective_rate_for_day(jr, today, us_banking) * tick.day_fraction();
    if lambda <= 0.0 {
        return 0;
    }
    poisson_sample(rng, lambda)
}

/// Deterministic daily Job count for a `deterministic = true` rate.
///
/// Returns the rounded full-day effective rate on the
/// `is_first_in_day` tick, and 0 on every other tick of the sim-day.
/// Day-anchored (unscaled by `tick.day_fraction()`) exactly like
/// `subject_cadence` rows, so the per-sim-day total is the rounded
/// effective rate regardless of tick granularity — at `1h` ticks
/// the day's whole count fires once, at midnight, not 24× or
/// rounded-to-zero-per-tick.
///
/// The effective rate already folds ramps + weekday/weekend +
/// month multipliers + US-federal-holiday suppression, so a brew
/// set `rate = 1.0`, `weekday_multiplier = 1.0`,
/// `weekend_multiplier = 0.0` yields exactly one review per Mon–Fri
/// and zero on weekends/holidays. No RNG — the count is a pure
/// function of the calendar day, which keeps replays bit-stable.
pub fn deterministic_count_for_tick(
    jr: &JobRate,
    today: NaiveDate,
    tick: &Tick,
    us_banking: &BusinessCalendar,
) -> u32 {
    if !tick.is_first_in_day {
        return 0;
    }
    let effective = effective_rate_for_day(jr, today, us_banking);
    if effective <= 0.0 {
        return 0;
    }
    effective.round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape_driven::tenant::RampPoint;

    fn jr(rate: f64) -> JobRate {
        JobRate {
            rate,
            ramp: vec![],
            weekday_multiplier: None,
            weekend_multiplier: None,
            subject_distribution: Default::default(),
            subject_cadence: Default::default(),
            month_multipliers: Default::default(),
            deterministic: false,
        }
    }

    /// A daily production-review rate: deterministic, one review per
    /// working day. `rate = 1.0`, `weekday_multiplier = 1.0`,
    /// `weekend_multiplier = 0.0` — the form the five `morning-brew*`
    /// kinds use.
    fn daily_review() -> JobRate {
        JobRate {
            deterministic: true,
            weekday_multiplier: Some(1.0),
            weekend_multiplier: Some(0.0),
            ..jr(1.0)
        }
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// The `us-banking` calendar the sampler consults — Sat+Sun
    /// weekend + the 2026 observed federal holidays (incl. 2026-07-03
    /// observed Independence Day, 2026-12-25 Christmas). Production
    /// fetches the same data from boss-calendar.
    fn cal() -> BusinessCalendar {
        crate::calendar::registry::us_banking_for_tests()
    }

    #[test]
    fn no_ramps_returns_base_rate() {
        let r = effective_rate_for_day(&jr(2.0), date(2026, 1, 5), &cal());
        assert!((r - 2.0).abs() < 1e-9);
    }

    #[test]
    fn ramps_take_effect_at_their_date() {
        let mut j = jr(4.0);
        j.ramp = vec![
            RampPoint {
                date: date(2026, 1, 1),
                rate: 1.0,
            },
            RampPoint {
                date: date(2026, 4, 1),
                rate: 2.5,
            },
            RampPoint {
                date: date(2026, 7, 1),
                rate: 4.0,
            },
        ];
        // Before any ramp date: still 1.0 (the earliest ramp covers it)
        assert_eq!(effective_rate_for_day(&j, date(2026, 1, 1), &cal()), 1.0);
        assert_eq!(effective_rate_for_day(&j, date(2026, 3, 31), &cal()), 1.0);
        // After 2nd ramp
        assert_eq!(effective_rate_for_day(&j, date(2026, 4, 1), &cal()), 2.5);
        assert_eq!(effective_rate_for_day(&j, date(2026, 6, 30), &cal()), 2.5);
        // After 3rd
        assert_eq!(effective_rate_for_day(&j, date(2026, 12, 1), &cal()), 4.0);
    }

    #[test]
    fn before_any_ramp_falls_back_to_base_rate() {
        // If the sim starts before any ramp begins, the base `rate`
        // is the floor.
        let mut j = jr(0.5);
        j.ramp = vec![RampPoint {
            date: date(2026, 6, 1),
            rate: 5.0,
        }];
        // 2026-01-01 is before the ramp — base rate (0.5) applies.
        assert_eq!(effective_rate_for_day(&j, date(2026, 1, 1), &cal()), 0.5);
        // On the ramp date — the ramp wins.
        assert_eq!(effective_rate_for_day(&j, date(2026, 6, 1), &cal()), 5.0);
    }

    #[test]
    fn weekday_multiplier_only_applies_mon_fri() {
        let mut j = jr(1.0);
        j.weekday_multiplier = Some(0.5);
        j.weekend_multiplier = Some(2.0);
        // 2026-01-05 is Monday
        assert_eq!(effective_rate_for_day(&j, date(2026, 1, 5), &cal()), 0.5);
        // 2026-01-03 is Saturday
        assert_eq!(effective_rate_for_day(&j, date(2026, 1, 3), &cal()), 2.0);
        // 2026-01-04 is Sunday
        assert_eq!(effective_rate_for_day(&j, date(2026, 1, 4), &cal()), 2.0);
    }

    #[test]
    fn us_federal_holiday_uses_weekend_multiplier() {
        let mut j = jr(1.0);
        j.weekday_multiplier = Some(2.0);
        j.weekend_multiplier = Some(0.0);
        // 2026-07-03 (Friday) is the observed Independence Day holiday
        // — should use weekend_multiplier (0.0) not weekday (2.0).
        assert_eq!(effective_rate_for_day(&j, date(2026, 7, 3), &cal()), 0.0);
        // 2026-12-25 (Friday) is Christmas — same.
        assert_eq!(effective_rate_for_day(&j, date(2026, 12, 25), &cal()), 0.0);
        // 2026-07-02 (Thursday) is a normal weekday → 2.0.
        assert_eq!(effective_rate_for_day(&j, date(2026, 7, 2), &cal()), 2.0);
    }

    #[test]
    fn evening_facing_flow_still_fires_on_holiday() {
        // Tap-launch sets weekend_multiplier > 1.0 because Sat/Sun
        // are big bar nights. Holidays inherit that — Christmas-eve
        // tap launch is appropriate, not zero.
        let mut j = jr(1.0);
        j.weekday_multiplier = Some(0.7);
        j.weekend_multiplier = Some(1.75);
        assert_eq!(effective_rate_for_day(&j, date(2026, 12, 25), &cal()), 1.75);
    }

    #[test]
    fn month_multiplier_stacks_on_top_of_weekday() {
        let mut j = jr(1.0);
        j.weekday_multiplier = Some(2.0);
        j.weekend_multiplier = Some(0.5);
        j.month_multipliers.insert("7".to_string(), 3.0); // July surge
        j.month_multipliers.insert("1".to_string(), 0.5); // January slump
        // July 2026-07-06 is Monday → 1.0 * 2.0 * 3.0 = 6.0
        assert!((effective_rate_for_day(&j, date(2026, 7, 6), &cal()) - 6.0).abs() < 1e-9);
        // January 2026-01-03 is Saturday → 1.0 * 0.5 * 0.5 = 0.25
        assert!((effective_rate_for_day(&j, date(2026, 1, 3), &cal()) - 0.25).abs() < 1e-9);
        // June 2026-06-15 is Monday, no June multiplier → 1.0 * 2.0 = 2.0
        assert!((effective_rate_for_day(&j, date(2026, 6, 15), &cal()) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn count_jobs_zero_rate_returns_zero() {
        let mut rng = Rng::new(42);
        let count = count_jobs_for_day(&jr(0.0), date(2026, 1, 1), &cal(), &mut rng);
        assert_eq!(count, 0);
    }

    #[test]
    fn count_jobs_is_deterministic_for_a_seed() {
        // Same seed + same rate + same date ⇒ same count, always.
        // This invariant is what makes the sim replayable.
        let j = jr(3.0);
        let day = date(2026, 1, 5);
        let mut rng_a = Rng::new(1234);
        let mut rng_b = Rng::new(1234);
        assert_eq!(
            count_jobs_for_day(&j, day, &cal(), &mut rng_a),
            count_jobs_for_day(&j, day, &cal(), &mut rng_b)
        );
    }

    #[test]
    fn count_jobs_average_tracks_rate_across_a_year() {
        // 365 daily Poisson(2.0) samples ⇒ mean ~2.0. Loose bound
        // since we're only checking that the rate isn't ignored
        // (drift from 2.0 is ~0.05 on this seed).
        let j = jr(2.0);
        let mut rng = Rng::new(0xc0ffee);
        let mut total = 0u64;
        let mut day = date(2026, 1, 1);
        for _ in 0..365 {
            total += count_jobs_for_day(&j, day, &cal(), &mut rng) as u64;
            day = day.succ_opt().unwrap();
        }
        let mean = total as f64 / 365.0;
        assert!(
            (mean - 2.0).abs() < 0.3,
            "expected mean ~2.0 over 365 days, got {mean:.3}"
        );
    }

    #[test]
    fn deterministic_fires_exactly_one_per_weekday() {
        // The cold-start regression: a daily production review must
        // create exactly one Job every weekday, with no Poisson
        // chance of a zero-draw drought. 2026-01-05 is a Monday.
        let dr = daily_review();
        let mut rng = Rng::new(7);
        for offset in 0..5 {
            let day = date(2026, 1, 5) + chrono::Duration::days(offset);
            assert_eq!(
                count_jobs_for_day(&dr, day, &cal(), &mut rng),
                1,
                "weekday {day} should review exactly one brew"
            );
        }
    }

    #[test]
    fn deterministic_skips_weekends() {
        // weekend_multiplier = 0.0 → no review Sat/Sun. 2026-01-03
        // is Saturday, 2026-01-04 is Sunday.
        let dr = daily_review();
        let mut rng = Rng::new(7);
        assert_eq!(
            count_jobs_for_day(&dr, date(2026, 1, 3), &cal(), &mut rng),
            0
        );
        assert_eq!(
            count_jobs_for_day(&dr, date(2026, 1, 4), &cal(), &mut rng),
            0
        );
    }

    #[test]
    fn deterministic_skips_us_federal_holidays() {
        // Holiday exclusion comes free: effective_rate_for_day reads
        // weekend_multiplier (0.0) on a federal holiday even when it
        // falls on a weekday. 2026-07-03 (Fri) is the observed
        // Independence Day; 2026-12-25 (Fri) is Christmas.
        let dr = daily_review();
        let mut rng = Rng::new(7);
        assert_eq!(
            count_jobs_for_day(&dr, date(2026, 7, 3), &cal(), &mut rng),
            0
        );
        assert_eq!(
            count_jobs_for_day(&dr, date(2026, 12, 25), &cal(), &mut rng),
            0
        );
        // A normal weekday around them still reviews.
        assert_eq!(
            count_jobs_for_day(&dr, date(2026, 7, 2), &cal(), &mut rng),
            1
        );
    }

    #[test]
    fn deterministic_is_day_anchored_at_sub_day_ticks() {
        // At hourly ticks the day's whole count fires once on the
        // is_first_in_day tick — not 24× (over-creation) and not
        // rounded-to-zero-per-tick (the naive day_fraction scaling
        // would give round(1.0/24) = 0 every tick → no brews).
        let dr = daily_review();
        let mut rng = Rng::new(7);
        let monday = date(2026, 1, 5);
        // The midnight (first-in-day) tick fires the full count.
        assert_eq!(
            count_jobs_for_tick(&dr, monday, &Tick::hour(0.0, true), &cal(), &mut rng),
            1
        );
        // Every other hourly tick of the day fires nothing.
        let mut rest = 0u32;
        for h in 1..24 {
            rest +=
                count_jobs_for_tick(&dr, monday, &Tick::hour(h as f64, false), &cal(), &mut rng);
        }
        assert_eq!(rest, 0, "non-first ticks must not double-create");
    }

    #[test]
    fn deterministic_week_totals_five_reviews() {
        // A full Mon–Sun week of daily reviews = exactly 5 (the five
        // weekdays), summed across hourly ticks. This is the count
        // the five morning-brew* kinds produce per brewhouse per
        // week before the gate decides brew-vs-skip.
        let dr = daily_review();
        let mut rng = Rng::new(7);
        let mut total = 0u32;
        let mut day = date(2026, 1, 5); // Monday
        for _ in 0..7 {
            for h in 0..24 {
                total +=
                    count_jobs_for_tick(&dr, day, &Tick::hour(h as f64, h == 0), &cal(), &mut rng);
            }
            day = day.succ_opt().unwrap();
        }
        assert_eq!(total, 5, "one review per weekday, none on the weekend");
    }
}
