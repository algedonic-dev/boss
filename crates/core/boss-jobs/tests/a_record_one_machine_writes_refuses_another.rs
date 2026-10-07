//! A step that declares `written_by` refuses its record from every
//! other automation or agent session, and never from a person (backlog
//! aa816dd4, LOW-1 of review ea2ecfd4).
//!
//! ops-request's `nothing-to-do` terminal closes a request with no
//! passkey asked, gated on a record the ops runner writes — and any
//! caller with Update on the step could write that record and the
//! marker, closing a request on a plan no runner rendered. The shape
//! here is that terminal's, reduced: a trigger, and an outcome behind a
//! job marker whose required field is the runner's record.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

const KIND: &str = "a-record-one-machine-writes";
const RUNNER: &str = "automation:ops-runner";

fn spec() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        KIND,
        "A record one machine writes",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "filed".into(),
                kind: "trigger".into(),
                ready_when: "true".into(),
                title_template: "Filed".into(),
                ..Default::default()
            },
            StepSpec {
                title: "nothing-to-do".into(),
                kind: "outcome".into(),
                ready_when: "steps.filed.done AND job.metadata.plan_names_nothing".into(),
                title_template: "Nothing to do".into(),
                metadata_defaults: serde_json::json!({
                    "outcome_kind": "skipped",
                    "written_by": RUNNER,
                }),
                terminal: Some(Terminal {
                    outcome: "nothing-to-do".into(),
                }),
                fields: vec![
                    serde_json::from_value(serde_json::json!(
                        {"name": "plan", "field_type": "string", "required": true}
                    ))
                    .unwrap(),
                ],
                ..Default::default()
            },
        ],
    );
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec
}

fn header(id: &str) -> String {
    serde_json::json!({
        "id": id,
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn app() -> axum::Router {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(spec()).expect("seed the kind");
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    router(JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

/// Open a packet and answer `(job id, nothing-to-do step id)`.
async fn open(app: &axum::Router) -> (String, String) {
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", header("emp-bootstrap-admin"))
            .body(Body::from(
                serde_json::json!({
                    "kind": KIND,
                    "subject": { "subject_kind": "custom", "id": "boss-gcp" },
                    "title": "A remedy request",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": {},
                    "tags": ["test"],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    let id = job["id"].as_str().unwrap().to_string();
    let (status, job) = send(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{id}"))
            .header("x-boss-user", header("emp-bootstrap-admin"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let step = job["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["spec_slug"] == "nothing-to-do")
        .unwrap_or_else(|| panic!("no nothing-to-do step: {job:#}"));
    assert_eq!(step["metadata"]["written_by"], RUNNER, "{step:#}");
    (id, step["id"].as_str().unwrap().to_string())
}

async fn merge(
    app: &axum::Router,
    job: &str,
    step: &str,
    as_actor: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    send(
        app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/jobs/{job}/steps/{step}/metadata"))
            .header("content-type", "application/json")
            .header("x-boss-user", header(as_actor))
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn an_agent_or_another_rule_writing_the_record_is_refused_by_name() {
    let app = app();
    let (job, step) = open(&app).await;
    for actor in [
        "agent-claude",
        "automation:rule:complete-marker-on-step-ready",
    ] {
        let (status, body) = merge(
            &app,
            &job,
            &step,
            actor,
            serde_json::json!({"plan": "a plan no runner rendered"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{actor}: {body}");
        assert_eq!(body["actor_id"], actor, "{body}");
        assert_eq!(body["written_by"], RUNNER, "{body}");
        assert_eq!(body["refused_keys"], serde_json::json!(["plan"]), "{body}");
    }
}

#[tokio::test]
async fn the_declared_runner_writes_its_record() {
    let app = app();
    let (job, step) = open(&app).await;
    let (status, body) = merge(
        &app,
        &job,
        &step,
        RUNNER,
        serde_json::json!({"plan": "would vacuum the journal to 1G\n"}),
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");
}

/// Never on a person's path (DR rule 62dac114): David closing a
/// request by hand is a decision, and the record says it was his.
#[tokio::test]
async fn a_person_is_never_refused() {
    let app = app();
    let (job, step) = open(&app).await;
    let (status, body) = merge(
        &app,
        &job,
        &step,
        "emp-bootstrap-admin",
        serde_json::json!({"plan": "closed by hand"}),
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");
}

/// The declaration cannot be written away first: deleting it is a
/// protocol key changed, refused to every writer.
#[tokio::test]
async fn the_declaration_cannot_be_deleted_to_get_round_it() {
    let app = app();
    let (job, step) = open(&app).await;
    let (status, body) = merge(
        &app,
        &job,
        &step,
        "agent-claude",
        serde_json::json!({"written_by": null}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["refused_keys"], serde_json::json!(["written_by"]));
}
