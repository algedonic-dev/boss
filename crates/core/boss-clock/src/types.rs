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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
        }
    }
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
    #[serde(default)]
    pub epoch_end: Option<NaiveDate>,
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
