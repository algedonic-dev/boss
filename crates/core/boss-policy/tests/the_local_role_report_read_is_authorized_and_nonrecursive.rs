use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_policy::check_mode::{CheckMode, Mode};
use boss_policy::role_reports::mount;
use boss_policy_client::in_memory::InMemoryPolicy;
use boss_policy_client::port::PolicyRepository;
use boss_policy_client::role_reader::{MonotonicRoleSnapshotClock, SnapshotRoleReader};
use boss_policy_client::role_reporting::{ReportMode, ReportTally};
use boss_policy_client::{AccessTier, Action, PolicyRule, Resource, Scope, User};
use http_body_util::BodyExt;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

struct UnreadSources;

#[async_trait::async_trait]
impl boss_policy::coverage::CoverageSources for UnreadSources {
    async fn roster(&self) -> Result<Vec<boss_policy_client::coverage::Person>, String> {
        panic!("a report read must not read guard sources")
    }
    async fn keys(&self) -> Result<Vec<boss_policy_client::coverage::Key>, String> {
        panic!("a report read must not read guard sources")
    }
    async fn workflows(&self) -> Result<Vec<boss_policy_client::coverage::WorkflowFacts>, String> {
        panic!("a report read must not read guard sources")
    }
}

#[tokio::test]
async fn report_read_requires_existing_authority_and_does_not_observe_itself() {
    let repo = Arc::new(InMemoryPolicy::new());
    repo.upsert_rule(
        &PolicyRule::new("reader", Resource::policy_rule(), Action::Read, Scope::All),
        "fixture",
    )
    .await
    .unwrap();
    let roles = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let tally = Arc::new(ReportTally::new(10));
    let app = mount(
        repo,
        CheckMode::fixed(Mode::Off),
        Arc::new(UnreadSources),
        roles,
        Arc::new(ReportMode::Report),
        tally.clone(),
        Duration::from_millis(50),
    );
    let refused = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/policy/actor-role-reports")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let user = User {
        id: "reader-identity".into(),
        role: "reader".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    };
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
    let value: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(value["service"], "policy");
    assert_eq!(value["mode"], "report");
    assert_eq!(value["snapshot"]["state"], "never-loaded");
    assert_eq!(value["report"]["durable_window"], false);
    assert_eq!(value["report"]["rows"], serde_json::json!([]));
    assert!(tally.snapshot().rows.is_empty());
}
