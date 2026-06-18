//! Boss jobs domain — universal coordination primitive.
//!
//! A Job is a bounded unit of coordinated work that crosses team
//! boundaries: device refurb, field service, procurement, sales,
//! marketing campaigns, employee onboarding. Each Job decomposes
//! into Steps — typed units of work owned by different people, with
//! optional sign-off gates and cross-job dependency tracking.
//!
//! Hexagonal: the domain defines a `JobsRepository` port (trait).
//! Postgres, in-memory, and other adapters implement the same trait.

pub mod calendar_hook;
pub mod escalation;
pub mod events;
pub mod http;
pub mod in_memory;
pub mod job_kind_lint;
pub mod jobs_config;
pub mod policy_glue;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod registry;
pub mod scheduling;
// Platform JobKinds live in `registry::platform_kinds()` (currently
// just `job-kind-design`); tenant JobKinds live in
// `examples/<tenant>/seeds/job_kinds.toml` and load via `seed_loader`.
// See docs/design/platform-vs-tenant-jobkinds.md.
pub mod seed_loader;
pub mod step_plugins;
pub mod step_registry;
pub mod subject_existence;

pub use in_memory::InMemoryJobs;
pub use port::{JobFilter, JobsError, JobsRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgJobs;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_jobs_and_steps};
#[cfg(feature = "postgres")]
pub use registry::PgJobKinds;
pub use registry::{
    InMemoryJobKinds, JobKindError, JobKindRegistry, JobKindSpec, JobKindStatus, StepSpec,
    Terminal, materialize_steps, reevaluate,
};
#[cfg(feature = "postgres")]
pub use step_plugins::PgStepPlugins;
pub use step_plugins::{InMemoryStepPlugins, StepPluginError, StepPluginRegistry, StepPluginSpec};
