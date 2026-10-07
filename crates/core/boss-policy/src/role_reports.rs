//! Report-only local policy comparison. Reading its inventory asks the
//! original policy engine, so observation never observes itself.
use crate::check_mode::CheckMode;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use boss_policy_client::engine::PolicyEngine;
use boss_policy_client::port::{PolicyError, PolicyRepository};
use boss_policy_client::role_reader::SnapshotRoleReader;
use boss_policy_client::role_reporting::{LocalReportingObserver, ReportModeSource, ReportTally};
use boss_policy_client::{CurrentUser, Decision, Scope, controls};
use std::sync::Arc;
use std::time::Duration;

struct ReportReader<R: PolicyRepository> {
    authorizer: PolicyEngine<R>,
    roles: Arc<SnapshotRoleReader>,
    mode: Arc<dyn ReportModeSource>,
    tally: Arc<ReportTally>,
}

pub fn mount<R: PolicyRepository + 'static>(
    repo: Arc<R>,
    check_mode: Arc<CheckMode>,
    sources: Arc<dyn crate::coverage::CoverageSources>,
    roles: Arc<SnapshotRoleReader>,
    mode: Arc<dyn ReportModeSource>,
    tally: Arc<ReportTally>,
    budget: Duration,
) -> Router {
    let observer = Arc::new(LocalReportingObserver::new(
        repo.clone(),
        roles.clone(),
        tally.clone(),
        mode.clone(),
        budget,
    ));
    let state = crate::http::PolicyApiState {
        repo: repo.clone(),
        engine: Arc::new(PolicyEngine::with_observer(repo.clone(), observer)),
        check_mode,
        sources,
    };
    let reader = Arc::new(ReportReader {
        authorizer: PolicyEngine::new(repo),
        roles,
        mode,
        tally,
    });
    crate::http::router(state).merge(
        Router::new()
            .route("/api/policy/actor-role-reports", get(reports::<R>))
            .with_state(reader),
    )
}

async fn reports<R: PolicyRepository + 'static>(
    State(state): State<Arc<ReportReader<R>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    match state
        .authorizer
        .ask(&user, controls::READ_POLICY_RULE)
        .await
    {
        Ok(Decision::Allow { scope: Scope::All }) => Json(serde_json::json!({
            "service": "policy", "mode": state.mode.mode(),
            "snapshot": state.roles.snapshot_status(), "report": state.tally.snapshot(),
            "policy_snapshot_atomic": false
        }))
        .into_response(),
        Ok(_) => (
            StatusCode::FORBIDDEN,
            "requires read policy-rule at scope all",
        )
            .into_response(),
        Err(error) => {
            let status = match error {
                PolicyError::NotFound(_) => StatusCode::NOT_FOUND,
                PolicyError::Conflict(_) => StatusCode::CONFLICT,
                PolicyError::Refused(_) => StatusCode::FORBIDDEN,
                PolicyError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, error.to_string()).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::in_memory::InMemoryPolicy;
    use boss_policy_client::role_reader::MonotonicRoleSnapshotClock;
    use boss_policy_client::role_reporting::ReportMode;
    use boss_policy_client::{Action, PolicyRule, Resource, User};
    use tower::ServiceExt;

    // The production report mount must preserve the write guard's sources,
    // while reading the report itself stays outside its own observer.
    #[tokio::test]
    async fn the_report_mount_preserves_the_guard_and_its_nonrecursive_read() {
        for mode in [ReportMode::Off, ReportMode::Report] {
            for (sources, expected) in [
                (
                    crate::guard::fixtures::Fixed::founder_only(),
                    StatusCode::CONFLICT,
                ),
                (crate::guard::fixtures::Fixed::bystander(), StatusCode::OK),
            ] {
                let repo = Arc::new(InMemoryPolicy::with_rules(crate::default_rules()));
                let before = repo.list_rules().await.unwrap();
                let roles = Arc::new(SnapshotRoleReader::new(
                    Duration::from_secs(30),
                    Arc::new(MonotonicRoleSnapshotClock),
                ));
                let tally = Arc::new(ReportTally::new(10));
                let app = mount(
                    repo.clone(),
                    CheckMode::fixed(crate::check_mode::Mode::Off),
                    Arc::new(sources),
                    roles,
                    Arc::new(mode),
                    tally.clone(),
                    Duration::from_millis(50),
                );
                let mut user = User::anonymous();
                user.id = "emp-founder".into();
                user.role = "platform-admin".into();
                let narrowed = PolicyRule::new(
                    "platform-admin",
                    Resource::policy_rule(),
                    Action::Update,
                    Scope::None,
                );
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("PUT")
                            .uri("/api/policy/rules/platform-admin:policy-rule:update")
                            .header("x-boss-user", serde_json::to_string(&user).unwrap())
                            .header("content-type", "application/json")
                            .body(Body::from(
                                serde_json::json!({"rule": narrowed}).to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), expected, "{mode:?}");
                if expected == StatusCode::CONFLICT {
                    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                        .await
                        .unwrap();
                    let why = String::from_utf8(bytes.to_vec()).unwrap();
                    assert!(
                        why.contains("policy:update:policy-rule") && why.contains("emp-founder"),
                        "{why}"
                    );
                    assert_eq!(repo.list_rules().await.unwrap(), before);
                } else {
                    assert_eq!(
                        repo.rule_for(&narrowed.id).await.unwrap().unwrap().scope,
                        Scope::None
                    );
                }
                let report_before = tally.snapshot();
                let response = app
                    .oneshot(
                        Request::builder()
                            .uri("/api/policy/actor-role-reports")
                            .header("x-boss-user", serde_json::to_string(&user).unwrap())
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(
                    serde_json::to_value(tally.snapshot()).unwrap(),
                    serde_json::to_value(report_before).unwrap(),
                    "report must not observe itself"
                );
            }
        }
    }
}
