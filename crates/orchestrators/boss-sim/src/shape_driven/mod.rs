//! Shape-driven simulator — reads JobKind / StepType / SubjectKind /
//! Class registries + a tenant config and synthesizes plausible
//! activity without per-domain Rust generators. The *behavior* is
//! data, not code: a new tenant ships JobKinds + tenant.toml + Class
//! seeds and the sim runs against them with no Rust changes,
//! producing Jobs, Steps, Subjects, and events on the wire.
//!
//! The module's pieces:
//!
//! - `tenant`: parses `examples/brewery/seeds/tenant.toml` and any
//!   equivalent file for other tenants. The TOML shape is the
//!   contract — every tenant authors one of these.
//! - `sampler`: pure functions that compute "how many Jobs of this
//!   JobKind should be created today" from a `JobRate` + a
//!   `NaiveDate` (handles ramps + weekday/weekend multipliers +
//!   Poisson sampling).
//! - `engine`: the day loop that ties the registries + tenant +
//!   state into Job/Step output.
//! - `state`: the per-run in-memory state the engine threads across
//!   ticks.

pub mod engine;
pub mod sampler;
pub mod state;
pub mod tenant;

pub use engine::{open_job_from_request, simulate_day, simulate_tick_with_handlers};
pub use sampler::{count_jobs_for_day, count_jobs_for_tick, effective_rate_for_day};
pub use state::{DaySummary, RunCounters, ShapeDrivenState};
pub use tenant::{
    AnomalyRates, JobRate, RampPoint, SubjectRate, TenantConfig, TenantConfigError, TenantMeta,
};
