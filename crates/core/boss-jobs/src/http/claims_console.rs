//! The factory-wide aggregate is available only to the existing full job-read
//! scope. A narrower packet scope cannot authorize global capacity/holder data.
use super::*;

#[derive(Deserialize, Default)]
pub(super) struct WaitingQuery {
    pool: Option<String>,
}

/// Where a step refused on `pool` now stands in that pool's line — the
/// `queued` half of a capacity refusal's answer (backlog 587f395a). A
/// line that cannot be read is said, never left out.
pub(super) async fn queued_for<R: JobsRepository + 'static>(
    jobs: &R,
    pool: &str,
    step: &boss_core::job::StepId,
) -> serde_json::Value {
    let line = jobs
        .resource_waiting_rows(pool)
        .await
        .map(crate::flow_resources::line)
        .map_err(|error| error.to_string());
    crate::flow_resources::queued_answer(pool, step, line)
}

/// `GET /api/claims/waiting?pool=<name>`: the pool's waiting line, oldest
/// wait first — the ready steps a capacity refusal left queued, each with
/// its position and the `waiting_on` it holds (backlog 587f395a). The
/// re-offer rule reads its head here when the pool frees. The same full
/// job-read scope as the claims console: the line names packets
/// factory-wide.
pub(super) async fn waiting<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(query): axum::extract::Query<WaitingQuery>,
) -> Response {
    // Policy first: a caller with no identity is refused by policy, never
    // answered 400 by the parameter check below it (gate 41fa10c9, the
    // every-jobs-read pin).
    let predicate = match state.policy.scope_of(&user, controls::READ_JOB).await {
        Ok(predicate) => predicate,
        Err(error) => return error.into_response(),
    };
    if JobScope::from_predicate(&user, &predicate) != JobScope::All {
        return (
            StatusCode::FORBIDDEN,
            "a pool's waiting line requires the existing full job-read scope",
        )
            .into_response();
    }
    let Some(pool) = query.pool.filter(|p| !p.trim().is_empty()) else {
        return (StatusCode::BAD_REQUEST, "name the pool: ?pool=<name>").into_response();
    };
    match state.jobs.resource_waiting_rows(&pool).await {
        Ok(rows) => Json(serde_json::json!({
            "pool": pool,
            "line": crate::flow_resources::line(rows),
        }))
        .into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("the waiting line for {pool} could not be read: {error}"),
        )
            .into_response(),
    }
}

#[derive(Deserialize, Default)]
pub(super) struct Query {
    /// The retired time-travel selector, read only so it can be refused:
    /// axum ignores a key no field names, and `?as_of=N` would then answer
    /// the head as if it were the past (backlog 4bec011d). A string, so
    /// any value — a number, a word, nothing — meets the same refusal.
    as_of: Option<String>,
    window: Option<String>,
    /// The station, by `id` or name, whose subtree's claims to answer
    /// (backlog d05e29a7). Absent: every claim, as before.
    station: Option<String>,
    /// The most claim rows to answer; `truncated` says when it bit.
    limit: Option<usize>,
}

/// The claims read as `GET /api/claims` answers it: the reading, plus what
/// the request narrowed it to (backlog d05e29a7).
#[derive(serde::Serialize)]
struct Narrowed {
    #[serde(flatten)]
    reading: crate::claims_console::ClaimsReading,
    /// The station whose subtree `claims` holds; null when it holds every
    /// claim (no station named, or the root, whose subtree is the company).
    within: Option<String>,
    /// `limit` left claims out: a sum over this list is short.
    truncated: bool,
    /// Counts over every open claim in this scope, before the row limit.
    root_fallback_claims: usize,
    station_resolution_unrecorded_claims: usize,
}

/// The names and ids a claim's recorded station may match to stand in the
/// subtree under `station`, with the subtree's top name; `None` for the
/// root, which holds every claim. 404 for a station no tree row is, a
/// named 503 when no registry is wired — never the whole read passed off
/// as the subtree's.
async fn subtree_of<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    station: &str,
) -> Result<Option<(String, std::collections::BTreeSet<String>)>, Response> {
    let registry = super::stations::stations_or_503(state)?;
    let rows = registry.list_active().await.map_err(|error| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("the station registry could not be read: {error}"),
        )
            .into_response()
    })?;
    let nodes = crate::station_tree::subtree(&rows, station)
        .map_err(|why| (StatusCode::NOT_FOUND, why).into_response())?;
    let Some(top) = nodes.first() else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no subtree under `{station}`"),
        )
            .into_response());
    };
    if top.station.parent.is_none() {
        return Ok(None);
    }
    let names = nodes
        .iter()
        .flat_map(|n| std::iter::once(n.station.name.clone()).chain(n.station.id.clone()))
        .collect();
    Ok(Some((top.station.name.clone(), names)))
}

/// Keep the claims whose recorded station stands in `within` — and those
/// that record none, which could stand anywhere: every reader says so
/// ("busy not recorded", "unplaced") rather than count them as zero, so
/// dropping them here would turn that sentence into a smaller figure.
/// Then bound the rows to `limit`, in the reading's own order.
fn narrow(
    mut reading: crate::claims_console::ClaimsReading,
    within: Option<(String, std::collections::BTreeSet<String>)>,
    limit: Option<usize>,
) -> Narrowed {
    let within = within.map(|(top, names)| {
        reading
            .claims
            .retain(|c| c.station.as_ref().is_none_or(|s| names.contains(s)));
        top
    });
    let truncated = limit.is_some_and(|limit| reading.claims.len() > limit);
    let root_fallback_claims = reading
        .claims
        .iter()
        .filter(|claim| claim.station_resolution.as_deref() == Some("root-fallback"))
        .count();
    let station_resolution_unrecorded_claims = reading
        .claims
        .iter()
        .filter(|claim| claim.station_resolution.is_none())
        .count();
    if let Some(limit) = limit {
        reading.claims.truncate(limit);
    }
    Narrowed {
        reading,
        within,
        truncated,
        root_fallback_claims,
        station_resolution_unrecorded_claims,
    }
}

/// The gate the claims read stands behind: the existing full job-read
/// scope, and a wired materialized reader. A narrower scope is 403, an
/// unwired reader is named 503, never a missing route.
async fn scoped_history<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
) -> Result<Arc<dyn crate::claims_console::ClaimsHistory>, Response> {
    let predicate = state
        .policy
        .scope_of(user, controls::READ_JOB)
        .await
        .map_err(|error| error.into_response())?;
    if JobScope::from_predicate(user, &predicate) != JobScope::All {
        return Err((
            StatusCode::FORBIDDEN,
            "claims console requires the existing full job-read scope",
        )
            .into_response());
    }
    state.claims_history.clone().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "claims history unavailable: materialized reader not configured",
        )
            .into_response()
    })
}

/// The claims read's answer shape: the 2 s deadline, Invalid 400,
/// OutsideRetention 410, everything else a named 503.
async fn answer<T: serde::Serialize>(
    read: impl std::future::Future<Output = Result<T, crate::claims_console::ClaimsError>>,
    trace: crate::claims_console::ReadTrace,
) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(2), read).await {
        Ok(Ok(reading)) => Json(reading).into_response(),
        Ok(Err(error @ crate::claims_console::ClaimsError::Invalid(_))) => {
            (StatusCode::BAD_REQUEST, error.to_string()).into_response()
        }
        Ok(Err(error @ crate::claims_console::ClaimsError::OutsideRetention(_))) => {
            (StatusCode::GONE, error.to_string()).into_response()
        }
        Ok(Err(error)) => (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "claims history unavailable: outer deadline exhausted; {}",
                trace.describe()
            ),
        )
            .into_response(),
    }
}

pub(super) async fn read<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(query): axum::extract::Query<Query>,
) -> Response {
    let history = match scoped_history(&state, &user).await {
        Ok(history) => history,
        Err(refused) => return refused,
    };
    // After policy, like every jobs read: an unscoped caller learns
    // nothing about the parameters (the every-jobs-read pin).
    if query.as_of.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            "as_of is retired: the claims console's time travel went with its \
             scrubber (backlog 4bec011d); GET /api/claims answers the head only",
        )
            .into_response();
    }
    let hours = match crate::regions::parse_window(query.window.as_deref()) {
        Ok(hours) => hours,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    // The subtree is resolved before the history is read, so a station no
    // tree row is costs no claims read (backlog d05e29a7).
    let within = match query.station.as_deref() {
        Some(station) => match subtree_of(&state, station).await {
            Ok(within) => within,
            Err(refused) => return refused,
        },
        None => None,
    };
    // A request-owned trace, so a deadline names the statement it
    // interrupted (backlog 81f8e468).
    let trace = crate::claims_console::ReadTrace::default();
    let observer = trace.clone();
    answer(
        async {
            observer.start("clock");
            let now = state.claims_history_clock.now().await.now;
            observer.finish();
            history
                .read_traced(None, hours, now, observer)
                .await
                .map(|reading| narrow(reading, within, query.limit))
        },
        trace,
    )
    .await
}
