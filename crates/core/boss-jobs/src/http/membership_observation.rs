//! One fixed, scoped observation of the native starting-state query.
use super::*;
use crate::membership_observation::{MAX_REPORT_BYTES, Observation};
use axum::extract::{RawQuery, Request};

/// A fixed small read of this signed reader's actual updater receipts.
pub(super) async fn refresh<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    RawQuery(query): RawQuery,
    request: Request,
) -> Response {
    if let Some(refused) =
        jobs::estate_read_refusal(&state, &user, "membership refresh receipts").await
    {
        return refused;
    }
    let predicate = match state.policy.scope_of(&user, controls::READ_JOB).await {
        Ok(predicate) => predicate,
        Err(error) => return error.into_response(),
    };
    let scope = JobScope::from_predicate(&user, &predicate);
    if matches!(scope, JobScope::None) || user.ambient_actor().is_none() {
        return (
            StatusCode::FORBIDDEN,
            "membership refresh receipts require an identified readable job scope",
        )
            .into_response();
    }
    let empty_body = matches!(tokio::time::timeout(
        std::time::Duration::from_secs(1),
        axum::body::to_bytes(request.into_body(), 1),
    ).await, Ok(Ok(bytes)) if bytes.is_empty());
    if query.is_some_and(|query| !query.is_empty()) || !empty_body {
        return (
            StatusCode::BAD_REQUEST,
            "membership refresh receipts accept no query arguments or body",
        )
            .into_response();
    }
    let Some(service) = &state.membership_projection else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "membership projection service unavailable",
        )
            .into_response();
    };
    let identity = crate::membership_projection::ProjectionIdentity::new(Some(user.id), scope);
    match service.observe_refresh(&identity).await {
        Ok(observation) => Json(observation).into_response(),
        Err(why) => (StatusCode::SERVICE_UNAVAILABLE, why).into_response(),
    }
}

pub(super) async fn observe<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    RawQuery(query): RawQuery,
    request: Request,
) -> Response {
    if let Some(refused) =
        jobs::estate_read_refusal(&state, &user, "the membership query observation").await
    {
        return refused;
    }
    let predicate = match state.policy.scope_of(&user, controls::READ_JOB).await {
        Ok(predicate) => predicate,
        Err(error) => return error.into_response(),
    };
    let scope = JobScope::from_predicate(&user, &predicate);
    if matches!(scope, JobScope::None) || user.ambient_actor().is_none() {
        return (
            StatusCode::FORBIDDEN,
            "membership observation requires an identified readable job scope",
        )
            .into_response();
    }
    // Read at most one byte: even an oversized or streaming body cannot select work.
    let empty_body = match tokio::time::timeout(
        std::time::Duration::from_secs(1),
        axum::body::to_bytes(request.into_body(), 1),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes.is_empty(),
        Ok(Err(_)) | Err(_) => false,
    };
    if query.is_some_and(|query| !query.is_empty()) || !empty_body {
        return (
            StatusCode::BAD_REQUEST,
            "membership observation accepts no query arguments or body",
        )
            .into_response();
    }
    // This diagnostic observes the real audit log, independently of tenant time.
    let observation_clock = boss_clock_client::WallClockClient;
    let since = boss_clock_client::ClockClient::now(&observation_clock)
        .await
        .now
        - chrono::Duration::hours(24);
    let jobs = state.jobs.clone();
    let actor = user.id;
    let result = state
        .membership_observation_owner
        .run(move |cancelled| async move {
            jobs.membership_starting_state_observation(
                since,
                Some(actor),
                &scope,
                crate::membership_projection_service::SOURCE_ROW_LIMIT,
                cancelled,
            )
            .await
            .map_err(|error| error.to_string())
        })
        .await;
    let (status, report) = match result {
        Ok(report) => (StatusCode::OK, report),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Observation {
                state: "unread".into(),
                why: Some(error),
                ..Observation::default()
            },
        ),
    };
    match serde_json::to_vec(&report) {
        Ok(bytes) if bytes.len() <= MAX_REPORT_BYTES => (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            bytes,
        )
            .into_response(),
        Ok(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "membership observation refused: complete report exceeds its byte bound",
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("membership observation serialization failed: {error}"),
        )
            .into_response(),
    }
}
