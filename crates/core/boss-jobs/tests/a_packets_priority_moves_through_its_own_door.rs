//! `PUT /api/jobs/{id}/priority` moves ONE field, with a reason, on the
//! record (backlog eb26059d).
//!
//! MEASURED 2026-10-18: raising 99b7ae76 from standard to urgent took a
//! GET of the whole job, an edit, and a PUT of the whole body back —
//! the job PUT replaces the row — then a hand diff of the read-back. No
//! verb and no narrow route set the field, and nothing recorded why.
//!
//! These pin the door the item asked for: the move lands and reads
//! back; the packet's reserved `priority_changes` list records from,
//! to, who and why; the state event is signed by the caller; a finished
//! packet is refused with nothing written; a reason is required; asking
//! for the priority it already has writes nothing; the caller needs
//! Update on the job; and the list has one writer — the metadata merge
//! door refuses it and the job PUT cannot rewrite it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const KIND: &str = "ship-a-change";
const ADMIN: &str = r#"{"id":"emp-bootstrap-admin","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;
const READER: &str = r#"{"id":"emp-reader","role":"reader","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

struct AdminRoster;

#[async_trait::async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

struct App {
    router: axum::Router,
    jobs: Arc<InMemoryJobs>,
}

fn app() -> App {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let mut policy = FakePolicyClient::builder();
    for (action, resource) in [
        (Action::Create, Resource::job()),
        (Action::Read, Resource::job()),
        (Action::Update, Resource::job()),
        (Action::Close, Resource::job()),
        (Action::Update, Resource::step()),
    ] {
        policy = policy.allow("platform-admin", action, resource, Scope::All);
    }
    // A reader sees every packet and may change none.
    policy = policy.allow("reader", Action::Read, Resource::job(), Scope::All);
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    App {
        router: router(state),
        jobs,
    }
}

async fn send(app: &App, user: &str, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", user)
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = app.router.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    (status, json)
}

async fn open(app: &App) -> String {
    let (status, job) = send(
        app,
        ADMIN,
        "POST",
        "/api/jobs",
        json!({
            "kind": KIND,
            "subject": {"subject_kind": "custom", "id": "feat/x"},
            "title": "t", "owner_id": "emp-bootstrap-admin",
            "status": "open", "priority": "standard",
            "metadata": {}, "tags": [],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create: {job}");
    job["id"].as_str().expect("job id").to_string()
}

async fn read(app: &App, id: &str) -> Value {
    let (status, job) = send(app, ADMIN, "GET", &format!("/api/jobs/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "job read: {job}");
    job
}

async fn set(app: &App, user: &str, id: &str, body: Value) -> (StatusCode, Value) {
    send(app, user, "PUT", &format!("/api/jobs/{id}/priority"), body).await
}

fn changes(job: &Value) -> Vec<Value> {
    job.pointer("/metadata/priority_changes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn the_move_lands_reads_back_and_records_who_and_why() {
    let app = app();
    let id = open(&app).await;
    let events_before = app.jobs.recorded_events().len();
    let (status, body) = set(
        &app,
        ADMIN,
        &id,
        json!({"priority": "urgent", "why": "the conductor is blocked on it"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["changed"], true, "{body}");
    assert_eq!(body["from"], "standard", "{body}");
    assert_eq!(body["priority"], "urgent", "{body}");

    let job = read(&app, &id).await;
    assert_eq!(
        job["priority"], "urgent",
        "the packet holds the move: {job}"
    );
    let list = changes(&job);
    assert_eq!(list.len(), 1, "{job}");
    assert_eq!(list[0]["from"], "standard");
    assert_eq!(list[0]["to"], "urgent");
    assert_eq!(list[0]["by"], "emp-bootstrap-admin");
    assert_eq!(list[0]["why"], "the conductor is blocked on it");
    assert!(list[0]["at"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(body["record"], list[0], "the answer is the record");

    // On the log, signed by the caller, carrying the row it wrote.
    let events = app.jobs.recorded_events();
    let written: Vec<_> = events[events_before..].iter().collect();
    assert_eq!(written.len(), 1, "one state event: {written:?}");
    assert_eq!(written[0].kind, "jobs.job.updated");
    assert_eq!(written[0].payload["priority"], "urgent");
    assert_eq!(written[0].payload["_actor"], "emp-bootstrap-admin");

    // A second move appends; the first record stays as it was.
    let (status, _) = set(
        &app,
        ADMIN,
        &id,
        json!({"priority": "emergency", "why": "dark"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let list = changes(&read(&app, &id).await);
    assert_eq!(list.len(), 2);
    assert_eq!(
        (list[1]["from"].as_str(), list[1]["to"].as_str()),
        (Some("urgent"), Some("emergency"))
    );
}

#[tokio::test]
async fn the_priority_it_already_has_writes_nothing() {
    let app = app();
    let id = open(&app).await;
    let events_before = app.jobs.recorded_events().len();
    let (status, body) = set(
        &app,
        ADMIN,
        &id,
        json!({"priority": "standard", "why": "w"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["changed"], false, "{body}");
    assert!(changes(&read(&app, &id).await).is_empty());
    assert_eq!(
        app.jobs.recorded_events().len(),
        events_before,
        "nothing recorded"
    );
}

#[tokio::test]
async fn a_move_without_a_reason_or_with_an_unknown_word_is_refused() {
    let app = app();
    let id = open(&app).await;
    for body in [
        json!({"priority": "urgent", "why": "   "}),
        json!({"priority": "urgent"}),
        json!({"priority": "high", "why": "w"}),
    ] {
        let (status, said) = set(&app, ADMIN, &id, body.clone()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {said}");
    }
    let job = read(&app, &id).await;
    assert_eq!(job["priority"], "standard");
    assert!(changes(&job).is_empty());
}

#[tokio::test]
async fn a_finished_packets_priority_does_not_move() {
    let app = app();
    let id = open(&app).await;
    let mut body = read(&app, &id).await;
    body.as_object_mut().unwrap().remove("steps");
    body["status"] = json!("closed");
    let (status, said) = send(&app, ADMIN, "PUT", &format!("/api/jobs/{id}"), body).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "close by hand: {said}");

    let (status, said) = set(&app, ADMIN, &id, json!({"priority": "urgent", "why": "w"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(
        said.to_string().contains("closed"),
        "names the status: {said}"
    );
    let job = read(&app, &id).await;
    assert_eq!(job["priority"], "standard");
    assert!(changes(&job).is_empty());
}

#[tokio::test]
async fn a_caller_who_may_not_update_the_job_is_refused() {
    let app = app();
    let id = open(&app).await;
    let (status, said) = set(&app, READER, &id, json!({"priority": "urgent", "why": "w"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{said}");
    assert_eq!(read(&app, &id).await["priority"], "standard");
}

#[tokio::test]
async fn the_record_has_one_writer() {
    let app = app();
    let id = open(&app).await;
    let (status, _) = set(
        &app,
        ADMIN,
        &id,
        json!({"priority": "urgent", "why": "first"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let kept = changes(&read(&app, &id).await);

    // The merge door refuses the key outright.
    let (status, said) = send(
        &app,
        ADMIN,
        "PATCH",
        &format!("/api/jobs/{id}/metadata"),
        json!({"priority_changes": null}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(
        said.to_string().contains("/priority"),
        "names the door: {said}"
    );

    // The job PUT carries the stored list over whatever its body holds.
    let mut body = read(&app, &id).await;
    body.as_object_mut().unwrap().remove("steps");
    body["metadata"]["priority_changes"] = json!([{"forged": true}]);
    let (status, said) = send(&app, ADMIN, "PUT", &format!("/api/jobs/{id}"), body).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{said}");
    assert_eq!(changes(&read(&app, &id).await), kept);
}
