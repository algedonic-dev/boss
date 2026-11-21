//! The door the clock rule calls to expire lapsed resource leases (backlog
//! 3aeaf08c; design a236103a D4).
//!
//! WHY A DOOR AND NOT A LOOP IN THIS SERVICE. Car 2b (d37e17b7, #1524)
//! landed the generation-fenced sweep with no caller at all, so a lease
//! past its `lease_until` only ever fenced its holder's completion: its
//! FLOW capacity stayed charged and its STOCK never became the obligation
//! a reclaimer reads. The trigger is a timer, and timers are dispatcher
//! rules (`infra/dispatcher/rules/`), so a missed firing is on the
//! dispatcher's firing record where the silence sweep and the dead-letter
//! count read it, not in a task inside the API that nothing watches.
//!
//! WHAT THE CALLER CHOOSES, AND WHAT IT DOES NOT. The instant is the
//! server's own clock, read when the stamp is made — the same clock the
//! claim door stamped `lease_until` from — and never a caller's value, so
//! no caller can expire a lease early. The only input is the batch size,
//! bounded by `MAX_SWEEP` before the adapter is asked. Which rows lapse is
//! the adapter's: active claims whose `lease_until` has passed, each
//! re-read under its step's row lock, so a fresher generation (a re-claim
//! with a later lease) is never swept and a firing that races another
//! releases nothing twice.
//!
//! WHO MAY. The sweep reads every step's claim and writes on every packet
//! whose lease lapsed, with no row predicate a narrower grant could match,
//! so it takes `read` AND `update` on `step` at scope `all` — the
//! platform-admin's in core defaults, which is the role the dispatcher's
//! rule actor presents — and an identified actor, whom each expiry fact
//! names. The machine gate the binary mounts over every route stands in
//! front of this one as it does every other.
//!
//! WHOSE PARTITION. One stamp is made here, for the caller, and it signs
//! every expiry; it does NOT decide the partition. Each expiry is a fact
//! about its own packet, so the adapter records it under the partition
//! that packet was admitted into, read under the step's row lock — a
//! real rule sweeping a simulated packet records a simulated expiry
//! (Codex review e7a414ae, R1 P2).

use super::*;
use boss_policy_client::Scope;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SweepQuery {
    limit: Option<usize>,
}

/// What one firing did. `saturated` is `expired == limit`: the batch was
/// full, so more lapsed leases may remain and the caller fires again.
#[derive(Serialize)]
struct SweepReport {
    observed_at: chrono::DateTime<chrono::Utc>,
    expired: usize,
    limit: usize,
    saturated: bool,
}

pub(super) async fn sweep<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(query): axum::extract::Query<SweepQuery>,
) -> Response {
    for control in [controls::READ_STEP, controls::UPDATE_STEP] {
        match state.policy.ask(&user, control).await {
            Ok(Decision::Allow { scope: Scope::All }) => {}
            Ok(Decision::Allow { .. }) => {
                return (
                    StatusCode::FORBIDDEN,
                    "the lease sweep acts on every packet: it requires read and update on \
                     step at scope all",
                )
                    .into_response();
            }
            Ok(Decision::Deny { reason }) => {
                return (StatusCode::FORBIDDEN, reason).into_response();
            }
            Err(e) => return e.into_response(),
        }
    }
    let Some(actor) = user.ambient_actor() else {
        return (
            StatusCode::FORBIDDEN,
            "the lease sweep requires an identified actor",
        )
            .into_response();
    };
    let limit = query.limit.unwrap_or(crate::resource_leases::MAX_SWEEP);
    if limit == 0 || limit > crate::resource_leases::MAX_SWEEP {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "limit must be 1..={}: one firing expires at most that many leases",
                crate::resource_leases::MAX_SWEEP
            ),
        )
            .into_response();
    }
    let stamp = state.publisher.stamp_with_actor(actor).await;
    match state.jobs.sweep_resource_leases_at(&stamp, limit).await {
        Ok(result) => Json(SweepReport {
            observed_at: result.observed_at,
            expired: result.expired,
            limit,
            saturated: result.expired >= limit,
        })
        .into_response(),
        Err(crate::port::JobsError::ResourceRefused(refusal)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": refusal.reason })),
        )
            .into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    }
}
