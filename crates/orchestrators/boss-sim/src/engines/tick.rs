//! `Tick` — the unit the shape-driven engine advances by.
//!
//! Generators express rates per sim-day (`[job_rates.X.rate] = 5.0`
//! = 5 Jobs/day); the engine scales per-tick math by
//! `tick.day_fraction()` so the per-day expected volume stays
//! invariant whether we tick once a day or 24 times.
//!
//! ## Math invariants
//!
//! Two equivalences hold exactly:
//!
//! 1. **Poisson rate**. `lambda_per_tick = rate × tick.day_fraction`,
//!    summed over `1 / day_fraction` ticks per sim-day = `rate`.
//!    A `[job_rates.X.rate = 5.0]` block fires ~5 Jobs per sim-day
//!    whether the tick is `Tick::day()` or `Tick::hour()`.
//!
//! 2. **Step-completion roll**. Per-tick completion probability:
//!    `1 - exp(-tick.day_fraction × SIM_DAY_HOURS / typical)`.
//!    Per-sim-day non-completion probability:
//!    `(1 - p_tick)^N where N = 1/day_fraction`.
//!    `(exp(-day_fraction × C))^(1/day_fraction) = exp(-C)`.
//!    So per-day completion stays `1 - exp(-SIM_DAY_HOURS / typical)`
//!    independent of tick granularity.
//!
//! Both identities hold because Poisson + exponential are
//! infinitely divisible. Variance changes (24 small samples vs
//! 1 big sample) but expected per-day volume does not.
//!
//! ## Day-anchored mechanisms
//!
//! Cadence rows (`subject_cadence`), the Periodic engine, and the
//! Counterparty queue drain are day-anchored: they gate on
//! `tick.is_first_in_day` so they fire once per sim-day, not once
//! per tick.

/// The unit the shape-driven engine advances by. A `Tick` carries
/// just enough context for the per-tick math to scale correctly
/// while preserving per-day expected volume.
///
/// ## Default
///
/// Use [`Tick::day`] for the day-tick: one tick covers the whole
/// sim-day.
#[derive(Debug, Clone, Copy)]
pub struct Tick {
    /// Sim-time advanced by this tick, in hours of calendar time.
    /// `24.0` = a full sim-day. `1.0` = a sim-hour. `1.0/60.0` =
    /// a sim-minute. Must be > 0 and <= 24.
    pub duration_hours: f64,
    /// Hour-of-day this tick STARTS at, in `[0.0, 24.0)`.
    /// `0.0` = midnight. `9.0` = 09:00. `9.5` = 09:30. Sub-day
    /// mechanisms (operating-hours filter,
    /// `subject_cadence.time_of_day`, sub-day periodic cadences)
    /// read this to decide whether THIS tick covers their target
    /// time.
    ///
    /// At `Tick::day()`, this is `0.0` (the day-tick covers the
    /// entire day starting at midnight). At hourly ticks, this
    /// runs `0.0, 1.0, 2.0, ..., 23.0` per sim-day.
    pub start_hour_of_day: f64,
    /// True only on the tick that crosses (or starts on) a new
    /// sim-day. Day-anchored mechanisms gate on this so they
    /// fire once per sim-day rather than once per tick.
    ///
    /// At `Tick::day()` granularity, every tick has this set
    /// (every tick IS the start of a new day).
    pub is_first_in_day: bool,
}

impl Tick {
    /// One full sim-day — the day-tick granularity.
    pub const fn day() -> Self {
        Self {
            duration_hours: 24.0,
            start_hour_of_day: 0.0,
            is_first_in_day: true,
        }
    }

    /// One sim-hour starting at `start_hour_of_day`. Day-anchored
    /// mechanisms only fire on the tick that has
    /// `is_first_in_day = true` (operator wires this when
    /// constructing the per-day tick sequence — see
    /// `boss_sim::engines::run_ticks_with_handlers`).
    pub const fn hour(start_hour_of_day: f64, is_first_in_day: bool) -> Self {
        Self {
            duration_hours: 1.0,
            start_hour_of_day,
            is_first_in_day,
        }
    }

    /// Construct a tick of arbitrary duration. The runner is
    /// responsible for ensuring the per-tick durations of one
    /// sim-day sum to 24.0 — uneven splits work but skew the
    /// per-tick variance.
    pub const fn new(duration_hours: f64, start_hour_of_day: f64, is_first_in_day: bool) -> Self {
        Self {
            duration_hours,
            start_hour_of_day,
            is_first_in_day,
        }
    }

    /// Fraction of a sim-day this tick covers (0.0..=1.0).
    /// Every per-day rate gets multiplied by this to derive the
    /// per-tick rate.
    pub fn day_fraction(&self) -> f64 {
        self.duration_hours / 24.0
    }

    /// True iff `target_hour` falls within this tick's window
    /// `[start_hour_of_day, start_hour_of_day + duration_hours)`.
    /// Sub-day mechanisms (cadence + periodic) gate on this to
    /// fire on the right tick within a sim-day rather than always
    /// on tick 0.
    ///
    /// At `Tick::day()`, every target_hour in `[0, 24)` covers
    /// (the tick spans the full day), so day-tick callers don't
    /// need to special-case.
    pub fn covers_hour(&self, target_hour: f64) -> bool {
        target_hour >= self.start_hour_of_day
            && target_hour < self.start_hour_of_day + self.duration_hours
    }
}

impl Default for Tick {
    /// Defaults to `Tick::day()` so a caller that wants the
    /// day-tick gets it without choosing.
    fn default() -> Self {
        Self::day()
    }
}

/// Parse a `"HH:MM"` time-of-day string into a fractional hour
/// (`0.0..24.0`). Used by `subject_cadence.time_of_day` +
/// `operating_hours` window edges + sub-day periodic cadence
/// anchors. Returns `Err` for malformed input.
pub fn parse_hh_mm(s: &str) -> Result<f64, String> {
    let trimmed = s.trim();
    let parts: Vec<&str> = trimmed.split(':').collect();
    if parts.len() != 2 {
        return Err(format!(
            "time `{trimmed}`: expected `HH:MM` (got {} segments)",
            parts.len()
        ));
    }
    let hh: u32 = parts[0]
        .parse()
        .map_err(|_| format!("time `{trimmed}`: hours `{}` must parse as u32", parts[0]))?;
    let mm: u32 = parts[1]
        .parse()
        .map_err(|_| format!("time `{trimmed}`: minutes `{}` must parse as u32", parts[1]))?;
    if hh >= 24 {
        return Err(format!("time `{trimmed}`: hours must be < 24"));
    }
    if mm >= 60 {
        return Err(format!("time `{trimmed}`: minutes must be < 60"));
    }
    Ok((hh as f64) + (mm as f64) / 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_tick_has_full_fraction_and_is_first() {
        let t = Tick::day();
        assert_eq!(t.duration_hours, 24.0);
        assert_eq!(t.start_hour_of_day, 0.0);
        assert_eq!(t.day_fraction(), 1.0);
        assert!(t.is_first_in_day);
    }

    #[test]
    fn hour_tick_has_24th_fraction() {
        let t = Tick::hour(7.0, false);
        assert_eq!(t.duration_hours, 1.0);
        assert_eq!(t.start_hour_of_day, 7.0);
        assert!((t.day_fraction() - 1.0 / 24.0).abs() < 1e-12);
        assert!(!t.is_first_in_day);
    }

    #[test]
    fn new_minute_tick() {
        let t = Tick::new(1.0 / 60.0, 9.5, false);
        assert!((t.day_fraction() - 1.0 / 1440.0).abs() < 1e-12);
        assert_eq!(t.start_hour_of_day, 9.5);
    }

    #[test]
    fn default_is_day_tick() {
        let t = Tick::default();
        assert_eq!(t.duration_hours, 24.0);
        assert_eq!(t.start_hour_of_day, 0.0);
        assert!(t.is_first_in_day);
    }

    #[test]
    fn covers_hour_at_day_tick() {
        // Day tick spans [0, 24), so every legitimate target_hour
        // is covered. Day-tick callers don't need to special-case.
        let t = Tick::day();
        assert!(t.covers_hour(0.0));
        assert!(t.covers_hour(9.0));
        assert!(t.covers_hour(23.999));
        assert!(!t.covers_hour(24.0)); // half-open interval
    }

    #[test]
    fn covers_hour_at_hourly_tick() {
        // Hour tick at 9.0 covers [9, 10).
        let t = Tick::hour(9.0, false);
        assert!(!t.covers_hour(8.999));
        assert!(t.covers_hour(9.0));
        assert!(t.covers_hour(9.5));
        assert!(t.covers_hour(9.999));
        assert!(!t.covers_hour(10.0));
    }

    #[test]
    fn covers_hour_at_15min_tick() {
        // 15-min tick at 9:00 covers [9, 9.25).
        let t = Tick::new(0.25, 9.0, false);
        assert!(t.covers_hour(9.0));
        assert!(t.covers_hour(9.24));
        assert!(!t.covers_hour(9.25));
        assert!(!t.covers_hour(9.5));
    }

    #[test]
    fn parse_hh_mm_round_trip() {
        assert_eq!(parse_hh_mm("00:00").unwrap(), 0.0);
        assert_eq!(parse_hh_mm("09:00").unwrap(), 9.0);
        assert_eq!(parse_hh_mm("09:30").unwrap(), 9.5);
        assert_eq!(parse_hh_mm("18:00").unwrap(), 18.0);
        assert_eq!(parse_hh_mm("23:59").unwrap(), 23.0 + 59.0 / 60.0);
    }

    #[test]
    fn parse_hh_mm_rejects_garbage() {
        assert!(parse_hh_mm("").is_err());
        assert!(parse_hh_mm("9").is_err());
        assert!(parse_hh_mm("9:00:00").is_err());
        assert!(parse_hh_mm("24:00").is_err());
        assert!(parse_hh_mm("10:60").is_err());
        assert!(parse_hh_mm("hh:mm").is_err());
    }
}
