//! The live pools door (backlog f399458f, David 2026-11-08: "we should be
//! able to make these registry changes via a different protocol than the
//! code changes").
//!
//! Until this door a resource-pool row reached the registry only through
//! the platform seed on a converge, so a value change — `robots:opus` 4
//! to 6, item 89b17d21 — rode a code car, a gate and a train. The door is
//! the stations and cadence doors' shape over the existing registry
//! ([`crate::pools::PoolRegistry`]): append-only and versioned, every
//! write one fact in the same transaction, gated on the `pool` resource.
//!
//! - `GET  /api/pools` — the active rows, by name.
//! - `GET  /api/pools/{name}/versions` — the lineage, oldest first.
//! - `POST /api/pools/_validate` — the publish's lint without the write:
//!   always 200 with `{ok, problems}`.
//! - `POST /api/pools/{name}/publish` — body = the row in the bundle
//!   file's shape (`infra/platform/pools/<name>.toml`'s `[[pool]]` as
//!   JSON) at its DECLARED version, so the tree file and the live row can
//!   agree; 409 when that version is not above the lineage's newest (or
//!   the change would invalidate an outstanding allocation), 400 for a
//!   row the seed's loader would refuse, 422 + `{ok, problems}` for one
//!   the cross-registry lint refuses, 200 with the row written. With
//!   `?over=<version>` — the live version a signed approval's diff was
//!   taken against — the write is also held to that baseline INSIDE its
//!   transaction: a lineage whose newest is any other version is 409 and
//!   writes nothing (review R1 of car 4019ba69: the client's own read
//!   cannot hold the baseline still between its GET and its POST).
//! - `POST /api/pools/{name}/retire` — 200 with the row retired, 404 when
//!   nothing of the name is active (never a silent success), 409 while it
//!   holds an outstanding allocation, 422 when an active protocol's step
//!   still bills it.
//!
//! The lint is the seed's own, judged live: what a file publish would
//! check across the station and pool bundles, run here on the active
//! registries before and after the change, answering only what the change
//! breaks ([`crate::pools::registry_change_failures`]).
//!
//! Every handler asks policy BEFORE it touches the port, so an unwired
//! registry never answers a caller the policy would refuse.

use super::*;
use axum::extract::Path;

use crate::pools::{PoolError, PoolRegistry, PoolSpec};

#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
fn pools_or_503<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> Result<&Arc<dyn PoolRegistry>, Response> {
    state.pools.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "pool registry not configured",
        )
            .into_response()
    })
}

async fn pool_policy_check<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    control: Pair,
) -> Result<(), Response> {
    match state.policy.ask(user, control).await {
        Ok(Decision::Allow { .. }) => Ok(()),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err(e.into_response()),
    }
}

fn pool_err_response(err: PoolError) -> Response {
    match err {
        PoolError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        PoolError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        PoolError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

/// The lint body — `{ok, problems}`, shared by the dry run (200) and the
/// refused write (422), so an author sees one answer from both.
fn lint_body(problems: &[String]) -> serde_json::Value {
    serde_json::json!({"ok": problems.is_empty(), "problems": problems})
}

/// What a change is judged against: the active pools, and the active
/// station and protocol rows the seed's lints read. A port this wiring
/// does not hold contributes nothing; a port that fails to answer fails
/// the write, never a quieter lint.
async fn lint_context<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    reg: &Arc<dyn PoolRegistry>,
) -> Result<
    (
        Vec<PoolSpec>,
        Vec<crate::stations::StationSpec>,
        Vec<WorkflowSpec>,
    ),
    Response,
> {
    let pools = reg.list_active().await.map_err(pool_err_response)?;
    let stations = match &state.stations {
        Some(stations) => stations.list_active().await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the station rows the lint reads could not be read: {e}"),
            )
                .into_response()
        })?,
        None => Vec::new(),
    };
    let workflows = match &state.kind_registry {
        Some(kinds) => kinds.list_active(None).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the protocols the lint reads could not be read: {e}"),
            )
                .into_response()
        })?,
        None => Vec::new(),
    };
    Ok((pools, stations, workflows))
}

/// The active pools with `name` replaced by `with` (or removed, for a
/// retire).
fn changed(active: &[PoolSpec], name: &str, with: Option<&PoolSpec>) -> Vec<PoolSpec> {
    active
        .iter()
        .filter(|p| p.name != name)
        .chain(with)
        .cloned()
        .collect()
}

/// Every problem a publish of `given` would be refused for: the row's
/// own (as the seed's loader reads a file), then the cross-registry lint.
async fn publish_problems<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    reg: &Arc<dyn PoolRegistry>,
    given: &serde_json::Value,
) -> Result<Result<PoolSpec, Vec<String>>, Response> {
    let spec = match crate::pools::authored_row(given) {
        Ok(spec) => spec,
        Err(e) => return Ok(Err(vec![e.to_string()])),
    };
    let (active, stations, workflows) = lint_context(state, reg).await?;
    let failures = crate::pools::registry_change_failures(
        &active,
        &changed(&active, &spec.name, Some(&spec)),
        &stations,
        &workflows,
    );
    Ok(if failures.is_empty() {
        Ok(spec)
    } else {
        Err(failures)
    })
}

/// `GET /api/pools` — the active rows.
pub(super) async fn list_pools<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if let Err(r) = pool_policy_check(&state, &user, controls::READ_POOL).await {
        return r;
    }
    let reg = match pools_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    match reg.list_active().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => pool_err_response(e),
    }
}

/// `GET /api/pools/{name}/versions` — every version, oldest first,
/// retired ones included: what the pool used to be.
pub(super) async fn list_pool_versions<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    if let Err(r) = pool_policy_check(&state, &user, controls::READ_POOL).await {
        return r;
    }
    let reg = match pools_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    match reg.list_versions(&name).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => pool_err_response(e),
    }
}

/// `POST /api/pools/_validate` — the publish's lint, nothing written.
/// Always 200: a lint failure is data, and its body is the one a refused
/// publish answers with.
pub(super) async fn validate_pool<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(given): Json<serde_json::Value>,
) -> Response {
    // Gated like the publish — the dry run is an authoring affordance.
    if let Err(r) = pool_policy_check(&state, &user, controls::PUBLISH_POOL).await {
        return r;
    }
    let reg = match pools_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    match publish_problems(&state, reg, &given).await {
        Ok(Ok(_)) => Json(lint_body(&[])).into_response(),
        Ok(Err(problems)) => Json(lint_body(&problems)).into_response(),
        Err(r) => r,
    }
}

/// The publish's query: `over`, the live version a signed approval's
/// diff was rendered against, which the write is held to inside its
/// transaction ([`PoolRegistry::publish_over`]). Absent, the publish is
/// bound only by the lineage's version floor. A value that is not an
/// integer is axum's 400, before anything is read.
#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct PublishQuery {
    over: Option<i32>,
}

/// `POST /api/pools/{name}/publish` — land the row at its declared
/// version over the active one, through
/// [`PoolRegistry::publish_declared`] (or [`PoolRegistry::publish_over`]
/// when `?over=` names the signed baseline), whose transaction holds the
/// version bound, the signed baseline and the outstanding-allocation
/// fence.
pub(super) async fn publish_pool<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
    axum::extract::Query(query): axum::extract::Query<PublishQuery>,
    Json(given): Json<serde_json::Value>,
) -> Response {
    if let Err(r) = pool_policy_check(&state, &user, controls::PUBLISH_POOL).await {
        return r;
    }
    let reg = match pools_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    // The row's own refusal is a 400, before the lint: it is the answer
    // the seed's loader gives the same file.
    let spec = match crate::pools::authored_row(&given) {
        Ok(spec) => spec,
        Err(e) => return pool_err_response(e),
    };
    if spec.name != name {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "the body declares `{}` but the path names `{name}`",
                spec.name
            ),
        )
            .into_response();
    }
    match publish_problems(&state, reg, &given).await {
        Ok(Ok(_)) => {}
        Ok(Err(problems)) => {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(lint_body(&problems))).into_response();
        }
        Err(r) => return r,
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    let written = match query.over {
        Some(over) => reg.publish_over(spec, over, &actor, now).await,
        None => reg.publish_declared(spec, &actor, now).await,
    };
    match written {
        Ok(written) => Json(written).into_response(),
        Err(e) => pool_err_response(e),
    }
}

/// `POST /api/pools/{name}/retire` — take the active version out of
/// service. Refused (422) while an active protocol's step still bills the
/// pool: every claim of that step would then be refused.
pub(super) async fn retire_pool<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    if let Err(r) = pool_policy_check(&state, &user, controls::RETIRE_POOL).await {
        return r;
    }
    let reg = match pools_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let (active, stations, workflows) = match lint_context(&state, reg).await {
        Ok(context) => context,
        Err(r) => return r,
    };
    let problems = crate::pools::registry_change_failures(
        &active,
        &changed(&active, &name, None),
        &stations,
        &workflows,
    );
    if !problems.is_empty() {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(lint_body(&problems))).into_response();
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    match reg.retire(&name, &actor, now).await {
        Ok(Some(retired)) => Json(retired).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("no active pool named `{name}` — nothing retired"),
        )
            .into_response(),
        Err(e) => pool_err_response(e),
    }
}
