//! `POST /api/jobs/{id}/release` — release a car held for its review, on
//! that review's verdict (backlog ed8e5c85). This is the ONE door.
//! `boss release <car> --review <run>` calls it, and so does the yard's
//! Release control. The rule is [`crate::car_release::judge`], the code
//! `boss-cli` ran alone until now. The write goes through the step
//! metadata merge handler ([`patch_step_metadata`]) unchanged, so the
//! release passes the same policy, scope, signer and terminal-step checks
//! as any other write to the review step. It is one body: `{hold: null,
//! release: record}`, as `boss release` always wrote it.
//!
//! Body: [`crate::car_release::ReleaseAsk`]. Answers 200 with `{car,
//! branch, head, release}` after the step reads back released, 409 with
//! the rule's refusal as plain text, 422 for a body
//! that is not one, and the merge door's own answer when it refuses the
//! write.
//!
//! Policy: the packet read scope to read the car and the review run,
//! then the merge door's `(Update, step)` gate on the write. The `by` on
//! the record is the request's authenticated caller. A body cannot name
//! it, and an anonymous request is refused before anything is read.

use super::*;

use axum::extract::Path;

use crate::car_release::{self, ExecutorRecordReader, ReleaseAsk};

/// The rule's reads, answered from this process's own stores. These
/// are the same rows `GET /api/workflows/{kind}/versions/{v}` and `GET
/// /api/jobs/{id}/steps/{step}/records/{key}` serve the CLI.
pub(super) struct StoreReader<'a, R: JobsRepository, B: EventBus> {
    pub state: &'a JobsApiState<R, B>,
}

#[async_trait::async_trait]
impl<R: JobsRepository + 'static, B: EventBus + 'static> ExecutorRecordReader
    for StoreReader<'_, R, B>
{
    async fn protocol_version(
        &self,
        kind: &str,
        version: i64,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let Some(reg) = self.state.kind_registry.as_ref() else {
            anyhow::bail!("the workflow registry is not configured");
        };
        let Ok(version) = i32::try_from(version) else {
            return Ok(None);
        };
        match reg.get_version(kind, version).await {
            Ok(spec) => Ok(Some(serde_json::to_value(spec)?)),
            Err(WorkflowError::NotFound(_)) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("reading {kind} v{version}: {e}")),
        }
    }

    async fn executor_original(
        &self,
        job: uuid::Uuid,
        step: uuid::Uuid,
        key: &str,
    ) -> anyhow::Result<Option<crate::first_record::FirstRecord>> {
        let step_id = boss_core::job::StepId::from_uuid(step);
        let record = self
            .state
            .jobs
            .first_step_record(&step_id, key)
            .await
            .map_err(|e| anyhow::anyhow!("reading record {key}: {e}"))?;
        if let Some(r) = &record {
            car_release::immutable_record_resource(r, job, step, key)?;
        }
        Ok(record)
    }
}

/// A packet as `GET /api/jobs/{id}` serves it: the rule reads the same
/// shape on both sides of the wire.
pub(super) async fn packet_value<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    job: Job,
) -> Result<serde_json::Value, String> {
    let steps = state
        .jobs
        .list_steps(&job.id)
        .await
        .map_err(|e| format!("reading the steps of {}: {e}", job.id))?;
    serde_json::to_value(JobDetail::new(job, steps)).map_err(|e| e.to_string())
}

/// The yard's held-car rows, each with what its release rests on
/// ([`car_release::decision`]): the same judge the door runs, asked as a
/// person with no run, so a row offers Release exactly when the door
/// would grant it (backlog ed8e5c85). Only an operator's hold is asked.
/// A red re-gate is not released by a review. A car or run this caller
/// cannot read leaves its row as it was (a car), or says so (a run).
pub(super) async fn with_release_decisions<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    mut status: crate::yard::YardStatus,
) -> crate::yard::YardStatus {
    if status.held_cars.is_empty() {
        return status;
    }
    let Ok(scope) = job_read_scope(state, user).await else {
        return status;
    };
    let reader = StoreReader { state };
    for held in status
        .held_cars
        .iter_mut()
        .filter(|h| h.kind == crate::yard::CarHoldKind::Operator)
    {
        let Some(car_id) = parse_job_id(&held.car.id) else {
            continue;
        };
        let Ok(job) = readable_job(state, user, &scope, &car_id).await else {
            continue;
        };
        let Ok(car) = packet_value(state, job).await else {
            continue;
        };
        let run = match car_release::dispatched_reviewer(&car).and_then(parse_job_id) {
            None => Err("its review step names no reviewer run".to_string()),
            Some(run_id) => match readable_job(state, user, &scope, &run_id).await {
                Ok(job) => packet_value(state, job).await,
                Err(_) => Err("the reviewer's run could not be read".to_string()),
            },
        };
        held.release =
            Some(car_release::decision(&reader, &car, run.as_ref().map_err(Clone::clone)).await);
    }
    status
}

/// The rule's refusal, as plain text: the CLI prints it after the status
/// line, and the yard shows it as it came. A JSON body would reach both
/// with its quotes escaped.
fn refused(why: impl Into<String>) -> Response {
    (StatusCode::CONFLICT, why.into()).into_response()
}

pub(super) async fn release_car<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    caller: Option<axum::Extension<crate::field_writer::CredentialedCaller>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let ask: ReleaseAsk = match serde_json::from_value(body) {
        Ok(ask) => ask,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("a release body is {{review, head?, main_sha?, run?, agent_shell?}}: {e}"),
            )
                .into_response();
        }
    };
    // Who released it is the record's first fact; an unnamed release is
    // refused before it is read, as the CLI refused one before sending.
    if user.id.trim().is_empty() || user.id == boss_policy_client::User::ANONYMOUS_ID {
        return (
            StatusCode::UNAUTHORIZED,
            "an unnamed release is refused: the record names who released the car",
        )
            .into_response();
    }
    let scope = match job_read_scope(&state, &user).await {
        Ok(scope) => scope,
        Err(refusal) => return refusal,
    };
    let car_job = match readable_path_job(&state, &user, &scope, &id).await {
        Ok(job) => job,
        Err(refusal) => return refusal,
    };
    let car_id = car_job.id;
    let car = match packet_value(&state, car_job).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let Some(run_id) = parse_job_id(ask.review.trim()) else {
        return refused(format!(
            "--review {:?} is not a full run id — the record carries the id that was checked, \
             never the characters that were typed",
            ask.review
        ));
    };
    let run = match readable_job(&state, &user, &scope, &run_id).await {
        Ok(job) => match packet_value(&state, job).await {
            Ok(v) => v,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        },
        Err(refusal) => return refusal,
    };
    let at = crate::car::stamp(boss_clock_client::wall_now());
    let reader = StoreReader { state: &state };
    let granted = match car_release::judge(&reader, &car, &run, &ask, &user.id, &at).await {
        Ok(g) => g,
        Err(why) if why.starts_with("boss release:") => return refused(why),
        Err(why) => return refused(format!("boss release: REFUSED — {why}")),
    };
    let written = patch_step_metadata(
        State(state.clone()),
        Path((car_id.to_string(), granted.step_id.clone())),
        CurrentUser(user),
        caller,
        headers,
        Json(granted.patch.clone()),
    )
    .await;
    if !written.status().is_success() {
        return written;
    }
    // Read back, as the CLI did: an answered write that does not read as
    // released is a failure, said aloud.
    let after = match state.jobs.get_job(&car_id).await {
        Ok(Some(job)) => match packet_value(&state, job).await {
            Ok(v) => v,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        },
        Ok(None) => return packet_not_found(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let review_md = car_release::review_step(&after)
        .and_then(|s| s.get("metadata"))
        .cloned()
        .unwrap_or_default();
    if let Some(h) = crate::stranded::hold_reason(&review_md) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the release was written but the review step still reads as held: {h:?}"),
        )
            .into_response();
    }
    if review_md.pointer(&format!("/{}/sha", crate::car::RELEASE))
        != Some(&serde_json::json!(granted.head))
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "the release was written but the car does not read back a release at {}",
                granted.head
            ),
        )
            .into_response();
    }
    Json(serde_json::json!({
        "car": car_id.to_string(),
        "branch": after.pointer("/metadata/branch").cloned().unwrap_or_default(),
        "head": granted.head,
        "release": review_md.get(crate::car::RELEASE).cloned().unwrap_or_default(),
    }))
    .into_response()
}
