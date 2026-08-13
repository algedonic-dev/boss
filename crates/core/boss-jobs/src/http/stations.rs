//! Station read surfaces — the registry rows and the evaluated
//! queues (docs/design/stations.md).
//!
//! Reads pass through the SAME CurrentUser/policy path as the job
//! lists: the caller's read-scope Predicate on the `job` resource is
//! computed once and pushed into the packet query, so a station
//! queue can never show a caller a packet /api/jobs would hide. A
//! denied caller gets a clean empty collection, matching list_jobs.

use super::*;

use axum::extract::Path;

use crate::station_queue::evaluate_station;
use crate::stations::{StationError, StationRegistry};

#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
pub(super) fn stations_or_503<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> Result<&Arc<dyn StationRegistry>, Response> {
    state.stations.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "station registry not configured",
        )
            .into_response()
    })
}

fn station_err_response(err: StationError) -> Response {
    match err {
        StationError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        StationError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        StationError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        StationError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

/// `GET /api/stations` — every active station row. The registry rows
/// themselves carry no packet data; the policy gate mirrors the job
/// list's posture (scope predicate on the `job` resource; a caller
/// who can see no packets sees no queues either).
pub(super) async fn list_stations<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
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
    if matches!(predicate, boss_policy_client::Predicate::None) {
        return Json(serde_json::json!({ "data": [], "total": 0 })).into_response();
    }
    match reg.list_active().await {
        Ok(rows) => {
            let total = rows.len();
            Json(serde_json::json!({ "data": rows, "total": total })).into_response()
        }
        Err(e) => station_err_response(e),
    }
}

/// `GET /api/stations/{name}/queue` — the station's evaluated,
/// ordered queue: derived membership (the predicate, bound to the
/// caller, over their policy-scoped packets), data-declared
/// discipline, and the advisory `over_limit` verdict in the envelope.
pub(super) async fn station_queue<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let row = match reg.get_active(&name).await {
        Ok(s) => s,
        Err(e) => return station_err_response(e),
    };
    let today = boss_clock_client::now_from(&state.clock).await.date_naive();

    // Bind the self placeholder ONCE, here, before any packet is
    // compared — a per-actor station is one registry row whose queue
    // depends on who is asking. A caller with no identity (guest) gets
    // the station's own empty queue: the envelope still describes the
    // station truthfully, it just holds nothing.
    let Some(spec) = row.bind_self(self_id(&user)) else {
        return Json(evaluate_station(&row, Vec::new(), today)).into_response();
    };

    // One policy path with /api/jobs: scope predicate → JobScope,
    // pushed into the adapter query.
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
    let scope = job_scope_from_predicate(&user, &predicate);

    // The evaluation universe: in-flight packets, because stations
    // hold in-flight traffic. A station declaring a terminal window
    // also wants recently-departed packets, so its status filter opens
    // up and the window narrows it back down in `evaluate_station` —
    // the pure half, where the rule is testable without a database.
    //
    // Kind and the bound `metadata_equals` push down into SQL so the
    // MAX_LIMIT page is drawn from the packets that can actually be
    // members. Without the metadata push-down, a per-actor station on
    // a busy install would page through the newest 1000 packets of the
    // whole company and find few of the caller's own.
    let filter = JobFilter {
        kind: spec.predicate.kind.clone(),
        status: spec
            .terminal_window_days
            .is_none()
            .then_some(JobStatus::Open),
        metadata_contains: metadata_containment(&spec.predicate),
        scope,
        ..Default::default()
    };
    let (jobs, _total) = match state.jobs.list_jobs(&filter, MAX_LIMIT, 0).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Steps are fetched only when the predicate reads step state.
    let needs_steps = spec.predicate.needs_steps();
    let mut packets = Vec::with_capacity(jobs.len());
    for job in jobs {
        let steps = if needs_steps {
            state.jobs.list_steps(&job.id).await.unwrap_or_default()
        } else {
            Vec::new()
        };
        packets.push((job, steps));
    }

    Json(evaluate_station(&spec, packets, today)).into_response()
}

/// The `metadata_equals` clause of an already-BOUND predicate as a
/// containment document the adapter can push into SQL. `None` when the
/// predicate declares none.
///
/// Only ever built from a bound predicate: pushing an unbound `"@me"`
/// down would ask the database for packets that literally wrote the
/// placeholder.
fn metadata_containment(
    predicate: &crate::station_queue::StationPredicate,
) -> Option<serde_json::Value> {
    if predicate.metadata_equals.is_empty() {
        return None;
    }
    Some(serde_json::Value::Object(
        predicate
            .metadata_equals
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    ))
}
