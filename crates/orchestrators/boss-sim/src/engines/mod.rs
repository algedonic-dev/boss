//! Pluggable sim engines and the shared event bus they communicate
//! over. Three archetypes — HumanWorker, Counterparty, Periodic —
//! driven in that day-loop order
//! (docs/architecture-decisions.md §Simulator). The shape-driven
//! HumanWorker engine lives in `crate::shape_driven`.
pub mod bus;
pub mod context;
pub mod counterparty;
pub mod day_runner;
pub mod periodic;
pub mod tick;
pub mod traits;

pub use bus::{SimBusEvent, SimEventBus};
pub use context::DayContext;
pub use counterparty::{
    CounterpartyEngine, CounterpartySpec, CounterpartyState, DelaySpec, QueuedEmission, ScanSpec,
};
pub use day_runner::{
    RunReport, end_of_day_rollup, run_days, run_one_tick_with_handlers, run_ticks_with_handlers,
};
pub use periodic::{Cadence, PeriodicAction, PeriodicEngine, PeriodicSpec};
pub use tick::{Tick, parse_hh_mm};
pub use traits::SimEngine;
