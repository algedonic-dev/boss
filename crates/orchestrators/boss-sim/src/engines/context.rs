//! `DayContext` — the per-day argument bundle every engine receives.
//! Holds mutable references to the four pieces of shared state
//! engines need: the current day, the RNG, the shape-driven sim
//! state, the output sink, and the cross-engine bus.
//!
//! Lifetime is tied to a single `simulate_day` call; engines must not
//! retain pieces of the context across calls.

use chrono::NaiveDate;

use crate::engines::{SimEventBus, Tick};
use crate::output::SimOutput;
use crate::rng::Rng;
use crate::shape_driven::ShapeDrivenState;

pub struct DayContext<'a> {
    pub day: NaiveDate,
    /// Current tick within the sim-day. Engines that need sub-day
    /// awareness (e.g. `PeriodicEngine` with `Cadence::Hourly` /
    /// `EveryNMinutes`) read this to decide whether the cadence
    /// anchor falls in this tick's window. Day-tick callers pass
    /// `Tick::day()`.
    pub tick: Tick,
    pub rng: &'a mut Rng,
    pub state: &'a mut ShapeDrivenState,
    pub output: &'a mut dyn SimOutput,
    pub bus: &'a mut SimEventBus,
}
