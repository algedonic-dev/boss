//! Exact-diagnosis subresource. No caller-selected audit predicates or clearance.
use super::*;
use crate::{
    move_admission::{self, Observation},
    move_diagnosis::{self, Diagnosis, Entry},
};
use axum::extract::Path;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Observe {
    request_id: Uuid,
}
fn conflict(e: impl std::fmt::Display) -> Response {
    (StatusCode::CONFLICT, e.to_string()).into_response()
}
async fn bound<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    seq: i64,
    id: Uuid,
) -> Result<Diagnosis, Response> {
    let original = super::move_corrections::original(state, seq).await?;
    let entries = super::move_corrections::entries(state).await?;
    let d = entries
        .into_iter()
        .find_map(|e| match e {
            Entry::Diagnosis { receipt } if receipt.id == id => Some(*receipt),
            _ => None,
        })
        .ok_or_else(|| conflict("diagnosis missing"))?;
    if move_diagnosis::hash(&original).map_err(conflict)? != d.original_hash
        || move_diagnosis::hash(&d.original).map_err(conflict)? != d.original_hash
    {
        return Err(conflict("diagnosis names another original"));
    }
    move_admission::identities(&d).map_err(conflict)?;
    Ok(d)
}
fn same_binding(o: &Observation, d: &Diagnosis) -> bool {
    o.diagnosis.id == d.id && move_diagnosis::hash(d).ok() == Some(o.diagnosis_hash.clone())
}

pub(super) async fn observe<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path((seq, id)): Path<(i64, Uuid)>,
    Json(request): Json<Observe>,
) -> Response {
    if let Err(e) = super::move_corrections::authority(&state, &user).await {
        return e;
    }
    let d = match bound(&state, seq, id).await {
        Ok(d) => d,
        Err(e) => return e,
    };
    let Some(feed) = state.yard_moves.as_ref() else {
        return conflict("moves unavailable");
    };
    match feed.store.admission_observation(request.request_id).await {
        Ok(Some(o)) => {
            return if same_binding(&o, &d) {
                (StatusCode::OK, Json(o)).into_response()
            } else {
                conflict("observation request names another original or diagnosis")
            };
        }
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
        Ok(None) => {}
    }
    let entries = match super::move_corrections::entries(&state).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if entries.iter().any(|e| e.id() == request.request_id) {
        return conflict("request identity already names a correction ledger entry");
    }
    let run = match d.original.r#move.packet.parse::<Uuid>() {
        Ok(id) => boss_core::job::JobId::from_uuid(id),
        Err(e) => return conflict(e),
    };
    let job = match state.jobs.get_job(&run).await {
        Ok(Some(j)) => j,
        _ => return conflict("original packet partition unavailable"),
    };
    let snapshot = match state.jobs.move_admission_snapshot(&d).await {
        Ok(s) => s,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    };
    let o = match move_admission::observe(request.request_id, d, snapshot, job.partition) {
        Ok(o) => o,
        Err(e) => return conflict(e),
    };
    let Some(actor) = user.ambient_actor() else {
        return conflict("named actor unavailable");
    };
    let e=state.publisher.stamp_with_actor(actor).await.with_partition(job.partition).event(move_admission::EVENT_KIND,
        serde_json::json!({"job_id":o.diagnosis.original.r#move.packet,"observation":o,"_actor":user.id}));
    match feed.store.append_admission_observation(&o, &e).await {
        Ok(true) => (StatusCode::CREATED, Json(o)).into_response(),
        Ok(false) => match feed.store.admission_observation(o.id).await {
            Ok(Some(stored)) if same_binding(&stored, &o.diagnosis) => {
                (StatusCode::OK, Json(stored)).into_response()
            }
            _ => conflict("observation readback unavailable"),
        },
        Err(e) => conflict(e),
    }
}
pub(super) async fn get<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    path: Result<Path<(i64, Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
) -> Response {
    // ef8f9488 Q2: reading an immutable receipt does not authorize its creation
    // or a correction. Reuse the whole-map read scope; a narrowed scope cannot
    // expose the linked packets conserved inside this evidence.
    if user.id.trim().is_empty() || user.ambient_actor().is_none() {
        return (
            StatusCode::FORBIDDEN,
            "a named actor must read exact evidence",
        )
            .into_response();
    }
    if let Some(refusal) = super::moves::refused(&state, &user).await {
        return refusal;
    }
    // A malformed identifier must not bypass the same authority as a valid read.
    let Path((seq, id, request)) = match path {
        Ok(path) => path,
        Err(e) => return e.into_response(),
    };
    let d = match bound(&state, seq, id).await {
        Ok(d) => d,
        Err(e) => return e,
    };
    let Some(feed) = state.yard_moves.as_ref() else {
        return conflict("moves unavailable");
    };
    match feed.store.admission_observation(request).await {
        Ok(Some(o)) if same_binding(&o, &d) => (StatusCode::OK, Json(o)).into_response(),
        Ok(Some(_)) => conflict("observation names another original or diagnosis"),
        Ok(None) => (StatusCode::NOT_FOUND, "observation missing").into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    }
}
