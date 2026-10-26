//! The fixed diagnostic is policy-scoped and accepts no caller-selected work.
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use boss_core::{port::EventBus, publisher::DomainPublisher};
use boss_jobs::{
    InMemoryJobs,
    http::{JobsApiState, router},
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use serde_json::json;
use tower::ServiceExt;

const PATH: &str = "/api/jobs/membership-starting-state-observation";

fn app() -> axum::Router {
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::estate(), Scope::All)
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("jobs-only", Action::Read, Resource::job(), Scope::All)
            .allow("estate-only", Action::Read, Resource::estate(), Scope::All)
            .build(),
    );
    router(JobsApiState::minimal(
        Arc::new(InMemoryJobs::new()),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    ))
}

fn request(role: &str, path: &str, body: &'static str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(path)
        .header(
            "x-boss-user",
            json!({"id":"agent-fixture", "role":role}).to_string(),
        )
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn a_job_reader_without_estate_diagnostic_authority_is_refused() {
    let response = app().oneshot(request("jobs-only", PATH, "")).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "an expensive diagnostic must have its existing estate policy gate"
    );
}

#[tokio::test]
async fn estate_authority_without_a_readable_job_scope_is_refused() {
    let response = app()
        .oneshot(request("estate-only", PATH, ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_fixed_observation_refuses_query_arguments_and_request_bytes() {
    for (path, body) in [(format!("{PATH}?window=48h"), ""), (PATH.to_owned(), "{}")] {
        let response = app()
            .oneshot(request("operator", &path, body))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "the caller cannot select SQL, windows, limits or work through query/body"
        );
    }
}

#[test]
fn the_native_reader_and_server_share_the_report_byte_bound() {
    let source = std::fs::read_to_string(
        boss_testing::repo_root().join("infra/forge/read-membership-starting-state.sh"),
    )
    .unwrap();
    assert!(source.lines().any(|line| line
        == format!(
            "MAX_REPORT_BYTES={}",
            boss_jobs::membership_observation::MAX_REPORT_BYTES
        )));
}
