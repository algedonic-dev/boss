//! Recovery after a writer committed its state but missed its readiness pass.
//! The clock invokes the same pinned protocol/CAS evaluator as ordinary writes.
//! GET is an interval audit, not a transaction-wide snapshot or a write.

use super::*;
use boss_core::actor::ActorId;
use boss_policy_client::{Predicate, Scope, User};
use std::collections::HashSet;

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
                Ok(rows) => {
                    report.jobs_scanned += 1;
                    report.data.extend(rows);
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
) -> Result<Vec<Waiting>, String> {
    let Some(job) = state.jobs.get_job(id).await.map_err(|e| e.to_string())? else {
        return Err("packet disappeared during audit".into());
    };
    if job.status != JobStatus::Open || !read_scope.admits(&job) {
        return Ok(Vec::new());
    }
    if repair
        && write_scopes
            .iter()
            .any(|scope| !scope_matches(user, scope, &job))
    {
        return Err("packet is outside repair authority".into());
    }
    let steps = state.jobs.list_steps(id).await.map_err(|e| e.to_string())?;
    if !steps.iter().any(|step| step.status == StepStatus::Pending) {
        return Ok(Vec::new());
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
        return Ok(rows);
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
    Ok(rows)
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
