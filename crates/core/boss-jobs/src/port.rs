//! Hexagonal port: `JobsRepository` defines what the domain needs from
//! persistence. Adapters (in-memory for tests, Postgres for prod)
//! implement this trait.

use async_trait::async_trait;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus};
use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum JobsError {
    #[error("job not found: {0}")]
    NotFound(JobId),
    #[error("step not found: {0}")]
    StepNotFound(StepId),
    #[error("storage failure: {0}")]
    Storage(String),
}

/// Optional filters for listing jobs.
#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub kind: Option<String>,
    /// Prefix match on `kind`. Used by the UI for nav buckets that
    /// span related registry kinds (e.g. `refurb-used` + `refurb-oem-new`
    /// both match `kind_prefix = "refurb"`).
    pub kind_prefix: Option<String>,
    pub status: Option<JobStatus>,
    pub priority: Option<Priority>,
    pub owner_id: Option<String>,
    /// Filter by subject reference (e.g., device serial, account id).
    pub subject_id: Option<String>,
    /// Row-level policy scope — translated from `boss_policy_client::Predicate`
    /// by the HTTP handler before calling the adapter. Pushing it down
    /// into SQL here means scoped roles get accurate `total` counts
    /// and pages that only contain jobs they can see (no wasted page
    /// space on rows the post-fetch filter would discard).
    pub scope: JobScope,
}

/// The policy-scope slice applied to a listing. Mirrors the shapes
/// of `boss_policy_client::Predicate` that translate cleanly to SQL;
/// `DepartmentIs` is absent because Jobs don't carry a department
/// column (the HTTP handler still handles that case as an all-or-nothing
/// pre-check since the answer only depends on the caller's department).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum JobScope {
    /// No additional constraint. Adapter applies only the usual filter
    /// fields.
    #[default]
    All,
    /// Short-circuit to an empty result set (policy says the caller
    /// can see nothing).
    None,
    /// Only jobs where `owner_id = user_id`.
    OwnerIs(String),
    /// Only jobs where `owner_id IN (ids)` (user + direct reports).
    OwnerIn(Vec<String>),
    /// Only jobs whose subject references a account_id in the
    /// provided list. Matches both `Subject::Account { id }` and
    /// `Subject::Employee { id }` — the policy convention treats
    /// an employee's account_id bucket the same as a account row.
    AccountIn(Vec<String>),
}

/// One row in the launch-calendar projection. Flat shape the frontend
/// renders directly — the caller doesn't need to fetch the full Job.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LaunchCalendarRow {
    pub job_id: JobId,
    pub title: String,
    pub owner_id: Option<String>,
    pub subject_id: Option<String>,
    pub status: JobStatus,
    /// Min sort_order of any non-done step = current tier. Null means
    /// every step is terminal but the Job isn't closed yet.
    pub current_tier: Option<i32>,
    /// `launch_date` from the tier-4 `marketing-launch` step's metadata.
    /// Null when the step exists but the date hasn't been set yet.
    pub launch_date: Option<chrono::NaiveDate>,
    /// Channel label from the launch step ("email" / "webinar" / etc.).
    pub launch_channel: Option<String>,
}

/// One open, workable step surfaced to an executor's "My Day" pull
/// query — the step plus the minimum Job context the caller needs to
/// act on it without a second fetch. Returned by
/// [`JobsRepository::list_assignments`]; consumed by the SPA My Day
/// surface and the sim's workforce loop.
#[derive(Debug, Clone, Serialize)]
pub struct AssignmentRow {
    pub job_id: JobId,
    pub job_kind: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub priority: Priority,
    pub step: Step,
}

/// Persistence port for jobs and steps.
///
/// **Timestamp threading.** The four mutation methods come in two
/// flavors: a convenience overload (`create_job(&job)`) that stamps
/// `Utc::now()` server-side, and an `_at` variant
/// (`create_job_at(&job, now)`) that takes an explicit timestamp.
/// Handlers that emit a domain event for the same mutation should
/// use the `_at` form so the projection write and the audit_log
/// event share one timestamp — required for the audit_log →
/// projection rebuild path to reproduce `created_at` / `updated_at`
/// exactly. See `docs/design/projection-rebuilders.md`.
#[async_trait]
pub trait JobsRepository: Send + Sync {
    // ----- Jobs -----

    async fn create_job(&self, job: &Job) -> Result<(), JobsError> {
        self.create_job_at(job, Utc::now()).await
    }

    async fn create_job_at(&self, job: &Job, now: DateTime<Utc>) -> Result<(), JobsError>;

    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, JobsError>;

    async fn update_job(&self, job: &Job) -> Result<(), JobsError> {
        self.update_job_at(job, Utc::now()).await
    }

    async fn update_job_at(&self, job: &Job, now: DateTime<Utc>) -> Result<(), JobsError>;

    async fn list_jobs(
        &self,
        filter: &JobFilter,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Job>, i64), JobsError>;

    // ----- Steps -----

    async fn add_step(&self, step: &Step) -> Result<(), JobsError> {
        self.add_step_at(step, Utc::now()).await
    }

    async fn add_step_at(&self, step: &Step, now: DateTime<Utc>) -> Result<(), JobsError>;

    async fn get_step(&self, id: &StepId) -> Result<Option<Step>, JobsError>;

    async fn update_step(&self, step: &Step) -> Result<(), JobsError> {
        self.update_step_at(step, Utc::now()).await
    }

    async fn update_step_at(&self, step: &Step, now: DateTime<Utc>) -> Result<(), JobsError>;

    /// Append one sign-off stamp atomically. Stamps are
    /// append-only and owned by this path — the generic step UPDATE
    /// never writes `sign_offs`, so a concurrent read-modify-write
    /// (dispatcher auto-assign, predicate re-eval) cannot clobber a
    /// stamp that landed between its read and its write.
    async fn append_sign_off(
        &self,
        step_id: &StepId,
        stamp: &boss_core::job::SignOffStamp,
        now: DateTime<Utc>,
    ) -> Result<(), JobsError>;

    async fn list_steps(&self, job_id: &JobId) -> Result<Vec<Step>, JobsError>;

    /// Open, workable steps for an executor — the pull side of the
    /// "human-powered state machine" dispatcher. Returns steps whose
    /// status is `Ready | Active` AND that are either assigned to
    /// `assignee_id` OR unassigned with a `metadata.authority_role`
    /// in `roles` (claimable by role). Only steps on `Open` Jobs.
    /// `limit` caps the result.
    ///
    /// The default impl scans open Jobs + their steps in Rust — correct
    /// but O(open jobs); the Postgres adapter overrides it with a single
    /// indexed JOIN. Drives the SPA My Day surface and the sim's
    /// workforce loop (which queries as each simulated employee).
    async fn list_assignments(
        &self,
        assignee_id: Option<&str>,
        roles: &[String],
        limit: i64,
    ) -> Result<Vec<AssignmentRow>, JobsError> {
        // Unoptimized fallback: scan open Jobs + their steps and filter
        // in Rust. The Postgres adapter overrides this with one indexed
        // JOIN. Open Jobs only; ordered by (opened_on, sort_order).
        let filter = JobFilter {
            status: Some(JobStatus::Open),
            ..Default::default()
        };
        let (mut jobs, _) = self.list_jobs(&filter, 10_000, 0).await?;
        jobs.sort_by_key(|j| j.opened_on);
        let mut out = Vec::new();
        for job in &jobs {
            for step in self.list_steps(&job.id).await? {
                if !matches!(step.status, StepStatus::Ready | StepStatus::Active) {
                    continue;
                }
                let assignee_match = match (step.assignee_id.as_deref(), assignee_id) {
                    (Some(a), Some(me)) => a == me,
                    _ => false,
                };
                // Role-match applies when the step is either unassigned
                // (claimable) OR already Active (in-progress, owned by the
                // role's workforce — so a worker can re-find and finish a
                // multi-day step it claimed earlier). A Ready step already
                // assigned to someone else is theirs, not poachable.
                let role_eligible =
                    step.assignee_id.is_none() || matches!(step.status, StepStatus::Active);
                let role_match = role_eligible
                    && step
                        .metadata
                        .get("authority_role")
                        .and_then(|v| v.as_str())
                        .is_some_and(|r| roles.iter().any(|x| x == r));
                if assignee_match || role_match {
                    out.push(AssignmentRow {
                        job_id: job.id,
                        job_kind: job.kind.clone(),
                        subject_kind: boss_core::primitives::Subject::kind(&job.subject)
                            .to_string(),
                        subject_id: boss_core::primitives::Subject::id(&job.subject).to_string(),
                        priority: job.priority,
                        step,
                    });
                    if out.len() >= limit as usize {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }

    /// The entire assigned-and-workable backlog in ONE query: every step
    /// of an open Job that is Ready or Active AND already carries an
    /// assignee. The sim workforce pulls this each pass and drives every
    /// assigned step regardless of who assigned it or to whom — which
    /// decouples the executor from assignment policy (the dispatcher, and
    /// later managers, own that) and replaces a per-employee query
    /// fan-out with a single round-trip.
    ///
    /// The default impl scans open Jobs in Rust; the Postgres adapter
    /// overrides it with one indexed JOIN. Ordered by (opened_on,
    /// sort_order) for a stable queue.
    async fn list_assigned_workable(&self, limit: i64) -> Result<Vec<AssignmentRow>, JobsError> {
        let filter = JobFilter {
            status: Some(JobStatus::Open),
            ..Default::default()
        };
        let (mut jobs, _) = self.list_jobs(&filter, 10_000, 0).await?;
        jobs.sort_by_key(|j| j.opened_on);
        let mut out = Vec::new();
        for job in &jobs {
            for step in self.list_steps(&job.id).await? {
                if !matches!(step.status, StepStatus::Ready | StepStatus::Active) {
                    continue;
                }
                if step
                    .assignee_id
                    .as_deref()
                    .filter(|a| !a.is_empty())
                    .is_none()
                {
                    continue;
                }
                out.push(AssignmentRow {
                    job_id: job.id,
                    job_kind: job.kind.clone(),
                    subject_kind: boss_core::primitives::Subject::kind(&job.subject).to_string(),
                    subject_id: boss_core::primitives::Subject::id(&job.subject).to_string(),
                    priority: job.priority,
                    step,
                });
                if out.len() >= limit as usize {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    /// Count steps whose kind matches `step_kind` and whose status is
    /// still non-terminal (pending, ready, active). Used by the Step
    /// UX plugin retire path to surface a blast-radius preview.
    async fn count_in_flight_steps_by_kind(&self, step_kind: &str) -> Result<i64, JobsError>;

    /// Group Jobs by kind and return `(kind, count)` pairs, optionally
    /// scoped to a specific status. Used by the operating-model view
    /// to drive the per-phase live counts without pulling the whole
    /// job list over the wire. Returns every kind present in the
    /// table (no zero-fills) — callers map the list into their own
    /// `{kind: count}` shape.
    async fn count_jobs_by_kind(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i64)>, JobsError>;

    /// For each open Job, compute its "current tier" — the min
    /// sort_order of any non-terminal step on the Job. Group by
    /// `(kind, current_tier)` and return the counts. The tier number
    /// is the step index the Job is currently working on; -1 means
    /// every step is terminal (completed/skipped) but the Job itself
    /// hasn't been closed yet.
    ///
    /// Drives the live histogram on the operating-model view so a
    /// JobKind bar can show "how many refurbs are in Acquire vs.
    /// Refurbish vs. Certify right now." Caller maps tier → phase
    /// via its own JobKind-specific mapping.
    async fn jobs_tier_distribution(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i32, i64)>, JobsError>;

    /// Projection backing the launch-calendar surface and the exec
    /// next-30-days panel per examples/used-device-shop/design/marketing-needs.md E2. Returns every
    /// open/in-flight `marketing-motion` Job joined to its tier-4
    /// `marketing-launch` step so the caller can render a forward
    /// calendar. `from` / `to` bound the launch_date window; Jobs
    /// whose launch step has no date yet are returned with `launch_date
    /// = None` so the UI can bucket them under "unscheduled".
    async fn list_launch_calendar(
        &self,
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
    ) -> Result<Vec<LaunchCalendarRow>, JobsError>;

    // ----- Cross-job dependency resolution (D10) -----

    /// Given a set of step IDs (possibly spanning multiple jobs),
    /// return their current statuses. Used to check whether a blocked
    /// step's dependencies have been satisfied.
    async fn resolve_blockers(
        &self,
        ids: &[StepId],
    ) -> Result<Vec<(StepId, StepStatus)>, JobsError>;

    /// Read the brewery sim_clock state (if the repo's underlying
    /// store has the `sim_clock` table). Returns None for in-
    /// memory adapters or fresh DBs that haven't seeded a
    /// sim_clock row yet. Used by the public landing's
    /// "simulated time" indicator + /workflows live counter so
    /// operators see the sim epoch advancing in real time.
    async fn sim_clock_state(&self) -> Result<Option<SimClockState>, JobsError> {
        Ok(None)
    }

    /// Set the sim_clock's `paused` flag. The brewery-sim daemon
    /// reads this on every tick and stops advancing
    /// `current_sim_date` when paused. Used by the Debug menu's
    /// pause/resume actions; in-memory adapters no-op.
    async fn set_sim_clock_paused(&self, _paused: bool) -> Result<(), JobsError> {
        Ok(())
    }

    /// Restart the sim epoch with a **clean** baseline — drops the
    /// prior loop's audit_log + projection state, replays the
    /// canonical seed bundle, resets the sim_clock to
    /// `epoch_start_date`, unpauses. The daemon's next tick
    /// resumes from `epoch_start_date` against fresh data.
    ///
    /// This is the demo-loop reset: the brewery playground is a
    /// "flea circus" the audience watches, so accumulating state
    /// across loops would poison the visual. The truncate+replay
    /// path is faster than the operator shell script
    /// (`infra/postgres/reset-to-baseline.sh`) since it skips
    /// the DB drop + bootstrap + data-seed steps — typical
    /// runtime is 30-60 seconds vs. 5 minutes.
    ///
    /// In-memory adapters no-op.
    async fn restart_sim_clock_epoch(&self) -> Result<(), JobsError> {
        Ok(())
    }
}

/// Snapshot of the simulated clock for read-side surfaces. All
/// values are derived from clock-api's formula in real time.
#[derive(Debug, Clone, Serialize)]
pub struct SimClockState {
    /// Full sim-time instant. The SPA renders date + HH:MM from
    /// this so the within-day movement of the formula clock is
    /// visible.
    pub now: chrono::DateTime<chrono::Utc>,
    /// Convenience date-only projection for surfaces that only
    /// need the day (appToday()-style consumers).
    pub current_sim_date: chrono::NaiveDate,
    pub epoch_start_date: Option<chrono::NaiveDate>,
    pub epoch_end_date: Option<chrono::NaiveDate>,
    pub paused: bool,
    /// True while the clean-reset path is mid-flight (audit_log
    /// truncate + boss-rebuild-all replay + clock rewind). The
    /// SimClockBadge polls this to render a spinner instead of
    /// the "Restart epoch" button.
    #[serde(default)]
    pub restart_in_progress: bool,
}
