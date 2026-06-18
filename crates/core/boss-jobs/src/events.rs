//! NATS event subjects for the jobs domain.
//!
//! Two layers:
//!
//! - **State events** carry full row state and are what the
//!   audit_log → projection rebuild path consumes. Every Job or
//!   Step mutation emits exactly one: `JOB_CREATED`, `JOB_UPDATED`,
//!   `STEP_CREATED`, `STEP_UPDATED`. The payload is a serialized
//!   `Job` / `Step` — the rebuilder reproduces the projection by
//!   replaying these in audit_log id order.
//! - **Marker events** are informational signals for downstream
//!   consumers (dispatcher rule registry, UI badges, integrations).
//!   They duplicate state already in the sibling state event but
//!   give consumers a topic to filter on without payload matching.
//!   Rebuilder ignores them.

// State events — projections rebuild from these.
pub const JOB_CREATED: &str = "jobs.job.created";
pub const JOB_UPDATED: &str = "jobs.job.updated";
pub const STEP_CREATED: &str = "jobs.step.created";
pub const STEP_UPDATED: &str = "jobs.step.updated";
/// Emitted when a `job-kind-publish` Step flips to `Done` and the
/// dispatch path successfully writes a JobKindSpec into the
/// registry. Payload is the full published `JobKindSpec` (with
/// `authoring_job_id` set to the meta-Job's id), matching what
/// `rebuild_job_kinds` consumes to reconstruct the projection.
pub const JOB_KIND_PUBLISHED: &str = "jobs.kind.published";

// Marker events — informational only; rebuild ignores them.
pub const JOB_STATUS_CHANGED: &str = "jobs.job.status_changed";
pub const STEP_COMPLETED: &str = "jobs.step.completed";
pub const STEP_SIGNED_OFF: &str = "jobs.step.signed_off";
/// Loud stamp invalidation (architecture-decisions.md §Step types
/// are property bundles): a stamped step's completion-relevant shape changed;
/// the listed stamps no longer attest the current content and the
/// named roles must re-sign before the step can complete.
pub const STEP_STAMPS_INVALIDATED: &str = "jobs.step.stamps_invalidated";
pub const JOB_CLOSED: &str = "jobs.job.closed";
