//! End-to-end proof that the Workflow registry HTTP surface works:
//! create → draft visible → publish → active → publish-again
//! transitions versioning correctly → retire hides the kind.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, Terminal, WorkflowRegistry, WorkflowSpec, WorkflowStatus};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows};
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

fn draft_spec(kind: &str) -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        kind,
        format!("Test {kind}"),
        "test",
        vec!["system".into()],
        Vec::new(),
    )
}

fn build_app(registry: Arc<dyn WorkflowRegistry>) -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Read, Resource::workflow(), Scope::All)
            .allow("cto", Action::Create, Resource::workflow(), Scope::All)
            .allow("cto", Action::Update, Resource::workflow(), Scope::All)
            .allow("cto", Action::Publish, Resource::workflow(), Scope::All)
            .allow("cto", Action::Retire, Resource::workflow(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs,
        bus,
        publisher,
        step_registry,
        policy,
        kind_registry: Some(registry),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
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

// --- author-time dry-run lint (POST /api/workflows/_validate) ---

fn trigger_step() -> StepSpec {
    StepSpec {
        title: "start".into(),
        kind: "task".into(),
        ready_when: "true".into(),
        ..Default::default()
    }
}

fn terminal_step() -> StepSpec {
    StepSpec {
        title: "finish".into(),
        kind: "task".into(),
        ready_when: "steps.start.done".into(),
        terminal: Some(Terminal {
            outcome: "done".into(),
        }),
        ..Default::default()
    }
}

async fn dry_run(app: Router, spec: &WorkflowSpec) -> serde_json::Value {
    let resp = send_json(
        app,
        "POST",
        "/api/workflows/_validate",
        &cto(),
        Some(serde_json::json!({ "kind": spec.kind, "steps": spec.steps })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn dry_run_validate_passes_a_viable_spec() {
    let registry: Arc<dyn WorkflowRegistry> = Arc::new(InMemoryWorkflows::new());
    let app = build_app(registry);
    let mut spec = draft_spec("viable");
    spec.steps = vec![trigger_step(), terminal_step()];

    let body = dry_run(app, &spec).await;
    assert_eq!(
        body["ok"].as_bool(),
        Some(true),
        "viable spec should pass: {body}"
    );
    assert_eq!(body["problems"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn dry_run_validate_flags_missing_terminal_without_persisting() {
    let registry: Arc<dyn WorkflowRegistry> = Arc::new(InMemoryWorkflows::new());
    let app = build_app(registry.clone());
    let mut spec = draft_spec("no-terminal");
    spec.steps = vec![trigger_step()]; // trigger only — no terminal

    let body = dry_run(app, &spec).await;
    assert_eq!(body["ok"].as_bool(), Some(false));
    let problems = body["problems"].as_array().unwrap();
    let joined: String = problems
        .iter()
        .filter_map(|p| p["message"].as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        joined.contains("no terminal"),
        "expected a 'no terminal' problem, got: {joined}"
    );

    // The dry run must not persist: the kind is not in the registry.
    assert!(
        registry.get_active("no-terminal").await.is_err(),
        "dry-run must not create the kind"
    );
}

#[tokio::test]
async fn full_create_publish_retire_cycle() {
    let registry: Arc<dyn WorkflowRegistry> = Arc::new(InMemoryWorkflows::new());
    let app = build_app(registry.clone());

    // 1. Create draft.
    let body = serde_json::to_value(draft_spec("warranty-rework")).unwrap();
    let resp = send_json(app.clone(), "POST", "/api/workflows", &cto(), Some(body)).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let returned: WorkflowSpec = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(returned.version, 1);
    assert_eq!(returned.status, WorkflowStatus::Draft);

    // 2. GET /api/workflows/warranty-rework returns 404 — no active yet.
    let resp = send_json(
        app.clone(),
        "GET",
        "/api/workflows/warranty-rework",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 3. Publish.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/workflows/warranty-rework/publish",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. Active row now visible.
    let resp = send_json(
        app.clone(),
        "GET",
        "/api/workflows/warranty-rework",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let active: WorkflowSpec = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(active.status, WorkflowStatus::Active);
    assert_eq!(active.version, 1);

    // 5. PUT a new version (draft v2) and publish → v1 retires, v2 active.
    let v2_body = serde_json::to_value({
        let mut s = draft_spec("warranty-rework");
        s.label = "Warranty Rework v2".into();
        s
    })
    .unwrap();
    let resp = send_json(
        app.clone(),
        "PUT",
        "/api/workflows/warranty-rework",
        &cto(),
        Some(v2_body),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/workflows/warranty-rework/publish",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 6. /versions shows three rows: v1 retired, v2 active.
    let resp = send_json(
        app.clone(),
        "GET",
        "/api/workflows/warranty-rework/versions",
        &cto(),
        None,
    )
    .await;
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let versions: Vec<WorkflowSpec> = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].version, 1);
    assert_eq!(versions[0].status, WorkflowStatus::Retired);
    assert_eq!(versions[1].version, 2);
    assert_eq!(versions[1].status, WorkflowStatus::Active);

    // 7. Retire the active kind.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/workflows/warranty-rework/retire",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 8. Active lookup now 404s.
    let resp = send_json(app, "GET", "/api/workflows/warranty-rework", &cto(), None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guest_cannot_publish_even_if_they_could_create() {
    // A policy where guests can create drafts but can't publish. Verifies
    // that Publish is independently gated from Create, per the design.
    let registry: Arc<dyn WorkflowRegistry> = Arc::new(InMemoryWorkflows::new());
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("guest", Action::Read, Resource::workflow(), Scope::All)
            .allow("guest", Action::Create, Resource::workflow(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs,
        bus,
        publisher,
        step_registry,
        policy,
        kind_registry: Some(registry),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    };
    let app = router(state);

    // Guest creates a draft — allowed.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/workflows",
        &guest(),
        Some(serde_json::to_value(draft_spec("exploratory")).unwrap()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Guest publishes — denied.
    let resp = send_json(
        app,
        "POST",
        "/api/workflows/exploratory/publish",
        &guest(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_kinds_filters_by_category() {
    let registry = Arc::new(InMemoryWorkflows::new());
    // Seed two actives across two categories — use the raw seed helper
    // so we don't have to drive publish for every one.
    let mut refurb = draft_spec("refurb-test");
    refurb.status = WorkflowStatus::Active;
    refurb.category = "refurb".into();
    registry.seed(refurb).unwrap();
    let mut sale = draft_spec("sale-test");
    sale.status = WorkflowStatus::Active;
    sale.category = "sales".into();
    registry.seed(sale).unwrap();

    let registry_dyn: Arc<dyn WorkflowRegistry> = registry;
    let app = build_app(registry_dyn);

    let resp = send_json(
        app.clone(),
        "GET",
        "/api/workflows?category=sales",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let kinds: Vec<WorkflowSpec> = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].kind, "sale-test");
}
