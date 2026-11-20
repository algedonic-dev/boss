//! Recovery after a writer committed its state but missed its readiness pass.
//! The clock invokes the same pinned protocol/CAS evaluator as ordinary writes.
//! GET is an interval audit, not a transaction-wide snapshot or a write.
//!
//! A READY MARKER IS RECOVERED TOO (backlog ee399c2c). A marker the system
//! completes itself (`StepType::completes_itself_when_ready`) is completed
//! by `complete-marker-on-step-ready` on its `step.ready` event, and only
//! there. Measured 2026-10-30 04:12-04:30Z: gate-run a2e19072's `green` sat
//! READY with no `completed_by` after its verdict, and the packet never
//! settled. A re-send of the verdict cannot re-emit a ready event for a
//! step already ready, and this sweep read only PENDING steps — so a lost
//! or timed-out event (the API was logging 35-60 statement timeouts per
//! ten minutes) left the packet open forever. Each pass now completes
//! every marker that was READY when it read the packet, through the same
//! step PUT the rule sends, credited to the rule that fired the pass.

use super::*;
use boss_core::actor::ActorId;
use boss_policy_client::{Predicate, Scope, User};
use std::collections::HashSet;

/// A ready marker a pass read, and what it did about it.
#[derive(Serialize)]
struct ReadyMarker {
    job_id: String,
    step_id: String,
    kind: String,
    /// `ready` — the audit writes nothing; `completed` — this pass
    /// completed it; `raced` — another writer (the event's own rule,
    /// most likely) moved the row first, so this pass recorded nothing
    /// and the next one reads it afresh; `needs-a-person` — it owes a
    /// field only a person can fill, as `jobs.complete_step` says too.
    outcome: &'static str,
}

const PAGE: i64 = 500;
const MAX_JOBS: usize = 10_000;

#[derive(Serialize)]
struct Waiting {
    job_id: String,
    step_id: String,
    workflow_kind: String,
    workflow_version: i32,
    ready_when: String,
}

#[derive(Serialize)]
struct Report {
    started_at: chrono::DateTime<chrono::Utc>,
    finished_at: chrono::DateTime<chrono::Utc>,
    complete: bool,
    global: bool,
    jobs_discovered: usize,
    jobs_scanned: usize,
    satisfied_pending: usize,
    data: Vec<Waiting>,
    ready_markers: Vec<ReadyMarker>,
    errors: Vec<String>,
}

pub(super) async fn audit<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    run(state, user, false).await
}

pub(super) async fn reconcile<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    run(state, user, true).await
}

async fn run<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: Arc<JobsApiState<R, B>>,
    user: User,
    repair: bool,
) -> Response {
    let predicate = match state.policy.scope_of(&user, controls::READ_JOB).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    if matches!(predicate, Predicate::None) {
        return (
            StatusCode::FORBIDDEN,
            "readiness audit requires a readable job scope",
        )
            .into_response();
    }
    let mut write_scopes = Vec::new();
    if repair {
        for control in [controls::UPDATE_JOB, controls::UPDATE_STEP] {
            match state.policy.ask(&user, control).await {
                Ok(Decision::Allow { scope }) if !matches!(scope, Scope::None) => {
                    write_scopes.push(scope)
                }
                Ok(Decision::Allow { .. }) | Ok(Decision::Deny { .. }) => {
                    return (
                        StatusCode::FORBIDDEN,
                        "readiness repair requires job and step update authority",
                    )
                        .into_response();
                }
                Err(e) => return e.into_response(),
            }
        }
    }
    let actor = match user.ambient_actor() {
        Some(actor) => actor,
        None if !repair => ActorId::automation("readiness-audit"),
        None => {
            return (
                StatusCode::FORBIDDEN,
                "readiness repair requires an identified actor",
            )
                .into_response();
        }
    };
    let scope = JobScope::from_predicate(&user, &predicate);
    if matches!(scope, JobScope::None) {
        return (
            StatusCode::FORBIDDEN,
            "readiness audit scope admits no jobs",
        )
            .into_response();
    }
    let mut report = Report {
        started_at: boss_clock_client::wall_now(),
        finished_at: boss_clock_client::wall_now(),
        complete: false,
        global: matches!(scope, JobScope::All),
        jobs_discovered: 0,
        jobs_scanned: 0,
        satisfied_pending: 0,
        data: Vec::new(),
        ready_markers: Vec::new(),
        errors: Vec::new(),
    };
    let filter = JobFilter {
        status: Some(JobStatus::Open),
        scope: scope.clone(),
        oldest_first: true,
        ..Default::default()
    };
    // Finish discovery BEFORE any writes: a terminal transition must not move
    // the offset under our feet. A changing inventory refuses completeness.
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    let mut expected = None;
    loop {
        let (rows, total) = match state.jobs.list_jobs(&filter, PAGE, ids.len() as i64).await {
            Ok(read) => read,
            Err(e) => {
                report.errors.push(format!("open job inventory: {e}"));
                break;
            }
        };
        if total < 0 || total as usize > MAX_JOBS || expected.is_some_and(|old| old != total) {
            report.errors.push(format!("open job inventory changed or exceeds {MAX_JOBS} jobs: expected {expected:?}, got {total}"));
            break;
        }
        expected = Some(total);
        if rows.is_empty() && ids.len() < total as usize {
            report.errors.push(format!(
                "open job inventory ended at {} of {total}",
                ids.len()
            ));
            break;
        }
        for job in rows {
            if !seen.insert(job.id.to_string()) {
                report
                    .errors
                    .push(format!("open job inventory repeated {}", job.id));
                break;
            }
            ids.push(job.id);
        }
        if ids.len() > total as usize {
            report
                .errors
                .push("open job inventory returned more rows than its total".into());
        }
        if !report.errors.is_empty() || ids.len() >= total as usize {
            break;
        }
    }
    report.jobs_discovered = ids.len();
    if report.errors.is_empty() {
        for id in ids {
            match inspect(&state, &user, &scope, &write_scopes, &actor, &id, repair).await {
                Ok((rows, markers)) => {
                    report.jobs_scanned += 1;
                    report.data.extend(rows);
                    report.ready_markers.extend(markers);
                }
                Err(e) => report.errors.push(format!("job {id}: {e}")),
            }
        }
    }
    report.satisfied_pending = report.data.len();
    report.complete = report.errors.is_empty();
    report.finished_at = boss_clock_client::wall_now();
    let status = if report.complete {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report)).into_response()
}

async fn inspect<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &User,
    read_scope: &JobScope,
    write_scopes: &[Scope],
    actor: &ActorId,
    id: &boss_core::job::JobId,
    repair: bool,
) -> Result<(Vec<Waiting>, Vec<ReadyMarker>), String> {
    let Some(mut job) = state.jobs.get_job(id).await.map_err(|e| e.to_string())? else {
        return Err("packet disappeared during audit".into());
    };
    if job.status != JobStatus::Open || !read_scope.admits(&job) {
        return Ok((Vec::new(), Vec::new()));
    }
    if repair
        && write_scopes
            .iter()
            .any(|scope| !scope_matches(user, scope, &job))
    {
        return Err("packet is outside repair authority".into());
    }
    let mut steps = state.jobs.list_steps(id).await.map_err(|e| e.to_string())?;
    // The markers READY as this pass read the packet — never one this
    // pass promotes below: that one's own event is in flight, and the
    // next pass recovers it if the event is lost.
    let mut markers = Vec::new();
    for step in steps.iter().filter(|step| {
        step.status == StepStatus::Ready
            && state
                .step_registry
                .get(&step.kind)
                .is_some_and(crate::step_registry::StepType::completes_itself_when_ready)
    }) {
        let outcome = if repair {
            complete_marker(state, user, actor, step).await?
        } else {
            "ready"
        };
        markers.push(ReadyMarker {
            job_id: job.id.to_string(),
            step_id: step.id.to_string(),
            kind: step.kind.clone(),
            outcome,
        });
    }
    if markers.iter().any(|m| m.outcome == "completed") {
        // A completed terminal closes the packet; any other completion
        // settled its readiness. Judge what is left from the rows now.
        job = match state.jobs.get_job(id).await.map_err(|e| e.to_string())? {
            Some(job) if job.status == JobStatus::Open => job,
            _ => return Ok((Vec::new(), markers)),
        };
        steps = state.jobs.list_steps(id).await.map_err(|e| e.to_string())?;
    }
    if !steps.iter().any(|step| step.status == StepStatus::Pending) {
        return Ok((Vec::new(), markers));
    }
    let registry = state
        .kind_registry
        .as_ref()
        .ok_or("workflow registry is unavailable")?;
    let spec = registry
        .get_version(&job.kind, job.workflow_version)
        .await
        .map_err(|e| {
            format!(
                "pinned workflow {} v{}: {e}",
                job.kind, job.workflow_version
            )
        })?;
    let rows = waiting(&job, &spec, &steps)?;
    if !repair || rows.is_empty() {
        return Ok((rows, markers));
    }
    super::steps::reevaluate_reported(state, &job, actor).await?;
    let steps = state.jobs.list_steps(id).await.map_err(|e| e.to_string())?;
    let rows = waiting(&job, &spec, &steps)?;
    if !rows.is_empty() {
        return Err(format!(
            "{} satisfied steps remain pending after bounded reevaluation",
            rows.len()
        ));
    }
    Ok((rows, markers))
}

/// Complete one ready marker exactly as `jobs.complete_step` does: a
/// status-only step PUT, as the caller (the rule that fired this pass),
/// through the one write door — its judgement, its row-version CAS, its
/// settle. What the row says afterwards decides the outcome, because a
/// PUT of a step another writer already completed also answers 204 and
/// writes nothing.
async fn complete_marker<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &User,
    actor: &ActorId,
    step: &Step,
) -> Result<&'static str, String> {
    let response = super::steps::update_step_with_condition(
        state.clone(),
        (step.job_id.to_string(), step.id.to_string()),
        user.clone(),
        axum::http::HeaderMap::new(),
        serde_json::json!({ "status": "completed" }),
        None,
        None,
    )
    .await;
    let status = response.status();
    let named = || format!("marker {} ({})", step.id, step.kind);
    if !status.is_success() {
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        if crate::step_registry::needs_a_person(status.as_u16(), &body) {
            return Ok("needs-a-person");
        }
        let moved = status == StatusCode::CONFLICT
            && serde_json::from_str::<serde_json::Value>(&body)
                .is_ok_and(|v| v["code"] == crate::step_metadata_write::STEP_CHANGED_CODE);
        if !moved {
            return Err(format!("{} refused completion {status}: {body}", named()));
        }
    }
    match state.jobs.get_step(&step.id).await {
        Ok(Some(now)) if now.status == StepStatus::Completed => Ok(
            if now.completed_by.as_ref() == Some(actor) && status.is_success() {
                "completed"
            } else {
                "raced"
            },
        ),
        // Moved by another writer and still open: the next pass reads it.
        Ok(Some(now)) if !status.is_success() || now.status == StepStatus::Skipped => Ok("raced"),
        Ok(_) => Err(format!(
            "{} answered {status} and is not completed",
            named()
        )),
        Err(e) => Err(format!("{}: reading it back: {e}", named())),
    }
}

fn waiting(job: &Job, spec: &WorkflowSpec, steps: &[Step]) -> Result<Vec<Waiting>, String> {
    let mut rows = Vec::new();
    for step in steps
        .iter()
        .filter(|step| step.status == StepStatus::Pending)
    {
        match crate::registry::read_step(spec, steps, &job.subject, &job.metadata, &step.id) {
            crate::registry::ProtocolReading::Holds { ready_when } => rows.push(Waiting {
                job_id: job.id.to_string(),
                step_id: step.id.to_string(),
                workflow_kind: job.kind.clone(),
                workflow_version: job.workflow_version,
                ready_when,
            }),
            crate::registry::ProtocolReading::Waits { .. } => {}
            crate::registry::ProtocolReading::Unpaired => {
                return Err(format!(
                    "pending step {} has no pinned protocol predicate",
                    step.id
                ));
            }
        }
    }
    Ok(rows)
}
