//! Job CRUD, the dashboard/landing read surfaces, and step-type
//! discovery.

use super::*;

use axum::extract::{Path, Query};

// ---------------------------------------------------------------------------
// Step type discovery
// ---------------------------------------------------------------------------

pub(super) async fn list_step_types<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
) -> Response {
    let types: Vec<_> = state.step_registry.all().into_iter().cloned().collect();
    Json(types).into_response()
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct ListJobsQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    kind: Option<String>,
    /// Prefix match on kind (e.g. `kind_prefix=refurb` returns both
    /// `refurb-used` and `refurb-oem-new` jobs).
    kind_prefix: Option<String>,
    status: Option<JobStatus>,
    owner_id: Option<String>,
    /// Filter by the Job's subject id, regardless of subject kind. One
    /// generic spelling for every kind — there are no per-kind aliases
    /// to pick the wrong one of.
    subject_id: Option<String>,
}

pub(super) async fn list_jobs<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ListJobsQuery>,
) -> Response {
    // Policy: compute the caller's read-scope Predicate. Denied →
    // return an empty collection so the UI shows a clean empty state
    // instead of a 403 noise. (If you need to know *why*, call /check
    // explicitly.)
    let predicate = match state.policy.scope_predicate(&user, Resource::job()).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };

    // Translate the Predicate into a JobScope the adapter can push
    // into SQL. `DepartmentIs` is the odd one out: Jobs don't carry
    // a department column, so it's either "all" (caller's department
    // matches) or "none" (it doesn't). The handler short-circuits
    // before hitting the adapter in the mismatch case.
    let scope = match &predicate {
        boss_policy_client::Predicate::Unrestricted => JobScope::All,
        boss_policy_client::Predicate::None => JobScope::None,
        boss_policy_client::Predicate::OwnerIs { user_id } => JobScope::OwnerIs(user_id.clone()),
        boss_policy_client::Predicate::OwnerIn { user_ids } => JobScope::OwnerIn(user_ids.clone()),
        boss_policy_client::Predicate::AccountIn { account_ids } => {
            JobScope::AccountIn(account_ids.clone())
        }
        boss_policy_client::Predicate::DepartmentIs { department } => {
            if user.department.as_deref() == Some(department.as_str()) {
                JobScope::All
            } else {
                JobScope::None
            }
        }
    };

    let filter = JobFilter {
        kind: q.kind,
        kind_prefix: q.kind_prefix,
        status: q.status,
        owner_id: q.owner_id,
        subject_id: q.subject_id,
        scope,
        ..Default::default()
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = q.offset.unwrap_or(0).max(0);

    // Scope is applied in the adapter — the returned `total` is the
    // DB-wide count of jobs matching the filter AND the caller's
    // policy scope, and every row in `jobs` is already visible to
    // them. No post-filter pass needed.
    let (jobs, total) = match state.jobs.list_jobs(&filter, limit, offset).await {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    // Enrich each job with its steps so the list view can show progress.
    let mut enriched: Vec<serde_json::Value> = Vec::with_capacity(jobs.len());
    for job in &jobs {
        let steps = state.jobs.list_steps(&job.id).await.unwrap_or_default();
        let mut j = serde_json::to_value(job).unwrap_or_default();
        j["steps"] = serde_json::to_value(&steps).unwrap_or_default();
        enriched.push(j);
    }
    Json(serde_json::json!({
        "data": enriched,
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct AssignmentsQuery {
    /// Steps assigned to this employee are returned.
    assignee_id: Option<String>,
    /// Comma-separated role slugs; unassigned steps whose
    /// `authority_role` is one of these are returned (claimable by
    /// role). e.g. `roles=bookkeeper,head-brewer`.
    roles: Option<String>,
    /// Bulk mode: when true, ignore `assignee_id`/`roles` and return the
    /// entire assigned-and-workable backlog (every open-Job Ready/Active
    /// step that has an assignee) in one query. The sim workforce's pull.
    #[serde(default)]
    all_assigned: bool,
    limit: Option<i64>,
}

/// Pull surface for the "human-powered state machine" dispatcher:
/// the open, workable steps (Ready | Active) an executor can act on
/// right now — assigned to them, or unassigned and matching a role
/// they hold. Consumed by the SPA My Day surface and the sim's
/// workforce loop (which queries as each simulated employee). Returns
/// `{ data: [AssignmentRow], total }`.
pub(super) async fn list_assignments<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Query(q): Query<AssignmentsQuery>,
) -> Response {
    // Bulk path: the whole assigned backlog in one query (sim workforce).
    if q.all_assigned {
        let limit = q
            .limit
            .unwrap_or(BULK_ASSIGNED_LIMIT)
            .clamp(1, BULK_ASSIGNED_LIMIT);
        return match state.jobs.list_assigned_workable(limit).await {
            Ok(rows) => {
                let total = rows.len();
                Json(serde_json::json!({ "data": rows, "total": total })).into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
    }
    let roles: Vec<String> = q
        .roles
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    // No selector at all → empty (avoid returning the whole table).
    if q.assignee_id.is_none() && roles.is_empty() {
        return Json(serde_json::json!({ "data": [], "total": 0 })).into_response();
    }
    match state
        .jobs
        .list_assignments(q.assignee_id.as_deref(), &roles, limit)
        .await
    {
        Ok(rows) => {
            let total = rows.len();
            Json(serde_json::json!({ "data": rows, "total": total })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct JobsSummaryQuery {
    status: Option<JobStatus>,
}

/// Lightweight counts-by-kind for the operating-model view.
///
/// Returns `{ "counts": {kind: n}, "total": N, "status": "open"? }`.
/// Unlike /api/jobs, this doesn't paginate — the result is O(kinds)
/// not O(jobs), and the caller uses it to light up per-phase
/// counts on a company-map view without pulling 5k+ rows of
/// Job JSON over the wire.
pub(super) async fn jobs_summary<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Query(q): Query<JobsSummaryQuery>,
) -> Response {
    match state.jobs.count_jobs_by_kind(q.status).await {
        Ok(pairs) => {
            let total: i64 = pairs.iter().map(|(_, n)| *n).sum();
            let counts: serde_json::Map<String, serde_json::Value> = pairs
                .into_iter()
                .map(|(k, n)| (k, serde_json::Value::from(n)))
                .collect();
            Json(serde_json::json!({
                "counts": counts,
                "total": total,
                "status": q.status.map(job_status_str_public),
            }))
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Public landing-page surface. Returns a small bundle of
/// "what's the brewery doing right now": per-JobKind counts of
/// open Jobs + a list of the most recently-opened in-flight
/// Jobs with their current step. No auth — the gateway proxies
/// this unauth so the public landing can render a live window
/// into the operating company.
pub(super) async fn jobs_live<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
) -> Response {
    use crate::port::{JobFilter, JobScope};
    // Counts by kind (open only — closed Jobs are history; the
    // landing wants in-flight).
    let by_kind = match state
        .jobs
        .count_jobs_by_kind(Some(boss_core::job::JobStatus::Open))
        .await
    {
        Ok(pairs) => pairs,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let total: i64 = by_kind.iter().map(|(_, n)| *n).sum();
    let counts: serde_json::Map<String, serde_json::Value> = by_kind
        .into_iter()
        .map(|(k, n)| (k, serde_json::Value::from(n)))
        .collect();

    // Latest 12 open Jobs as a "what's running right now" feed.
    let filter = JobFilter {
        kind: None,
        kind_prefix: None,
        status: Some(boss_core::job::JobStatus::Open),
        priority: None,
        owner_id: None,
        subject_id: None,
        scope: JobScope::All,
    };
    let jobs = match state.jobs.list_jobs(&filter, 12, 0).await {
        Ok((jobs, _total)) => jobs,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // Strip the heavy bits — the public surface only needs the
    // shape that lights up the landing's "in-flight" panel.
    let recent: Vec<serde_json::Value> = jobs
        .into_iter()
        .map(|j| {
            serde_json::json!({
                "id": j.id.to_string(),
                "kind": j.kind,
                "title": j.title,
                "status": j.status,
                "priority": j.priority,
                "subject_kind": boss_core::primitives::Subject::kind(&j.subject),
                "subject_id": boss_core::primitives::Subject::id(&j.subject),
                "opened_on": j.opened_on,
            })
        })
        .collect();

    Json(serde_json::json!({
        "counts": counts,
        "open_total": total,
        "recent": recent,
        // Sim_clock snapshot drives the public landing's
        // "simulated time: 2026-07-09" indicator + the /workflows
        // header. Derived from clock-api directly, so the date the
        // SPA shows always matches the timestamp events stamp.
        "sim_clock": sim_clock_state_from_clock(state.clock.as_ref()).await,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct LaunchCalendarQuery {
    /// ISO date (YYYY-MM-DD); defaults to today (UTC).
    from: Option<chrono::NaiveDate>,
    /// ISO date (YYYY-MM-DD); defaults to `from + 90 days`.
    to: Option<chrono::NaiveDate>,
}

/// Launch-calendar projection per examples/used-device-shop/design/marketing-needs.md E2. Returns every
/// open/in-flight `marketing-motion` Job with its tier-4
/// `marketing-launch` step's date + channel, plus the Job's current
/// tier. Frontend renders at `/calendar` (standalone) and in the exec
/// dashboard next-30-days panel.
pub(super) async fn launch_calendar<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Query(q): Query<LaunchCalendarQuery>,
) -> Response {
    let from = q
        .from
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());
    let to = q.to.unwrap_or_else(|| from + chrono::Duration::days(90));
    match state.jobs.list_launch_calendar(from, to).await {
        Ok(rows) => {
            #[derive(Serialize)]
            struct Out {
                data: Vec<LaunchCalendarRow>,
                from: chrono::NaiveDate,
                to: chrono::NaiveDate,
            }
            Json(Out {
                data: rows,
                from,
                to,
            })
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Return the public kebab-case string for a JobStatus.
/// Mirrors the DB storage format used by the adapters.
pub(super) fn job_status_str_public(s: JobStatus) -> &'static str {
    match s {
        JobStatus::Draft => "draft",
        JobStatus::Open => "open",
        JobStatus::Blocked => "blocked",
        JobStatus::PendingSignOff => "pending-sign-off",
        JobStatus::Closed => "closed",
        JobStatus::Cancelled => "cancelled",
    }
}

/// Per-kind distribution of Jobs across step sort_order tiers.
///
/// Response shape:
/// ```json
/// {
///   "by_kind": {
///     "refurb-used": { "tiers": { "0": 12, "1": 55153, "2": 4, "-1": 100 } },
///     "sale":        { "tiers": { "0": 55779, "1": 3, "-1": 177 } }
///   },
///   "status": "open"
/// }
/// ```
/// `tiers[-1]` = Jobs with every step terminal (completed/skipped)
/// but the Job not yet closed (e.g. awaiting sign-off). Frontend maps
/// tier → lifecycle phase via the JobKind's step list (sort_order
/// buckets).
pub(super) async fn jobs_phase_distribution<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Query(q): Query<JobsSummaryQuery>,
) -> Response {
    match state.jobs.jobs_tier_distribution(q.status).await {
        Ok(rows) => {
            let mut by_kind: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            for (kind, tier, count) in rows {
                let entry = by_kind
                    .entry(kind)
                    .or_insert_with(|| serde_json::json!({ "tiers": {} }));
                if let Some(tiers) = entry.get_mut("tiers").and_then(|v| v.as_object_mut()) {
                    tiers.insert(tier.to_string(), serde_json::Value::from(count));
                }
            }
            Json(serde_json::json!({
                "by_kind": by_kind,
                "status": q.status.map(job_status_str_public),
            }))
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize, Default)]
pub(super) struct CreateJobQuery {
    /// When false, the handler creates the Job row but skips
    /// materializing the JobKind's steps. Used by the brewery
    /// engine, which emits its own deterministic-UUID step
    /// creates via POST /api/jobs/{id}/steps and would otherwise
    /// land 2× the steps per Job. SPA / admin creates omit the
    /// param so steps auto-materialize (default true).
    #[serde(default = "default_materialize_steps")]
    materialize_steps: bool,
}

fn default_materialize_steps() -> bool {
    true
}

pub(super) async fn create_job<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<CreateJobQuery>,
    Json(mut raw): Json<serde_json::Value>,
) -> Response {
    // `opened_on` is optional on the wire: dispatcher- and
    // operator-initiated creates omit it and inherit the authoritative
    // (sim-aware) clock; the simulator supplies it explicitly to stamp
    // historical sim-dates. Inject the default before deser since the
    // shared `Job` type requires the field.
    let now = boss_clock_client::now_from(&state.clock).await;
    if raw.get("opened_on").is_none_or(|v| v.is_null())
        && let Some(obj) = raw.as_object_mut()
    {
        obj.insert("opened_on".to_string(), serde_json::json!(now.date_naive()));
    }
    let mut job: Job = match serde_json::from_value(raw) {
        Ok(j) => j,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid job body: {e}"),
            )
                .into_response();
        }
    };

    // Validate the kind against the JobKind registry. When no registry
    // is plumbed (older tests) we accept any kind string. We capture
    // the active spec here so the step-materialization pass below
    // doesn't need a second registry lookup.
    let kind_spec = if let Some(ref reg) = state.kind_registry {
        match reg.get_active(&job.kind).await {
            Ok(spec) => Some(spec),
            Err(crate::registry::JobKindError::NotFound(_)) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("unknown or inactive job kind: {}", job.kind),
                )
                    .into_response();
            }
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
    } else {
        None
    };

    // Pin the Job to the kind's active version — the version it opens
    // under. Per docs/architecture-decisions.md §Jobs, JobKinds, Steps:
    // in-flight Jobs pin to the version they opened under, and creation
    // is blocked against draft/retired kinds (enforced by get_active
    // above, which 400s on an inactive kind). Server-assigned —
    // overrides any value a client put on the wire.
    if let Some(ref spec) = kind_spec {
        job.job_kind_version = spec.version;
    }

    // When `subject = Subject::Custom`, validate `custom_kind` against
    // the SubjectKind registry. Closed-variant subjects (System,
    // Account, Vendor, …) are intrinsically valid and bypass the check.
    if let Err(resp) = validate_custom_subject(&state, &job.subject).await {
        return resp;
    }

    // When the existence checker is plumbed, ask the upstream service
    // whether the Subject id exists. NotFound → 400; Unavailable →
    // fail CLOSED (503). The abort-by-default posture (subject-model
    // design Q2, resolved 2026-07-15): a Job about an unverifiable
    // subject is exactly the phantom class the gate exists to stop,
    // and with the PgSubjectExistence adapter "unavailable" means the
    // same Postgres this handler is about to write to — nothing
    // downstream would succeed anyway.
    if let Some(check) = &state.subject_existence {
        match check.check(&job.subject).await {
            Ok(()) => {}
            Err(crate::subject_existence::SubjectExistenceError::NotFound(id)) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("subject does not exist: {id}"),
                )
                    .into_response();
            }
            Err(crate::subject_existence::SubjectExistenceError::Unavailable(msg)) => {
                tracing::warn!(
                    %msg,
                    job_kind = %job.kind,
                    "subject existence check unavailable; failing closed"
                );
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "subject existence check unavailable — retry",
                )
                    .into_response();
            }
        }
    }

    let job_id = job.id;

    // Log-first: emit JOB_CREATED, then write the projection. The log
    // is authoritative — if the emit fails we don't write the
    // projection at all (returning 500), so the jobs table never runs
    // ahead of the log and a rebuild can't reconstruct a phantom row.
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let emitted = state
        .publisher
        .emit_with_actor_at(
            events::JOB_CREATED,
            actor,
            serde_json::to_value(&job).unwrap_or_default(),
            now,
        )
        .await;
    if !emitted {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to publish jobs.job.created — see audit_log writer logs".to_string(),
        )
            .into_response();
    }

    if let Err(e) = state.jobs.create_job_at(&job, now).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Materialize the JobKind's steps into actual `steps` rows. Job
    // kinds with no steps (`ad-hoc`, where the user defines work as
    // they go) materialize into zero steps.
    //
    // The brewery engine sets ?materialize_steps=false because it
    // emits its own deterministic-UUID step creates via
    // POST /api/jobs/{id}/steps; without the opt-out, every Job
    // would carry 2× the spec's step count.
    if q.materialize_steps
        && let Some(spec) = kind_spec
    {
        // Live-API path stamps `{day}` tokens against the
        // clock-api's current day so payroll / period-end
        // metadata derives from the system clock (sim or wall
        // depending on the deploy's clock mode), matching what
        // the sim engine does with its own day cursor.
        let steps = crate::registry::materialize_steps_at(
            &spec,
            &job.subject,
            job_id,
            &job.metadata,
            boss_core::job::StepId::new,
            Some(now.date_naive()),
            // Resolve trigger provenance at materialization: the firing
            // trigger (named by `metadata.trigger_name`) is born
            // `Completed`, its alternatives `Skipped`. Every production
            // Job — dispatcher-spawned, sim, operator — flows through
            // here, so this is the single point that makes triggers
            // honest.
            Some(state.step_registry.as_ref()),
        );
        // Materialization is ATOMIC from an observer's view. A consumer
        // that reacts to a `step.ready` event — the dispatcher's marker
        // auto-complete, a delegate-subjob fork — must see the COMPLETE
        // step graph. So two passes: (1) emit STEP_CREATED + persist
        // EVERY step (log-first: emit, then write); (2) only once the
        // whole graph is durable, fire `step.ready`. Materialized steps
        // need their STEP_CREATED events or the rebuilder at
        // boss-jobs/src/rebuild.rs can't reconstruct the rows from
        // audit_log and the projection diverges from the log.
        for step in &steps {
            let step_actor = user
                .ambient_actor()
                .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
            let step_emitted = state
                .publisher
                .emit_with_actor_at(
                    events::STEP_CREATED,
                    step_actor,
                    serde_json::to_value(step).unwrap_or_default(),
                    now,
                )
                .await;
            if !step_emitted {
                tracing::warn!(
                    job_id = %job_id,
                    step_id = %step.id,
                    "failed to publish jobs.step.created for materialized step",
                );
                continue;
            }
            if let Err(e) = state.jobs.add_step_at(step, now).await {
                tracing::warn!(
                    job_id = %job_id,
                    step_id = %step.id,
                    error = %e,
                    "failed to write materialized step projection",
                );
            }
        }
        // Second pass: the full step graph is now persisted, so any observer
        // of a `step.ready` event sees a complete, consistent Job.
        // `materialize_steps_at` ran the open-time readiness pass, so the
        // trigger (and any step whose `ready_when` already holds) is `Ready`
        // here — fire `step.ready.<kind>` so the dispatcher's marker
        // auto-complete + delegate-subjob forks (D7) react against the whole
        // graph, never a partial one.
        for step in &steps {
            if step.status == StepStatus::Ready && !step.kind.is_empty() {
                let ready_actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                emit_step_ready(&state, &job, step, &ready_actor, now).await;
            }
        }
        if !steps.is_empty() {
            tracing::debug!(
                job_id = %job_id,
                kind = %job.kind,
                step_count = steps.len(),
                "materialized job kind steps",
            );
        }
    }

    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": job_id.to_string() })),
    )
        .into_response()
}

#[derive(Serialize)]
pub(super) struct JobDetail {
    #[serde(flatten)]
    job: Job,
    steps: Vec<Step>,
}

pub(super) async fn get_job<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };

    match state.jobs.get_job(&job_id).await {
        Ok(Some(job)) => {
            let steps = state.jobs.list_steps(&job_id).await.unwrap_or_default();
            Json(JobDetail { job, steps }).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "job not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Per-Job SSE stream. Server-side polls the Job + its steps
/// every 2s and pushes a `JobDetail` JSON frame when something
/// observable has changed (job status / priority / closed_on,
/// or any step's status / completed_on / metadata-version
/// signature), so an operator viewing the page sees a step go
/// from in-progress to done the moment the daemon (or another
/// operator) makes the transition.
///
/// Doesn't subscribe to NATS directly — boss-jobs-api carries
/// its own DB pool but isn't a bus subscriber. Server-side
/// polling is the same shape as `/api/jobs/sim-clock/stream`;
/// the dedupe keeps push volume low (most ticks hold steady).
pub(super) async fn job_stream<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
) -> impl axum::response::IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use std::convert::Infallible;
    use std::time::Duration;

    // Parse upfront; on invalid id the stream emits one `error`
    // frame and exits. Keeping a single stream type lets `Sse::new`
    // accept it (each `stream!` macro produces a unique type, so
    // branched-return variants don't unify).
    let parsed_id = parse_job_id(&id);

    let stream = async_stream::stream! {
        let Some(job_id) = parsed_id else {
            yield Ok::<_, Infallible>(
                SseEvent::default().event("error").data("invalid job id"),
            );
            return;
        };
        // Compact change-detection signature: (job_status,
        // priority, closed_on, [(step_id, status, completed_on)]).
        // Cheap to hash, doesn't depend on the metadata blob's
        // exact JSON serialization (per-step `updated_at` would
        // be ideal but isn't always tracked uniformly).
        type JobSig = (
            Option<String>,    // job status as text
            Option<String>,    // priority as text
            Option<String>,    // closed_on as ISO string
            Vec<(boss_core::job::StepId, String, Option<String>)>,
        );
        fn signature(
            job: &boss_core::job::Job,
            steps: &[boss_core::job::Step],
        ) -> JobSig {
            let mut step_sig: Vec<_> = steps
                .iter()
                .map(|s| {
                    (
                        s.id,
                        format!("{:?}", s.status),
                        s.completed_on.map(|d| d.to_string()),
                    )
                })
                .collect();
            // StepId wraps a Uuid; sort by its display form so the
            // signature is stable across runs without StepId itself
            // needing an Ord impl.
            step_sig.sort_by_key(|s| s.0.to_string());
            (
                Some(format!("{:?}", job.status)),
                Some(format!("{:?}", job.priority)),
                job.closed_on.map(|d| d.to_string()),
                step_sig,
            )
        }

        // Push initial snapshot. last_sig is bound from the
        // success branch so the compiler doesn't warn about a
        // dead-write on a `None` initializer that's never read.
        let initial = state.jobs.get_job(&job_id).await;
        let mut last_sig: JobSig = if let Ok(Some(job)) = initial {
            let steps = state.jobs.list_steps(&job_id).await.unwrap_or_default();
            let sig = signature(&job, &steps);
            let detail = JobDetail { job, steps };
            if let Ok(json) = serde_json::to_string(&detail) {
                yield Ok::<_, Infallible>(SseEvent::default().data(json));
            }
            sig
        } else {
            yield Ok::<_, Infallible>(
                SseEvent::default().event("error").data("job not found"),
            );
            return;
        };

        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(
            tokio::time::MissedTickBehavior::Delay,
        );
        loop {
            tick.tick().await;
            let Ok(Some(job)) = state.jobs.get_job(&job_id).await else {
                // Job vanished mid-stream (cancelled, deleted).
                yield Ok::<_, Infallible>(
                    SseEvent::default().event("gone").data(""),
                );
                break;
            };
            let steps = state.jobs.list_steps(&job_id).await.unwrap_or_default();
            let sig = signature(&job, &steps);
            if last_sig != sig {
                let detail = JobDetail { job, steps };
                if let Ok(json) = serde_json::to_string(&detail) {
                    yield Ok::<_, Infallible>(SseEvent::default().data(json));
                }
                last_sig = sig;
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub(super) async fn update_job<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(mut job): Json<Job>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };

    // Ensure path ID matches body ID.
    job.id = job_id;

    let existing = match state.jobs.get_job(&job_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return (StatusCode::NOT_FOUND, "job not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let old_status = existing.status;

    // Pick the right policy action: transitioning to Closed is a Close
    // action (more restricted than Update); everything else is Update.
    let action = if job.status == JobStatus::Closed && old_status != JobStatus::Closed {
        Action::Close
    } else {
        Action::Update
    };

    // Policy check: role allowed to perform this action on Jobs at all?
    let decision = match state.policy.check(&user, action, Resource::job()).await {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };
    let scope = match decision {
        Decision::Deny { reason } => {
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
        Decision::Allow { scope } => scope,
    };

    // Scope check: is THIS specific Job inside the caller's scope?
    if !scope_matches(&user, &scope, &existing) {
        return (StatusCode::FORBIDDEN, "job is outside your scope").into_response();
    }

    // Same opt-in subject validation as create_job. Catches a body that
    // swaps Subject::System(…) for Subject::Custom { custom_kind:
    // "made-up" } on update.
    if let Err(resp) = validate_custom_subject(&state, &job.subject).await {
        return resp;
    }

    let now = boss_clock_client::now_from(&state.clock).await;
    if let Err(e) = state.jobs.update_job_at(&job, now).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // State event — full row state, what the rebuild consumes.
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    state
        .publisher
        .emit_with_actor_at(
            events::JOB_UPDATED,
            actor.clone(),
            serde_json::to_value(&job).unwrap_or_default(),
            now,
        )
        .await;

    // Marker events for downstream consumers (UI, integrations) —
    // duplicates state already in JOB_UPDATED but gives them a topic
    // to filter on without payload matching. Rebuild ignores these.
    if old_status != job.status {
        state
            .publisher
            .emit_with_actor_at(
                events::JOB_STATUS_CHANGED,
                actor.clone(),
                serde_json::json!({
                    "id": job.id.to_string(),
                    "old_status": old_status,
                    "new_status": job.status,
                }),
                now,
            )
            .await;

        if job.status == JobStatus::Closed {
            state
                .publisher
                .emit_with_actor_at(
                    events::JOB_CLOSED,
                    actor.clone(),
                    serde_json::json!({
                        "id": job.id.to_string(),
                        "closed_on": job.closed_on,
                    }),
                    now,
                )
                .await;
        }
    }

    StatusCode::NO_CONTENT.into_response()
}
