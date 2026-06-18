//! Axum HTTP handlers for the jobs API.
//!
//! Handlers are grouped by concern into submodules; this module owns
//! the shared `JobsApiState`, the router that wires every route, and
//! the helpers used across more than one concern.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use boss_core::job::{Job, JobStatus, Step, StepStatus};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_policy_client::{Action, Decision, Resource};

use boss_policy_client::{CurrentUser, PolicyClient};
use serde::{Deserialize, Serialize};

use crate::events;
use crate::in_memory::compute_job_status;
use crate::policy_glue::scope_matches;
use crate::port::{JobFilter, JobScope, JobsRepository, LaunchCalendarRow};
use crate::registry::{JobKindError, JobKindRegistry, JobKindSpec};
use crate::step_plugins::{StepPluginError, StepPluginRegistry, StepPluginSpec};
use crate::step_registry::StepRegistry;

mod jobs;
mod kinds;
mod plugins;
mod sim_clock;
mod steps;

use jobs::*;
use kinds::*;
use plugins::*;
use sim_clock::*;
use steps::*;

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 1000;
/// Cap for the sim workforce's bulk assigned-backlog pull — higher than
/// the My-Day `MAX_LIMIT` because the workforce legitimately wants the
/// whole assigned backlog in one round-trip, not a page.
const BULK_ASSIGNED_LIMIT: i64 = 50_000;

/// Shared state for jobs API handlers.
pub struct JobsApiState<R: JobsRepository, B: EventBus> {
    pub jobs: Arc<R>,
    pub bus: Arc<B>,
    pub publisher: DomainPublisher,
    pub step_registry: Arc<StepRegistry>,
    /// Cross-service client for row-level authorization. Plumb in a
    /// `ReqwestPolicyClient` in prod, `FakePolicyClient` in tests.
    pub policy: Arc<dyn PolicyClient>,
    /// JobKind registry — authored via /api/jobs/kinds. None until a
    /// caller wires the adapter in; endpoints respond with 503 in that
    /// case to keep the seam explicit.
    pub kind_registry: Option<Arc<dyn JobKindRegistry>>,
    /// Step UX plugin registry — authored via /api/jobs/step-plugins.
    /// Same optionality semantics as `kind_registry`.
    pub plugin_registry: Option<Arc<dyn StepPluginRegistry>>,
    /// Cross-service client for the global calendar primitive
    /// (`docs/architecture-decisions.md` §Calendar). When set, scheduling
    /// steps that transition `ready → active` with full
    /// metadata (`scheduled_at`, `duration_hours`, `assignee_id`)
    /// reserve the assignee's time; conflicts surface as 409.
    /// `None` keeps every existing test path working — the
    /// reservation hook is purely additive.
    pub calendar: Option<Arc<dyn boss_calendar_client::CalendarClient>>,
    /// SubjectKind registry client. When set, Job creates / updates
    /// with `subject = Subject::Custom { custom_kind }` validate
    /// `custom_kind` against the registry on writes. `None` skips the
    /// check — same opt-in shape as the calendar client; lets
    /// boss-jobs-api deploy independently of the subject-kinds rollout.
    pub subject_kinds: Option<Arc<dyn boss_subject_kinds_client::SubjectKindsClient>>,
    /// Subject-existence validator. When set, Job creates check the
    /// Subject's id against the relevant upstream service (people,
    /// assets, locations, inventory). `None` skips the check — same
    /// opt-in shape as the registry clients above; preserves the
    /// in-memory test path.
    pub subject_existence: Option<Arc<dyn crate::subject_existence::SubjectExistenceCheck>>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

/// Build the jobs API router.
pub fn router<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: JobsApiState<R, B>,
) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/jobs/health", get(health))
        .route("/api/jobs/summary", get(jobs_summary::<R, B>))
        .route("/api/jobs/live", get(jobs_live::<R, B>))
        .route("/api/jobs/sim-clock/pause", post(sim_clock_pause::<R, B>))
        .route("/api/jobs/sim-clock/resume", post(sim_clock_resume::<R, B>))
        .route(
            "/api/jobs/sim-clock/restart-epoch",
            post(sim_clock_restart_epoch::<R, B>),
        )
        .route("/api/jobs/sim-clock/stream", get(sim_clock_stream::<R, B>))
        .route(
            "/api/jobs/phase-distribution",
            get(jobs_phase_distribution::<R, B>),
        )
        .route("/api/jobs/launch-calendar", get(launch_calendar::<R, B>))
        .route("/api/jobs/assignments", get(list_assignments::<R, B>))
        .route("/api/jobs", get(list_jobs::<R, B>))
        .route("/api/jobs", post(create_job::<R, B>))
        .route("/api/jobs/{id}", get(get_job::<R, B>))
        .route("/api/jobs/{id}", put(update_job::<R, B>))
        .route("/api/jobs/{id}/stream", get(job_stream::<R, B>))
        .route("/api/jobs/step-types", get(list_step_types::<R, B>))
        .route("/api/jobs/{id}/steps", get(list_steps::<R, B>))
        .route("/api/jobs/{id}/steps", post(add_step::<R, B>))
        .route("/api/jobs/{id}/steps/{step_id}", put(update_step::<R, B>))
        .route(
            "/api/jobs/{id}/steps/{step_id}/sign-offs",
            post(post_step_sign_off::<R, B>),
        )
        // JobKind registry — see docs/architecture-decisions.md
        // §Jobs, JobKinds, Steps
        .route(
            "/api/jobs/kinds",
            get(list_kinds::<R, B>).post(create_kind::<R, B>),
        )
        .route(
            "/api/jobs/kinds/{kind}",
            get(get_kind::<R, B>).put(update_kind::<R, B>),
        )
        .route(
            "/api/jobs/kinds/{kind}/versions",
            get(list_kind_versions::<R, B>),
        )
        .route(
            "/api/jobs/kinds/{kind}/versions/{version}",
            get(get_kind_version::<R, B>),
        )
        .route("/api/jobs/kinds/{kind}/publish", post(publish_kind::<R, B>))
        .route("/api/jobs/kinds/{kind}/retire", post(retire_kind::<R, B>))
        // Step UX plugin registry — see docs/architecture-decisions.md
        // §Step UX & frontend
        .route(
            "/api/jobs/step-plugins",
            get(list_plugins::<R, B>).post(create_plugin::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}",
            get(get_plugin::<R, B>).put(update_plugin::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}/versions",
            get(list_plugin_versions::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}/versions/{version}",
            get(get_plugin_version::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}/publish",
            post(publish_plugin::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}/retire",
            post(retire_plugin::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}/in-flight-count",
            get(in_flight_plugin_count::<R, B>),
        )
        .with_state(shared)
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-jobs-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

// ---------------------------------------------------------------------------
// Subject validation (shared by create_job / update_job and the kinds path)
// ---------------------------------------------------------------------------

/// SubjectKind validator. The SubjectKind registry is the **single
/// source of truth** for the noun vocabulary — core enumerates no
/// kinds itself. When the registry is wired, every kind is validated
/// against it (the platform specializations — `asset`, `account`,
/// `purchase_order`, … — are seeded rows, so they pass; an unknown
/// kind 400s). When it isn't wired (tests) the check is a no-op and
/// all kinds pass.
///
/// 502 on registry-call failure rather than dropping the write
/// silently — a misconfigured registry is loud, not a silent
/// always-pass.
#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
pub(super) async fn check_custom_subject(
    registry: Option<&Arc<dyn boss_subject_kinds_client::SubjectKindsClient>>,
    subject: &boss_core::job::Subject,
) -> Result<(), Response> {
    let Some(reg) = registry else {
        return Ok(());
    };
    let kind = subject.kind.as_str();
    match reg.subject_kind_exists(kind).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown subject kind `{kind}` — register it in the subject_kinds registry first",
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            format!("subject-kinds registry unreachable: {e}"),
        )
            .into_response()),
    }
}

/// Adapter so handlers stay terse. Pulls the registry off
/// `JobsApiState` and delegates to `check_custom_subject`.
#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
pub(super) async fn validate_custom_subject<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    subject: &boss_core::job::Subject,
) -> Result<(), Response> {
    check_custom_subject(state.subject_kinds.as_ref(), subject).await
}

// ---------------------------------------------------------------------------
// Shared id parsers
// ---------------------------------------------------------------------------

pub(super) fn parse_job_id(s: &str) -> Option<boss_core::job::JobId> {
    let uuid = uuid::Uuid::parse_str(s).ok()?;
    Some(boss_core::job::JobId::from_uuid(uuid))
}

pub(super) fn parse_step_id(s: &str) -> Option<boss_core::job::StepId> {
    let uuid = uuid::Uuid::parse_str(s).ok()?;
    Some(boss_core::job::StepId::from_uuid(uuid))
}
