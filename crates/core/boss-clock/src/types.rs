//! Wire types shared between `boss-clock` (server) and
//! `boss-clock-client` (consumer). Kept here so both crates
//! deserialize against the same definition.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// Response of `GET /api/clock/now`. The single answer every
/// service uses to stamp dates + mark SIM-vs-real on events.
///
/// `simulated` is the audit-log tag — services include it in
/// every event payload they emit, so downstream queries can
/// filter sim activity from real activity without going back
/// to the source.
// No `Eq`: `warp_factor` is `Option<f64>` and f64 is only `PartialEq`
// (same reason `SimClockParams` below isn't `Eq`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ClockNow {
    /// Effective `now` for any handler stamping a date.
    pub now: DateTime<Utc>,
    /// `true` when the clock is in sim mode; `false` in wall
    /// (production) mode. Services include this in event
    /// payloads as the canonical SIM marker.
    pub simulated: bool,
    /// Sim-mode epoch start (the day the sim started). `None`
    /// in wall mode.
    #[serde(default)]
    pub epoch_start: Option<NaiveDate>,
    /// Sim-mode epoch end (auto-pause day). `None` in wall mode.
    #[serde(default)]
    pub epoch_end: Option<NaiveDate>,
    /// Sim-mode pause state. `false` in wall mode (wall clock
    /// never pauses).
    #[serde(default)]
    pub paused: bool,
    /// Sim-mode restart-in-progress signal — the clean-reset
    /// path is mid-flight (audit_log trim + projection
    /// rebuild). Services + UIs can render a spinner. `false`
    /// in wall mode.
    #[serde(default)]
    pub restart_in_progress: bool,
    /// Sim-mode pacing — sim-seconds advanced per wall-second
    /// (`8640.0` = 1 sim-day every 10 wall-seconds). `None` in
    /// wall mode. clock-api owns this, so a UI reading it here is
    /// reading the authoritative warp directly — correct even
    /// while the brewery-sim daemon (which carries its own
    /// `/telemetry`) is stopped, e.g. mid seed-rebuild.
    #[serde(default)]
    pub warp_factor: Option<f64>,
}

impl ClockNow {
    /// Wall-clock answer — no sim mode, just `Utc::now()`. Used
    /// by both the wall-mode binary and by `WallClockClient`
    /// (the in-memory test default).
    pub fn wall() -> Self {
        Self {
            now: Utc::now(),
            simulated: false,
            epoch_start: None,
            epoch_end: None,
            paused: false,
            restart_in_progress: false,
            warp_factor: None,
        }
    }
}

/// The simulated clock's parameters. Sim time is a pure function
/// of (wall_now − wall_anchor) × warp_factor + epoch_start — the
/// clock-api owns these parameters and computes `ClockNow` on
/// every request rather than holding a mutable `current_sim_date`
/// that needs explicit advancing.
///
/// Persisted in the `sim_clock` table so the formula survives
/// clock-api restarts. The brewery-sim daemon (and every other
/// service) is a pure consumer — nobody outside clock-api writes
/// these fields.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct SimClockParams {
    /// The calendar date sim-time started at. Sim now =
    /// epoch_start midnight UTC + sim-elapsed.
    pub epoch_start: NaiveDate,
    /// Optional auto-cap. When sim now reaches this date the
    /// formula stops advancing past it (brewery's 12-month
    /// epoch boundary). `None` = no cap.
    #[serde(default)]
    pub epoch_end: Option<NaiveDate>,
    /// Sim-seconds advanced per wall-second. `1.0` = real time;
    /// `8640.0` = 1 sim-day every 10 wall-seconds (brewery
    /// default for the playground demo). Backtests use very
    /// large values so the run completes in wall-minutes.
    pub warp_factor: f64,
    /// Wall-clock instant the formula's "elapsed" baseline was
    /// last reset (boot, configure, restart-epoch). Serialized
    /// as RFC3339.
    pub wall_anchor: DateTime<Utc>,
    /// `true` while the clock is paused — sim_now stops
    /// advancing until resumed.
    #[serde(default)]
    pub paused: bool,
    /// Wall instant the current pause started. `None` if not
    /// paused. Used to compute `paused_offset` on resume.
    #[serde(default)]
    pub paused_at: Option<DateTime<Utc>>,
    /// Total wall-seconds of accumulated pause time. Subtracted
    /// from (wall_now − wall_anchor) so a pause-then-resume
    /// continues sim-time from where it stopped instead of
    /// jumping forward.
    #[serde(default)]
    pub paused_offset_seconds: f64,
    /// Mid-flight restart signal — `true` while
    /// audit_log-trim + projection-rebuild is running.
    #[serde(default)]
    pub restart_in_progress: bool,
}

impl SimClockParams {
    /// Compute the current sim instant by applying the formula.
    /// Pure function of `self` + wall-clock now; safe to call on
    /// every request.
    pub fn now(&self) -> ClockNow {
        // While paused, sim-time is frozen at the instant the pause
        // began: clamp the wall reference to `paused_at`. Computing
        // the frozen instant directly from `paused_at` (rather than
        // subtracting a live-growing `now − paused_at` term from a
        // live-growing `now − wall_anchor` term) keeps it exact — the
        // two-growing-numbers form drifted by a millisecond or two
        // under high warp factors as the floats rounded independently.
        let wall_now = match (self.paused, self.paused_at) {
            (true, Some(paused_at)) => paused_at,
            _ => Utc::now(),
        };
        let wall_elapsed_secs =
            (wall_now - self.wall_anchor).num_milliseconds().max(0) as f64 / 1000.0;
        let active_wall_secs = (wall_elapsed_secs - self.paused_offset_seconds).max(0.0);
        let sim_elapsed_secs = active_wall_secs * self.warp_factor;
        let epoch_start_dt = self
            .epoch_start
            .and_hms_opt(0, 0, 0)
            .expect("midnight is always valid")
            .and_utc();
        let raw_now =
            epoch_start_dt + chrono::Duration::milliseconds((sim_elapsed_secs * 1000.0) as i64);
        // Cap at epoch_end if configured.
        let now = match self.epoch_end {
            Some(end) => {
                let cap = end
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is always valid")
                    .and_utc();
                if raw_now >= cap { cap } else { raw_now }
            }
            None => raw_now,
        };
        ClockNow {
            now,
            simulated: true,
            epoch_start: Some(self.epoch_start),
            epoch_end: self.epoch_end,
            paused: self.paused,
            restart_in_progress: self.restart_in_progress,
            warp_factor: Some(self.warp_factor),
        }
    }
}

/// Deserialize `Option<Option<T>>` so an absent field and an explicit
/// `null` are distinguishable. serde collapses both to `None`, which
/// makes "leave it alone" and "remove it" the same request.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(de).map(Some)
}

/// Body of `POST /api/clock/configure`. Operators (and the
/// restart-epoch path) use this to reset the formula's
/// parameters. All fields optional — only the supplied ones
/// change. Posting `epoch_start` rebases sim-time to "now"
/// at that date; posting only `warp_factor` changes the rate
/// without resetting the elapsed offset (so a live speedup
/// doesn't teleport the clock).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConfigureRequest {
    /// Reset the formula's wall_anchor + epoch_start in one
    /// step. When supplied without `warp_factor`, the warp
    /// factor is preserved.
    #[serde(default)]
    pub epoch_start: Option<NaiveDate>,
    /// Absent leaves the cap alone; explicit `null` REMOVES it.
    ///
    /// Removing it has to be expressible, because `epoch_end` is not
    /// only a guard callers read — it is a hard cap inside `now()`.
    /// Sim-time clamps at it. A run that reaches its epoch_end and
    /// hands the clock to real time therefore FREEZES at midnight of
    /// that day unless the cap goes, and with absent-means-unchanged
    /// there was no way to say so. Seen exactly that way: a backfill
    /// went live at warp 1.0 and the clock sat at
    /// `2026-08-07T00:00:00` while the simulator failed its readiness
    /// gate on a loop.
    #[serde(default, deserialize_with = "double_option")]
    pub epoch_end: Option<Option<NaiveDate>>,
    /// Sim-seconds per wall-second. Default brewery playground
    /// value is `8640.0` (1 sim-day per 10 wall-seconds).
    /// Backtests use large values; live demo uses moderate.
    #[serde(default)]
    pub warp_factor: Option<f64>,
}

/// Body of `POST /api/clock/restart-epoch`. Resets the formula
/// so sim-time starts over from epoch_start at wall-now. No
/// payload required; the existing epoch_start + epoch_end +
/// warp_factor are preserved.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RestartEpochRequest {}

/// Mode the clock service is running in. Reported by
/// `GET /api/clock/health` so deploys can sanity-check.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClockMode {
    /// Wall-clock (production). `now` always returns
    /// `Utc::now()`.
    Wall,
    /// Sim mode. `now` returns the formula-computed sim instant.
    Sim,
}

/// Response of `GET /api/clock/baseline` — the provenance of the
/// seed baseline this tenant replays.
///
/// Deliberately NOT folded into [`ClockNow`]. That type is `Copy` and
/// is read on the hot path by every service stamping every event;
/// `source_ref` is a `String` (breaking `Copy` across 41 call sites)
/// carrying information those callers never look at, and which only
/// changes when someone re-cuts the baseline. Provenance is a
/// different question from "what time is it", so it gets its own
/// endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineProvenance {
    /// `MAX(audit_log.id)` at the moment the seed finished — the id
    /// `restart-epoch` trims back to. `None` before any seed has
    /// completed.
    pub baseline_audit_id: Option<i64>,
    /// Wall instant the baseline was cut. `None` on installs seeded
    /// before this was recorded.
    pub cut_at: Option<DateTime<Utc>>,
    /// Source revision the seed was cut from — a short git SHA.
    /// `None` when the seeding host had no git (docker images ship
    /// no `.git`), which reads as "unknown", never as "current".
    pub source_ref: Option<String>,
    /// Whole days between the cut and now. `None` when `cut_at` is
    /// unknown. This is the number worth looking at: the demo
    /// replays the baseline forever, so age is exactly how far the
    /// running tenant's model has fallen behind the source tree.
    pub age_days: Option<i64>,
}

impl BaselineProvenance {
    /// Build from stored fields, deriving `age_days` against `now`.
    ///
    /// A cut timestamp in the future (clock skew between the seeding
    /// host and the reader) clamps to 0 rather than reporting a
    /// negative age — "cut in the future" is not a meaningful
    /// staleness answer, and 0 is the honest floor.
    pub fn new(
        baseline_audit_id: Option<i64>,
        cut_at: Option<DateTime<Utc>>,
        source_ref: Option<String>,
        now: DateTime<Utc>,
    ) -> Self {
        let age_days = cut_at.map(|cut| (now - cut).num_days().max(0));
        Self {
            baseline_audit_id,
            cut_at,
            source_ref,
            age_days,
        }
    }
}

#[cfg(test)]
mod baseline_tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().expect("test timestamp parses")
    }

    #[test]
    fn age_is_whole_days_between_cut_and_now() {
        let p = BaselineProvenance::new(
            Some(4495),
            Some(t("2026-07-11T03:54:56Z")),
            Some("27e29fda".to_string()),
            t("2026-08-04T03:05:12Z"),
        );
        // The real playground gap that made a 3-week-old fix look
        // like an open modeling defect.
        assert_eq!(p.age_days, Some(23));
        assert_eq!(p.baseline_audit_id, Some(4495));
        assert_eq!(p.source_ref.as_deref(), Some("27e29fda"));
    }

    #[test]
    fn a_fresh_cut_is_zero_days_old() {
        let cut = t("2026-08-04T03:05:12Z");
        let p = BaselineProvenance::new(Some(1), Some(cut), None, cut);
        assert_eq!(p.age_days, Some(0));
    }

    #[test]
    fn unknown_cut_yields_unknown_age_not_zero() {
        // The distinction that matters: a seed with no recorded
        // provenance must not read as "cut just now".
        let p = BaselineProvenance::new(Some(1), None, None, t("2026-08-04T03:05:12Z"));
        assert_eq!(p.age_days, None);
        assert_eq!(p.cut_at, None);
        assert_eq!(p.source_ref, None);
    }

    #[test]
    fn a_future_cut_clamps_to_zero_rather_than_going_negative() {
        let p = BaselineProvenance::new(
            Some(1),
            Some(t("2026-08-05T00:00:00Z")),
            None,
            t("2026-08-04T00:00:00Z"),
        );
        assert_eq!(p.age_days, Some(0));
    }
}
