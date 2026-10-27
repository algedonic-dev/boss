//! The small reader observes owned work; it never starts history work.
use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use boss_core::{port::EventBus, publisher::DomainPublisher};
use boss_jobs::{
    InMemoryJobs,
    http::{JobsApiState, router},
    membership_projection::InMemoryMembershipProjection,
    membership_projection_service::MembershipProjectionService,
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use tower::ServiceExt;

const PATH: &str = "/api/jobs/membership-refresh-observation";

fn app_and_service() -> (axum::Router, Arc<MembershipProjectionService>) {
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
    let jobs = Arc::new(InMemoryJobs::new());
    let clock = Arc::new(boss_clock_client::WallClockClient);
    let mut state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        clock.clone(),
    );
    let service = Arc::new(MembershipProjectionService::new(
        jobs,
        Arc::new(InMemoryMembershipProjection::new()),
        clock,
    ));
    state.membership_projection = Some(service.clone());
    (router(state), service)
}

fn app() -> axum::Router {
    app_and_service().0
}

#[tokio::test]
async fn a_reader_cannot_observe_another_readers_completed_flight() {
    let (app, service) = app_and_service();
    let alice = boss_jobs::membership_projection::ProjectionIdentity::new(
        Some("alice".into()),
        boss_jobs::port::JobScope::All,
    );
    assert!(service.refresh_one(&alice).await.is_err());
    let response = app
        .clone()
        .oneshot(request("operator", PATH, ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["state"], "observed");
    assert!(value["latest_completed"]["id"].is_string());
    let bob = Request::builder()
        .uri(PATH)
        .header(
            "x-boss-user",
            serde_json::json!({"id":"bob","role":"operator"}).to_string(),
        )
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(bob).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let foreign: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(foreign["state"], "not_observed");
    assert!(foreign["latest_completed"].is_null());
    assert!(
        !String::from_utf8(body.to_vec())
            .unwrap()
            .contains(value["latest_completed"]["id"].as_str().unwrap())
    );
}

fn request(role: &str, path: &str, body: &'static str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header(
            "x-boss-user",
            serde_json::json!({"id":"alice","role":role}).to_string(),
        )
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn a_fixed_small_read_reports_no_observed_flight_without_admitting_one() {
    let app = app();
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(request("operator", PATH, ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["state"], "not_observed");
        assert!(value["active"].is_null());
        assert!(value["latest_completed"].is_null());
    }
}

#[tokio::test]
async fn the_reader_preserves_both_existing_policy_gates_and_refuses_selection() {
    for role in ["jobs-only", "estate-only"] {
        let response = app().oneshot(request(role, PATH, "")).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    for (path, body) in [
        (format!("{PATH}?actor=bob"), ""),
        (PATH.into(), "{}"),
        (format!("{PATH}?window=48h"), ""),
    ] {
        let response = app()
            .oneshot(request("operator", &path, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
