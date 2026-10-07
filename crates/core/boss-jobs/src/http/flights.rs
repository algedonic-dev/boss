//! `GET /api/flights/mine` — the codes of the flights that are on for
//! the caller (design c4c2a607, backlog 73c31776 car 1). The rule is
//! `crate::flights`; this handler owns the reads.
//!
//! One generic read: every OPEN packet carrying a `flight` block,
//! whatever its kind, each judged against the workflow version it is
//! pinned to. The answer is `{"flights": [codes]}` and nothing else —
//! a viewer never learns another flight's audience, or that a flight
//! it is not in exists.
//!
//! NOT SCOPED BY THE CALLER'S JOB-READ POLICY, on purpose. The answer
//! is about the caller alone: a user-tier session that may not read the
//! IT department's packets must still see the change it is in the
//! audience for, and the answer discloses nothing a packet read would.
//!
//! FAILING SAFE. A code this read does not list is off. A packet whose
//! steps or pinned protocol cannot be read is left out — off — and
//! said in the journal; a deployment with no workflow registry answers
//! 503, which every reader treats as off. None of those is a guess that
//! something is on.

use std::collections::BTreeMap;

use super::*;
use crate::flights::{self, Declaration, FlightState};

pub(super) async fn flights_mine<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let Some(registry) = state.kind_registry.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "workflow registry not configured — no flight state can be read, so none is on",
        )
            .into_response();
    };
    let filter = JobFilter {
        status: Some(JobStatus::Open),
        metadata_has: Some(flights::FLIGHT_KEY.to_string()),
        ..Default::default()
    };
    // Paged to `total`: a truncated page answers a smaller question,
    // and a flight on the unread tail would read as off. The shared
    // loop since backlog f71d1e81, which this read had written for
    // itself.
    let packets: Vec<Job> =
        match crate::list_every::list_every(state.jobs.as_ref(), &filter, MAX_LIMIT).await {
            Ok(rows) => rows,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };

    let mut specs: BTreeMap<(String, i32), WorkflowSpec> = BTreeMap::new();
    let mut judged: Vec<(Declaration, FlightState)> = Vec::new();
    for job in &packets {
        let Some(decl) = flights::declaration(&job.metadata) else {
            continue;
        };
        let key = (job.kind.clone(), job.workflow_version);
        if !specs.contains_key(&key) {
            match registry.get_version(&job.kind, job.workflow_version).await {
                Ok(spec) => {
                    specs.insert(key.clone(), spec);
                }
                Err(e) => {
                    tracing::warn!(job = %job.id, kind = %job.kind, version = job.workflow_version, "flights: the pinned protocol could not be read, so flight `{}` is off: {e}", decl.code);
                    continue;
                }
            }
        }
        let steps = match state.jobs.list_steps(&job.id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(job = %job.id, "flights: the steps could not be read, so flight `{}` is off: {e}", decl.code);
                continue;
            }
        };
        if let Some(spec) = specs.get(&key) {
            judged.push((decl, flights::state_of(&steps, spec)));
        }
    }
    let codes = flights::codes_on_for(&judged, &user.id, &user.role);
    if let Some(report) = &state.role_guards {
        // Keep every declaration of a code together: ambiguity is off for
        // both viewers, exactly as in the original codes-only response.
        // These are already-read declarations and pinned step judgements;
        // comparing a role performs no additional registry or packet IO.
        let mut by_code: BTreeMap<&str, Vec<(Declaration, FlightState)>> = BTreeMap::new();
        for (declaration, flight_state) in &judged {
            by_code
                .entry(&declaration.code)
                .or_default()
                .push((declaration.clone(), *flight_state));
        }
        for (code, declarations) in by_code {
            report.observe_captured(
                "flight-code-visibility",
                "visibility",
                &user,
                codes.iter().any(|visible| visible == code),
                |candidate| {
                    Some(
                        flights::codes_on_for(&declarations, &candidate.id, &candidate.role)
                            .iter()
                            .any(|visible| visible == code),
                    )
                },
            );
        }
    }
    Json(serde_json::json!({ "flights": codes })).into_response()
}
