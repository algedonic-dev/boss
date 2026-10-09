//! A service's process-local report inventory, authorized through its
//! undecorated policy client to avoid observing the observation reader.
use crate::role_reader::SnapshotRoleReader;
use crate::role_reporting::{ReportModeSource, ReportTally};
use crate::{CurrentUser, Decision, PolicyClient, Scope, controls};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use std::sync::Arc;

struct Inventory {
    service: &'static str,
    authorizer: Arc<dyn PolicyClient>,
    roles: Arc<SnapshotRoleReader>,
    mode: Arc<dyn ReportModeSource>,
    tally: Arc<ReportTally>,
}

pub fn router(
    service: &'static str,
    path: &'static str,
    authorizer: Arc<dyn PolicyClient>,
    roles: Arc<SnapshotRoleReader>,
    mode: Arc<dyn ReportModeSource>,
    tally: Arc<ReportTally>,
) -> Router {
    Router::new()
        .route(path, get(read))
        .with_state(Arc::new(Inventory {
            service,
            authorizer,
            roles,
            mode,
            tally,
        }))
}

async fn read(State(state): State<Arc<Inventory>>, CurrentUser(user): CurrentUser) -> Response {
    match state
        .authorizer
        .ask(&user, controls::READ_POLICY_RULE)
        .await
    {
        Ok(Decision::Allow { scope: Scope::All }) => Json(serde_json::json!({
            "service": state.service,
            "mode": state.mode.mode(),
            "snapshot": state.roles.snapshot_status(),
            // The tally with its window read from the log: a restart
            // empties the counts and not the window (backlog e0bdba74).
            "report": state.tally.durable_snapshot().await
        }))
        .into_response(),
        Ok(_) => (
            StatusCode::FORBIDDEN,
            "requires read policy-rule at scope all",
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}
