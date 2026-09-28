//! End-to-end proof that the StepPlugin registry HTTP surface
//! works: create → publish → retire, plus policy gating (a guest
//! with Create but not Publish is blocked from publishing).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{
    InMemoryJobs, InMemoryStepPlugins, StepPluginRegistry, StepPluginSpec, WorkflowStatus,
};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn cto() -> User {
    User {
        id: "emp-cto".into(),
        role: "cto".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn guest() -> User {
    User {
        id: "anonymous".into(),
        role: "guest".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn user_header(u: &User) -> String {
    serde_json::to_string(u).unwrap()
}

fn draft(kind: &str) -> StepPluginSpec {
    StepPluginSpec::draft(
        kind,
        format!("Test {kind}"),
        "qa",
        format!("{kind}.js"),
        serde_json::json!({}),
    )
}

fn build_app(registry: Arc<dyn StepPluginRegistry>, policy: Arc<dyn PolicyClient>) -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let state = JobsApiState {
        step_registry,
        plugin_registry: Some(registry),
        ..JobsApiState::minimal(
            jobs,
            bus,
            publisher,
            policy,
            std::sync::Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

async fn send_json(
    app: Router,
    method: &str,
    uri: &str,
    user: &User,
    body: Option<serde_json::Value>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-boss-user", user_header(user));
    let body = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&v).unwrap())
        }
        None => Body::empty(),
    };
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

#[tokio::test]
async fn full_plugin_lifecycle() {
    let registry: Arc<dyn StepPluginRegistry> = Arc::new(InMemoryStepPlugins::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Read, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Create, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Update, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Publish, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Retire, Resource::step_plugin(), Scope::All)
            .build(),
    );
    let app = build_app(registry, policy);

    // 1. Create draft.
    let body = serde_json::to_value(draft("emerald-inspection")).unwrap();
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/jobs/step-plugins",
        &cto(),
        Some(body),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let stored: StepPluginSpec = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(stored.version, 1);
    assert_eq!(stored.status, WorkflowStatus::Draft);

    // 2. Active GET 404s (no active yet).
    let resp = send_json(
        app.clone(),
        "GET",
        "/api/jobs/step-plugins/emerald-inspection",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 3. Publish.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/jobs/step-plugins/emerald-inspection/publish",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. Active now visible.
    let resp = send_json(
        app.clone(),
        "GET",
        "/api/jobs/step-plugins/emerald-inspection",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let active: StepPluginSpec = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(active.status, WorkflowStatus::Active);

    // 5. Retire.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/jobs/step-plugins/emerald-inspection/retire",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 6. Active GET 404s again after retire.
    let resp = send_json(
        app,
        "GET",
        "/api/jobs/step-plugins/emerald-inspection",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guest_with_create_cannot_publish() {
    // Publish is independently policy-gated from Create (Q7: manager-
    // tier install with policy). A guest who can draft can't publish.
    let registry: Arc<dyn StepPluginRegistry> = Arc::new(InMemoryStepPlugins::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("guest", Action::Read, Resource::step_plugin(), Scope::All)
            .allow("guest", Action::Create, Resource::step_plugin(), Scope::All)
            .build(),
    );
    let app = build_app(registry, policy);

    // Guest drafts — allowed.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/jobs/step-plugins",
        &guest(),
        Some(serde_json::to_value(draft("guest-plugin")).unwrap()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Guest publishes — denied.
    let resp = send_json(
        app,
        "POST",
        "/api/jobs/step-plugins/guest-plugin/publish",
        &guest(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

fn as_role(id: &str, role: &str) -> User {
    User {
        id: id.into(),
        role: role.into(),
        ..cto()
    }
}

/// THE DEFECT (backlog 1a4a4d03, first finding): the in-flight count
/// asked only for Read on `step-plugin` — which the basic guest
/// (`visitor`) holds by shipped default — and counted steps of ANY
/// `{kind}` it was handed, plugin or not: a live measure of the
/// company's work volume, per kind, to a stranger. The count is a read
/// of steps across every packet, so it takes Read on `step` at scope
/// all as well, and it answers only for a kind the plugin registry
/// holds. Its one caller, the retire confirm on the plugin page, is the
/// operator's, who holds both.
#[tokio::test]
async fn the_in_flight_count_reads_steps_and_only_of_a_plugin() {
    let registry: Arc<dyn StepPluginRegistry> = Arc::new(InMemoryStepPlugins::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Read, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Create, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Publish, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Read, Resource::step(), Scope::All)
            // The basic guest's shipped grant: the operating model only.
            .allow("visitor", Action::Read, Resource::step_plugin(), Scope::All)
            .allow("visitor", Action::Read, Resource::workflow(), Scope::All)
            // A reader of its own steps only: the count spans everyone's.
            .allow("clerk", Action::Read, Resource::step_plugin(), Scope::All)
            .allow("clerk", Action::Read, Resource::step(), Scope::Self_)
            .build(),
    );
    let app = build_app(registry, policy);
    let body = serde_json::to_value(draft("emerald-inspection")).unwrap();
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/jobs/step-plugins",
        &cto(),
        Some(body),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/jobs/step-plugins/emerald-inspection/publish",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let count = |kind: &str| format!("/api/jobs/step-plugins/{kind}/in-flight-count");
    let visitor = as_role("guest@algedonic.dev", "visitor");
    let clerk = as_role("emp-clerk", "clerk");
    for (who, caller) in [
        ("the basic guest", &visitor),
        ("a self-scoped reader", &clerk),
    ] {
        // A plugin's kind and a kind no plugin names are refused alike,
        // so the refusal does not say which kinds exist either.
        for kind in ["emerald-inspection", "checklist"] {
            let resp = send_json(app.clone(), "GET", &count(kind), caller, None).await;
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{who}: {kind}");
            let text = resp.into_body().collect().await.unwrap().to_bytes();
            assert!(
                !String::from_utf8_lossy(&text).contains("in_flight"),
                "{who}: {kind}: no count leaves in the refusal"
            );
        }
    }

    // The operator reads the count of a plugin's kind…
    let resp = send_json(
        app.clone(),
        "GET",
        &count("emerald-inspection"),
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["in_flight"], 0, "{v}");
    // …and of no other: a step kind no plugin names is not this door's.
    let resp = send_json(app, "GET", &count("checklist"), &cto(), None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
