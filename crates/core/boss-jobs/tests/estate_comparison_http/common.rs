use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::port::JobsRepository;
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use tower::ServiceExt;

// SERIALISE THE REAL TYPE, never a copy of its wire shape (7c3649e2).
pub fn user_header(id: &str, tier: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: "platform-admin".to_string(),
        access_tier: serde_json::from_value::<AccessTier>(serde_json::Value::String(
            tier.to_string(),
        ))
        .expect("unknown access tier"),
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("platform".to_string()),
    })
    .expect("a User always serialises")
}

pub fn app_with<R: JobsRepository + 'static>(jobs: Arc<R>) -> axum::Router {
    let policy: Arc<dyn PolicyClient> = Arc::new(FakePolicyClient::builder().build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    router(state)
}

pub async fn post_comparison(app: &axum::Router, user: Option<&str>, body: &str) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/estate/comparison")
        .header("content-type", "application/json");
    if let Some(u) = user {
        req = req.header("x-boss-user", u);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    resp.status()
}

pub fn comparison_body() -> String {
    serde_json::json!({
        "scope": "kubernetes-nodes",
        "observed_at": "2026-08-30T10:20:00Z",
        "observer": "boss-estate-observe",
        "counts": { "observed": 5, "observed_not_declared": 1 },
        "findings": {
            "observed_not_declared": [{"id": "w-1"}],
            "declared_not_observed": [],
            "drift": [],
        },
    })
    .to_string()
}

pub async fn exercise_observation_identity_boundary(app: &axum::Router) {
    let user = user_header("rule:estate-compare-on-observation", "operator");
    let mut body: serde_json::Value = serde_json::from_str(&comparison_body()).unwrap();
    for id in [
        "a56cb17b-00ba-4f61-9dc3-01c48871968a",
        "A56CB17B-00BA-4F61-9DC3-01C48871968A",
        "a56cb17b-00ba-4f61-9dc3-01c48871968b",
    ] {
        body["observation_id"] = serde_json::json!(id);
        assert_eq!(
            post_comparison(app, Some(&user), &body.to_string()).await,
            StatusCode::ACCEPTED
        );
    }
    body["observation_id"] = serde_json::json!("not-an-observation-uuid");
    assert_eq!(
        post_comparison(app, Some(&user), &body.to_string()).await,
        StatusCode::BAD_REQUEST
    );
}
