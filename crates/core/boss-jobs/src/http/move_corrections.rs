//! Native-input diagnoses and append-only corrections; no caller-selected proof.
use super::*;
use crate::{
    move_diagnosis::{self, Entry, Inputs},
    moves::Recorded,
};
use axum::extract::Path;
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Diagnose {
    request_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Correct {
    request_id: Uuid,
    diagnosis_id: Uuid,
    proof_request: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Withdraw {
    request_id: Uuid,
    correction_id: Uuid,
}
fn error(e: impl std::fmt::Display) -> Response {
    (StatusCode::CONFLICT, e.to_string()).into_response()
}
pub(super) async fn authority<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
) -> Result<(), Response> {
    if user.ambient_actor().is_none() {
        return Err((
            StatusCode::FORBIDDEN,
            "a named actor must own a move correction",
        )
            .into_response());
    }
    if let Some(refusal) = super::moves::refused(state, user).await {
        return Err(refusal);
    }
    match state.policy.ask(user, controls::UPDATE_JOB).await {
        Ok(Decision::Allow {
            scope: boss_policy_client::Scope::All,
        }) => Ok(()),
        Ok(_) => Err((
            StatusCode::FORBIDDEN,
            "move correction requires update authority over every named packet",
        )
            .into_response()),
        Err(e) => Err(e.into_response()),
    }
}
pub(super) async fn original<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    seq: i64,
) -> Result<Recorded, Response> {
    if seq < 1 {
        return Err(error("positive original sequence required"));
    }
    let feed = state
        .yard_moves
        .as_ref()
        .ok_or_else(|| error("moves unavailable"))?;
    feed.store
        .since(seq - 1, 1)
        .await
        .map_err(error)?
        .into_iter()
        .find(|r| r.seq == seq)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "original move missing").into_response())
}
pub(super) async fn entries<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> Result<Vec<Entry>, Response> {
    state
        .yard_moves
        .as_ref()
        .ok_or_else(|| error("moves unavailable"))?
        .store
        .correction_entries()
        .await
        .map_err(error)
}
async fn save<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    entry: Entry,
) -> Response {
    let Some(feed) = state.yard_moves.as_ref() else {
        return error("moves unavailable");
    };
    let original = match original(state, entry.original_seq()).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let payload =
        serde_json::json!({"job_id":original.r#move.packet,"entry":entry,"_actor":user.id});
    let Some(actor) = user.ambient_actor() else {
        return error("named actor unavailable");
    };
    let job_id = match Uuid::parse_str(&original.r#move.packet) {
        Ok(id) => boss_core::job::JobId::from_uuid(id),
        Err(e) => return error(e),
    };
    let job = match state.jobs.get_job(&job_id).await {
        Ok(Some(job)) => job,
        _ => return error("original packet partition unavailable"),
    };
    let event = state
        .publisher
        .stamp_with_actor(actor)
        .await
        .with_partition(job.partition)
        .event(entry.event_kind(), payload);
    match feed.store.append_correction(&entry, &event).await {
        Ok(added) => {
            // A correction changes the reading, not the raw move stream.
            let body = match entry {
                Entry::Diagnosis { receipt } => serde_json::to_value(receipt),
                other => serde_json::to_value(other),
            };
            match body {
                Ok(value) => (
                    if added {
                        StatusCode::CREATED
                    } else {
                        StatusCode::OK
                    },
                    Json(value),
                )
                    .into_response(),
                Err(e) => error(e),
            }
        }
        Err(e) => error(e),
    }
}
pub(super) async fn diagnosis<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(seq): Path<i64>,
    Json(request): Json<Diagnose>,
) -> Response {
    if let Err(e) = authority(&state, &user).await {
        return e;
    }
    let original = match original(&state, seq).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let old = match entries(&state).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Some(Entry::Diagnosis { receipt }) = old
        .iter()
        .find(|e| e.id() == request.request_id && e.original_seq() == seq)
    {
        return (StatusCode::OK, Json(receipt)).into_response();
    }
    let input = async {
        let m = &original.r#move;
        let run_id = boss_core::job::JobId::from_uuid(
            Uuid::parse_str(&m.packet).map_err(|_| "invalid run identity".to_string())?,
        );
        let parent_id = boss_core::job::JobId::from_uuid(
            Uuid::parse_str(
                m.handoff_from
                    .as_deref()
                    .ok_or("original has no predecessor")?,
            )
            .map_err(|_| "invalid predecessor identity".to_string())?,
        );
        let run = state
            .jobs
            .historical_events(&run_id, m.cause_seq, 4096)
            .await
            .map_err(|e| e.to_string())?;
        let parent = state
            .jobs
            .historical_events(&parent_id, m.cause_seq, 4096)
            .await
            .map_err(|e| e.to_string())?;
        let (run_job, parent_job) = match (
            move_diagnosis::state(&run, &m.packet, m.cause_seq),
            move_diagnosis::state(
                &parent,
                m.handoff_from.as_deref().unwrap_or_default(),
                m.cause_seq,
            ),
        ) {
            (Ok((run_job, _)), Ok((parent_job, _))) => (run_job, parent_job),
            _ => {
                // Incomplete state is a diagnosis input, not a transport failure.
                // Keep the exact bounded originals; the deterministic domain
                // reader records UNKNOWN before needing any pinned registries.
                return Ok(Inputs {
                    run,
                    parent,
                    workflows: Vec::new(),
                    stations: Vec::new(),
                    station_history: Vec::new(),
                });
            }
        };
        let registry = state
            .kind_registry
            .as_ref()
            .ok_or("workflow registry unavailable")?;
        // Only the original pinned protocols, not today's versions.
        let mut workflows = vec![
            registry
                .get_version(&run_job.kind, run_job.workflow_version)
                .await
                .map_err(|e| e.to_string())?,
            registry
                .get_version(&parent_job.kind, parent_job.workflow_version)
                .await
                .map_err(|e| e.to_string())?,
        ];
        workflows.sort_by(|a, b| (&a.kind, a.version).cmp(&(&b.kind, b.version)));
        let station_history = state
            .jobs
            .historical_station_events("loading-dock", m.cause_seq, 4096)
            .await
            .map_err(|e| e.to_string())?;
        let stations = crate::move_diagnosis::stations::at(&station_history, m.cause_seq)?;
        let inputs = Inputs {
            run,
            parent,
            workflows,
            stations,
            station_history,
        };
        Ok::<_, String>(inputs)
    }
    .await;
    let inputs = match input {
        Ok(v) => v,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
    };
    match serde_json::to_vec(&inputs) {
        Ok(bytes) if bytes.len() <= 2_000_000 => {}
        _ => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "native input byte bound exceeded",
            )
                .into_response();
        }
    }
    match move_diagnosis::diagnose(request.request_id, original, inputs) {
        Ok(receipt) => {
            save(
                &state,
                &user,
                Entry::Diagnosis {
                    receipt: Box::new(receipt),
                },
            )
            .await
        }
        Err(e) => error(e),
    }
}
pub(super) async fn correction<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(seq): Path<i64>,
    Json(request): Json<Correct>,
) -> Response {
    if let Err(e) = authority(&state, &user).await {
        return e;
    }
    let original = match original(&state, seq).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let old = match entries(&state).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !old.iter().any(|e|matches!(e,Entry::Diagnosis{receipt} if receipt.id==request.diagnosis_id && receipt.original.seq==seq)){return error("diagnosis missing or names another original");}
    let original_hash = match move_diagnosis::hash(&original) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    // A committed receipt is a fact even if unrelated packet annotations
    // changed after a lost acknowledgement. Never replace its copied proof.
    if let Some(entry) = old.iter().find(|e| e.id() == request.request_id) {
        return match entry {
            Entry::Correction {
                original_seq,
                diagnosis_id,
                original_hash: stored_hash,
                proof: Some(proof),
                ..
            } if *original_seq == seq
                && *diagnosis_id == request.diagnosis_id
                && *stored_hash == original_hash
                && request
                    .proof_request
                    .is_some_and(|id| boss_core::job::JobId::from_uuid(id) == proof.request.id) =>
            {
                (StatusCode::OK, Json(entry)).into_response()
            }
            _ => error("request identity already names another correction"),
        };
    }
    let proof = async {
        let id = request
            .proof_request
            .ok_or("independently recorded proof request required")?;
        let job_id = boss_core::job::JobId::from_uuid(id);
        let proof_request = state
            .jobs
            .get_job(&job_id)
            .await
            .map_err(|_| "proof request unreadable")?
            .ok_or("proof request missing")?;
        let steps = state
            .jobs
            .list_steps(&job_id)
            .await
            .map_err(|_| "proof execution unreadable")?;
        let execute = steps
            .into_iter()
            .find(|s| s.spec_slug.as_deref() == Some("execute"))
            .ok_or("proof execution missing")?;
        let artifact_id = proof_request
            .metadata
            .get("args")
            .and_then(|v| v.get(0))
            .and_then(serde_json::Value::as_str)
            .ok_or("proof artifact missing")?
            .parse::<Uuid>()
            .map_err(|_| "proof artifact invalid")?;
        let artifact_id = boss_core::job::JobId::from_uuid(artifact_id);
        let artifact = state
            .jobs
            .get_job(&artifact_id)
            .await
            .map_err(|_| "proof artifact unreadable")?
            .ok_or("proof artifact missing")?;
        let proven = state
            .jobs
            .list_steps(&artifact_id)
            .await
            .map_err(|_| "artifact proof unreadable")?
            .into_iter()
            .find(|s| s.spec_slug.as_deref() == Some("proven"))
            .ok_or("artifact proof missing")?;
        let registry = state
            .kind_registry
            .as_ref()
            .ok_or("proof protocol unavailable")?;
        let protocol = registry
            .get_version(&proof_request.kind, proof_request.workflow_version)
            .await
            .map_err(|_| "proof protocol unreadable")?;
        let original_id = boss_core::job::JobId::from_uuid(
            original
                .r#move
                .packet
                .parse()
                .map_err(|_| "original packet invalid")?,
        );
        let original_job = state
            .jobs
            .get_job(&original_id)
            .await
            .map_err(|_| "original partition unreadable")?
            .ok_or("original partition unavailable")?;
        if original_job.partition != proof_request.partition
            || original_job.partition != artifact.partition
        {
            return Err("proof crosses original partition");
        }
        Ok::<_, &str>(move_diagnosis::proof::MachineProof {
            request: proof_request,
            execute,
            protocol,
            artifact,
            proven,
        })
    }
    .await;
    let proof = match proof {
        Ok(p) => Some(Box::new(p)),
        Err(why) => return error(why),
    };
    save(
        &state,
        &user,
        Entry::Correction {
            id: request.request_id,
            original_seq: seq,
            diagnosis_id: request.diagnosis_id,
            original_hash,
            proof,
        },
    )
    .await
}
pub(super) async fn withdrawal<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(seq): Path<i64>,
    Json(request): Json<Withdraw>,
) -> Response {
    if let Err(e) = authority(&state, &user).await {
        return e;
    }
    save(
        &state,
        &user,
        Entry::Withdrawal {
            id: request.request_id,
            original_seq: seq,
            correction_id: request.correction_id,
        },
    )
    .await
}
