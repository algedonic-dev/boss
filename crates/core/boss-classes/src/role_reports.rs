//! First report-only mount of the server role resolver (ddf0773e car 2).
//! Policy and registry services are unchanged: their readers cannot
//! re-enter this classes-only decorator. No asserted authority changes.
use std::sync::Arc;

use axum::http::StatusCode;
use axum::{
    Json, Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
};
use boss_core::role_of_record::RoleOfRecord;
use boss_policy_client::role_reporting::{ReportModeSource, ReportTally, ReportingPolicyClient};
use boss_policy_client::{CurrentUser, Decision, PolicyClient, Scope, controls};

use crate::http::{ClassesApiState, router};

/// The complete, expiring registry projection used by estate-wide reporting.
/// The inventory names unloaded snapshots and uses the original authorizer.
pub fn mount_snapshot(
    state: ClassesApiState,
    roles: Arc<boss_policy_client::role_reader::SnapshotRoleReader>,
    mode: Arc<dyn ReportModeSource>,
    tally: Arc<ReportTally>,
) -> Router {
    let inventory = boss_policy_client::role_inventory::router(
        "classes",
        "/api/classes/actor-role-reports",
        state.policy.clone(),
        roles.clone(),
        mode.clone(),
        tally.clone(),
    );
    let policy = Arc::new(ReportingPolicyClient::with_mode_source(
        state.policy,
        roles,
        tally,
        mode,
    ));
    router(ClassesApiState {
        classes: state.classes,
        policy,
    })
    .merge(inventory)
}

#[derive(Clone)]
struct ReportReader {
    policy: Arc<dyn PolicyClient>,
    tally: Arc<ReportTally>,
}

pub fn mount(
    state: ClassesApiState,
    roles: Arc<dyn RoleOfRecord>,
    mode: Arc<dyn ReportModeSource>,
    tally: Arc<ReportTally>,
) -> Router {
    // Read the tally through the existing Read policy-rule/all door,
    // using the original client so observing the observer records no
    // comparison and triggers no role-registry reads.
    let reader = ReportReader {
        policy: state.policy.clone(),
        tally: tally.clone(),
    };
    let policy = Arc::new(ReportingPolicyClient::with_mode_source(
        state.policy,
        roles,
        tally,
        mode,
    ));
    router(ClassesApiState {
        classes: state.classes,
        policy,
    })
    .merge(
        Router::new()
            .route("/api/classes/actor-role-reports", get(reports))
            .with_state(reader),
    )
}

async fn reports(State(state): State<ReportReader>, CurrentUser(user): CurrentUser) -> Response {
    match state.policy.ask(&user, controls::READ_POLICY_RULE).await {
        Ok(Decision::Allow { scope: Scope::All }) => Json(state.tally.snapshot()).into_response(),
        Ok(_) => (
            StatusCode::FORBIDDEN,
            "requires read policy-rule at scope all",
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}
