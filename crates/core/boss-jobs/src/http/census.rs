//! `POST /api/network/census` — where the packet-loss census lands.
//!
//! The census (docs/design/packet-loss.md, decided in review
//! `9fb9904f`) is computed by the dispatcher's `network.census`
//! handler over this API's own read surfaces — /api/jobs totals,
//! /api/stations, each station's queue. Q3's ruling is that the
//! counts become a MEASURED SERIES on the log, one event per firing,
//! so loss is a queryable trend rather than a spot check. Dispatcher
//! handlers own no database — they speak HTTP to the public surface
//! like any caller — so the jobs service, which owns the network
//! substrate the census measures, provides the one write: accept the
//! counts, stamp the actor, record exactly one `jobs.network.census`
//! event through the repository's standalone-event path
//! (`record_events` — the same reliable-delivery outbox path the
//! post-materialization `step.ready.<kind>` markers use).
//!
//! DELIBERATELY A DUMB DOOR. The endpoint validates shape (a JSON
//! object) and trust (operator tier), and records what it was
//! handed. It does NOT recompute or cross-check the counts: the
//! census's honesty lives in the handler that measured, and a door
//! that second-guesses its instrument is a second instrument. The
//! payload field list is owned by the handler
//! (boss-dispatcher-handlers `network_census`), same as every other
//! marker's payload is owned by its emitter.
//!
//! OPERATOR TIER ONLY, mirroring the cadence surface's posture:
//! the census is operator machinery. The dispatcher stamps
//! `access_tier: operator` on every rule-as-actor header, and the
//! gateway injects `x-boss-user` for external callers, so an
//! unauthenticated hit parses as guest and is refused. Writes also
//! pass the machine gate when a token is configured, like every
//! other state-changing route on this router.

use super::*;

use boss_policy_client::AccessTier;

pub(super) async fn record_network_census<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(counts): Json<serde_json::Value>,
) -> Response {
    if user.access_tier != AccessTier::Operator {
        return (
            StatusCode::FORBIDDEN,
            "the census door is operator machinery — operator tier required",
        )
            .into_response();
    }
    if !counts.is_object() {
        return (
            StatusCode::BAD_REQUEST,
            "census counts must be a JSON object",
        )
            .into_response();
    }

    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    // The census event stamps wall-clock time like every record (sim
    // time is retired from the record); the counts payload is what
    // carries the measured state.
    let event = events::network_census_event(&actor, counts);

    match state.jobs.record_events(std::slice::from_ref(&event)).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "recorded": true,
                "kind": events::NETWORK_CENSUS,
            })),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
