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
        // 422, not 400: the spec parsed and is well-formed JSON — it
        // just describes a queue that cannot behave as declared. Body
        // is the same `{ok, problems}` shape `_validate` returns, so
        // the editor renders a refused publish exactly like a failed
        // dry run.
        StationError::Unviable(problems) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(lint_result_json(&problems)),
        )
            .into_response(),
        StationError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

/// The lint result body — `{ok, problems}`. One definition shared by
/// the author-time dry run (200) and the publish refusal (422).
fn lint_result_json(problems: &[crate::station_lint::StationLintError]) -> serde_json::Value {
    serde_json::json!({
        "ok": problems.is_empty(),
        "problems": crate::station_lint::problems_json(problems),
    })
}

/// Station authoring is a network-configuration change, so it is
/// gated on the `workflow` resource — the same privilege that governs
/// the other registries a protocol is assembled from. A reader who
/// may see queues still cannot redraw them.
async fn station_policy_check<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    action: Action,
) -> Result<(), Response> {
    match state.policy.check(user, action, Resource::workflow()).await {
        Ok(Decision::Allow { .. }) => Ok(()),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("policy check failed: {e}"),
        )
            .into_response()),
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

    // Steps are fetched when the predicate reads step state, or when
    // the station's lens declares it needs them to draw the queue —
    // "where has this packet got to" is a fact about its steps, and a
    // surface without them can only render a list.
    let needs_steps =
        spec.predicate.needs_steps() || spec.lens.as_ref().is_some_and(|l| l.with_steps);
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

// ---------------------------------------------------------------------------
// Authoring — the runtime write path
// ---------------------------------------------------------------------------
//
// Stations are the substrate's routing table, and David's ratified
// answer (2026-08-13) was that they must be editable at run time:
// "stations need to be editable at run time. They should be data in a
// registry." The registry and the port already existed; without these
// routes the only way to redraw a queue was a SQL seed and a deploy,
// which is precisely the leak the three-layer reading calls out — a
// protocol that cannot be replaced without a deploy has leaked into
// the substrate.

/// `POST /api/stations` — append a draft version. Version numbering
/// is the registry's business (max+1); a draft is work in progress and
/// is deliberately NOT linted, matching the Workflow registry.
pub(super) async fn create_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(spec): Json<crate::stations::StationSpec>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, Action::Create).await {
        return r;
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    match reg.create_draft(spec, &actor, now).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `POST /api/stations/_validate` — author-time dry run. Lints a spec
/// WITHOUT persisting, calling the same `station_lint::gate_active`
/// the publish path enforces, so an editor showing "no problems"
/// publishes cleanly and a refused publish shows the same list.
///
/// Always 200: lint failures are data, not an HTTP error. The 422 on
/// publish and this 200 carry the same body.
pub(super) async fn validate_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(spec): Json<crate::stations::StationSpec>,
) -> Response {
    // Gated like create — the dry run is an authoring affordance.
    if let Err(r) = station_policy_check(&state, &user, Action::Create).await {
        return r;
    }
    let problems = match crate::station_lint::gate_active(&spec) {
        Ok(()) => Vec::new(),
        Err(p) => p,
    };
    (StatusCode::OK, Json(lint_result_json(&problems))).into_response()
}

/// `GET /api/stations/{name}/versions` — every version of one name,
/// oldest first, drafts and retired included. The audit view: what
/// this queue used to be, and what is staged to replace it.
pub(super) async fn list_station_versions<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.list_versions(&name).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `GET /api/stations/{name}/versions/{version}` — one historical row.
pub(super) async fn get_station_version<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path((name, version)): Path<(String, i32)>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.get_version(&name, version).await {
        Ok(row) => Json(row).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `POST /api/stations/{name}/publish` — promote the latest draft to
/// ACTIVE, retiring the incumbent.
///
/// The viability gate runs inside `StationRegistry::publish`, against
/// the draft row the transaction actually promotes — not against a
/// copy re-read here, which could race a concurrent author. An
/// unviable draft comes back as `StationError::Unviable` and leaves as
/// 422 + the problem list.
pub(super) async fn publish_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, Action::Update).await {
        return r;
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    match reg.publish(&name, &actor, now).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `POST /api/stations/{name}/retire` — close the station. Idempotent:
/// retiring an already-retired name is a 204 that records nothing.
pub(super) async fn retire_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, Action::Update).await {
        return r;
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    match reg.retire(&name, &actor, now).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => station_err_response(e),
    }
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
