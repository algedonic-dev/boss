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
//! THE HOLD IT LIFTS IS THE HOLD THAT WAS JUDGED (review 792b871a N1).
//! The body names the hold its asker read (`hold: {reason, sha}`,
//! required); the judge refuses a car carrying another, and the write
//! carries the hold the judge read as a precondition the adapter checks
//! under the row's lock. A hold written at any point after the asker
//! read the car is therefore refused, never lifted, whether it landed
//! before the POST (the rule's read-then-ask gap, the yard's poll) or
//! between this door's read and its write. (The same text at the same
//! head is the same hold to the record, and is not refused.)
//!
//! Policy: the packet read scope to read the car and the review run,
//! then the merge door's `(Update, step)` gate on the write. The `by` on
//! the record is the request's authenticated caller, in the spelling its
//! event records ([`release_actor`]). A body cannot name it, and an
//! anonymous request is refused before anything is read.

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

/// The `by` a release records: the actor the merge event will record for
/// this caller (`User::ambient_actor`), in its text form. A dispatcher
/// rule's wire id `rule:<name>` is thereby recorded as the automation
/// `automation:rule:<name>`, as its event is (backlog 0cfc57e6), and the
/// judge reads it as an automation. Every other id prints back as sent.
fn release_actor(user: &boss_policy_client::User) -> String {
    user.ambient_actor()
        .map(|a| a.to_string())
        .unwrap_or_else(|| user.id.clone())
}

/// The rule's refusal, as plain text: the CLI prints it after the status
/// line, and the yard shows it as it came. A JSON body would reach both
/// with its quotes escaped.
fn refused(why: impl Into<String>) -> Response {
    (StatusCode::CONFLICT, why.into()).into_response()
}

/// The merge door's answer to the release write, as the release door
/// says it. The step race (409 `step_changed`) here means the review
/// step moved between the judge's read and the write — under the
/// precondition the door sends, its hold — so it is the rule's refusal
/// in the rule's words, and nothing was written. Every other answer is
/// passed on as the merge door gave it.
async fn hold_moved_refusal(written: Response, car: boss_core::job::JobId) -> Response {
    if written.status() != StatusCode::CONFLICT {
        return written;
    }
    let (parts, body) = written.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 64 * 1024).await else {
        return (parts.status, "the merge door's refusal could not be read").into_response();
    };
    let race = serde_json::from_slice::<serde_json::Value>(&bytes)
        .is_ok_and(|b| crate::step_metadata_write::is_step_race(&b));
    if race {
        return refused(format!(
            "boss release: REFUSED — car {}'s review step changed while this release was being \
             written, so the hold it carries now is not the hold that was judged (review \
             792b871a N1). Nothing was written. Read the car again and judge the hold it \
             carries.",
            car.to_string().chars().take(8).collect::<String>()
        ));
    }
    Response::from_parts(parts, axum::body::Body::from(bytes))
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
                format!(
                    "a release body is {{review, hold: {{reason, sha}}, head?, main_sha?, run?, \
                     agent_shell?}} — `hold` is the hold the asker judged, as it read the car: \
                     {e}"
                ),
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
    let by = release_actor(&user);
    let granted = match car_release::judge(&reader, &car, &run, &ask, &by, &at).await {
        Ok(g) => g,
        Err(why) if why.starts_with("boss release:") => return refused(why),
        Err(why) => return refused(format!("boss release: REFUSED — {why}")),
    };
    // The write lands only while the review step still carries the hold
    // the judge read (review 792b871a N1): the asker's hold was compared
    // with this read, and this read is compared with the row under the
    // write's own lock, so no hold written in between is lifted.
    let written = patch_step_metadata_expecting(
        State(state.clone()),
        Path((car_id.to_string(), granted.step_id.clone())),
        CurrentUser(user),
        caller,
        headers,
        Json(granted.patch.clone()),
        granted.expect.as_object().cloned(),
    )
    .await;
    if !written.status().is_success() {
        return hold_moved_refusal(written, car_id).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str) -> boss_policy_client::User {
        serde_json::from_value(serde_json::json!({"id": id, "role": "platform-admin"}))
            .expect("a user")
    }

    async fn said(r: Response) -> (StatusCode, String) {
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 64 * 1024)
            .await
            .expect("a body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// THE DOOR'S OWN READ-TO-WRITE GAP (review 792b871a N1). The write
    /// carries the hold the judge read as a precondition; when the merge
    /// door answers the step race, the review step moved after the
    /// judgement, and the release door says so in the rule's words —
    /// nothing was written. Any other answer passes through as given.
    #[tokio::test]
    async fn a_step_race_on_the_release_write_is_the_rules_refusal() {
        let car = boss_core::job::JobId::from_uuid(
            uuid::Uuid::parse_str("c0ffee00-0000-4000-8000-000000000000").unwrap(),
        );
        let race = (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": crate::step_metadata_write::STEP_CHANGED_ERROR,
                "code": crate::step_metadata_write::STEP_CHANGED_CODE,
            })),
        )
            .into_response();
        let (status, text) = said(hold_moved_refusal(race, car).await).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            text.starts_with("boss release: REFUSED — car c0ffee00"),
            "{text}"
        );
        assert!(text.contains("Nothing was written"), "{text}");
        for (status, body) in [
            (StatusCode::CONFLICT, "step is terminal"),
            (StatusCode::FORBIDDEN, "no grant"),
        ] {
            let (s, t) = said(hold_moved_refusal((status, body).into_response(), car).await).await;
            assert_eq!((s, t.as_str()), (status, body));
        }
    }

    /// WHO IS CREDITED (backlog 0cfc57e6). A dispatcher rule calls with
    /// the wire id `rule:<name>`, and the merge event the release writes
    /// records it as `automation:rule:<name>` (`User::ambient_actor`).
    /// The record's `by` is the same actor in the same spelling, so a
    /// reader of `by` and a reader of the event agree on who released
    /// the car. A person and an agent are stamped as they always were.
    #[test]
    fn the_release_is_stamped_with_the_actor_the_event_records() {
        assert_eq!(
            release_actor(&user("rule:release-a-held-car-on-its-reviewer-verdict")),
            "automation:rule:release-a-held-car-on-its-reviewer-verdict"
        );
        for id in ["emp-david", "agent-codex", "automation:rule:x"] {
            assert_eq!(release_actor(&user(id)), id);
        }
    }
}
