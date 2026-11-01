//! `POST /api/jobs/{id}/input-classifications` — an operator classifies
//! ONE FROZEN VALUE of a packet for its executors' START (design e72dc7c9,
//! backlog 40f0436c).
//!
//! Measured: a dispatched START printed every value a packet had ever
//! recorded, 1,003,270 bytes on SOURCE 266f06e8, and an 836,756-byte Oct
//! 11 observation sat 959,711 bytes ahead of the pinned protocol. The
//! approved contract lets a holder of publish on workflow say that one
//! such value is an available (or mandatory) original rather than printed
//! required input — bound to the packet, the pointer and the value's
//! sha256, appended to a reserved list, the value untouched.
//!
//! These pin: the append and its declared event; publish authority (an
//! Update-only caller is refused and nothing is written); the refusals
//! that keep it from being a guess (protected material, a digest of a
//! value no longer there); and the two other writers that must not be able
//! to forge or rewrite the list (the metadata PATCH refuses the key; the
//! job PUT carries the stored list forward).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::execution_inputs::{CLASSIFICATIONS_KEY, sha256_hex, value_bytes};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const ID: &str = "00000000-0000-4000-8000-0000000c1a55";

fn user(id: &str, role: &str) -> User {
    User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("it".into()),
    }
}

fn observation() -> Value {
    json!({ "method": "gate window", "verdict": "green", "receipt": "r".repeat(20_000) })
}

fn build_app() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let mut policy = FakePolicyClient::builder();
    for role in ["platform-admin", "service-tech"] {
        policy = policy
            .allow(role, Action::Create, Resource::job(), Scope::All)
            .allow(role, Action::Update, Resource::job(), Scope::All)
            .allow(role, Action::Read, Resource::job(), Scope::All);
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(
        policy
            .allow(
                "platform-admin",
                Action::Publish,
                Resource::workflow(),
                Scope::All,
            )
            .build(),
    );
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        publisher,
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs)
}

async fn send(
    app: &Router,
    who: &User,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(who).unwrap())
                .body(match body {
                    Some(b) => Body::from(b.to_string()),
                    None => Body::empty(),
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, v)
}

async fn seed(jobs: &Arc<InMemoryJobs>) -> Job {
    let job = Job {
        id: JobId::from_uuid(Uuid::parse_str(ID).unwrap()),
        status: JobStatus::Open,
        metadata: json!({
            "description": "the item's scope",
            "coordinator_observation_20261011": observation(),
        }),
        ..Job::new(
            "backlog-item",
            Subject::new("custom", "/it/backlog"),
            "A packet with a large historical observation",
            "emp-1",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
        )
    };
    jobs.create_job(&job).await.unwrap();
    let mut triage = Step::new(job.id, "task", "Measure the claim", 0);
    triage.spec_slug = Some("triage".into());
    triage.status = StepStatus::Completed;
    triage.metadata = json!({ "evidence": "measured on main" });
    jobs.add_step(&triage).await.unwrap();
    let mut build = Step::new(job.id, "task", "Build the change", 1);
    build.spec_slug = Some("build".into());
    build.status = StepStatus::Active;
    jobs.add_step(&build).await.unwrap();
    job
}

fn request(pointer: &str, value: &Value) -> Value {
    json!({
        "pointer": pointer,
        "sha256": sha256_hex(&value_bytes(value)),
        "class": "available_historical",
        "purpose": "an Oct 11 gate-window observation",
        "reason": "the current repair supersedes it; retained verbatim",
    })
}

fn uri() -> String {
    format!("/api/jobs/{ID}/input-classifications")
}

#[tokio::test]
async fn a_publish_holder_classifies_a_frozen_value_and_the_value_stays_as_it_was() {
    let (app, jobs) = build_app();
    seed(&jobs).await;
    let admin = user("emp-david", "platform-admin");
    let (status, body) = send(
        &app,
        &admin,
        "POST",
        &uri(),
        Some(request(
            "/metadata/coordinator_observation_20261011",
            &observation(),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["index"], 0);

    let after = jobs
        .get_job(&JobId::from_uuid(Uuid::parse_str(ID).unwrap()))
        .await
        .unwrap()
        .unwrap();
    let list = after.metadata[CLASSIFICATIONS_KEY].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["job"], ID, "bound to this packet");
    assert_eq!(list[0]["by"], "emp-david");
    assert_eq!(
        list[0]["size_bytes"],
        value_bytes(&observation()).len(),
        "{list:?}"
    );
    assert_eq!(
        after.metadata["coordinator_observation_20261011"],
        observation(),
        "the classified value is never touched"
    );

    let events = jobs.recorded_events();
    let classified: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "jobs.job.input_classified")
        .collect();
    assert_eq!(classified.len(), 1);
    assert_eq!(classified[0].payload["job_id"], ID);
    assert!(
        events.iter().any(|e| e.kind == "jobs.job.updated"
            && e.payload["metadata"][CLASSIFICATIONS_KEY][0]["pointer"]
                == "/metadata/coordinator_observation_20261011"),
        "the row state the rebuild replays carries the list"
    );

    // And the packet read hands it to every reader.
    let (_, read) = send(&app, &admin, "GET", &format!("/api/jobs/{ID}"), None).await;
    assert_eq!(
        read["metadata"][CLASSIFICATIONS_KEY][0]["class"],
        "available_historical"
    );
}

#[tokio::test]
async fn without_publish_authority_nothing_is_classified() {
    let (app, jobs) = build_app();
    seed(&jobs).await;
    let tech = user("emp-tech", "service-tech");
    let (status, body) = send(
        &app,
        &tech,
        "POST",
        &uri(),
        Some(request(
            "/metadata/coordinator_observation_20261011",
            &observation(),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        jobs.recorded_events()
            .iter()
            .all(|e| e.kind != "jobs.job.input_classified")
    );
}

#[tokio::test]
async fn protected_material_and_a_value_no_longer_there_are_refused_by_name() {
    let (app, jobs) = build_app();
    seed(&jobs).await;
    let admin = user("emp-david", "platform-admin");
    for (pointer, value, words) in [
        (
            "/metadata/description",
            json!("the item's scope"),
            "never be classified below required",
        ),
        (
            "/metadata/coordinator_observation_20261011",
            json!("a different value"),
            "does not hash to the sha256 sent",
        ),
        (
            "/steps/build/metadata/procedure",
            json!("x"),
            "holds no value",
        ),
    ] {
        let (status, body) =
            send(&app, &admin, "POST", &uri(), Some(request(pointer, &value))).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{pointer}: {body}"
        );
        assert!(
            body["error"].as_str().unwrap_or("").contains(words),
            "{pointer}: {body}"
        );
    }
    assert!(
        jobs.recorded_events()
            .iter()
            .all(|e| e.kind != "jobs.job.input_classified")
    );
}

#[tokio::test]
async fn the_metadata_patch_cannot_forge_and_the_put_cannot_rewrite_the_list() {
    let (app, jobs) = build_app();
    seed(&jobs).await;
    let admin = user("emp-david", "platform-admin");
    let forged = json!({ CLASSIFICATIONS_KEY: [{ "job": ID,
        "pointer": "/metadata/description", "class": "available_historical" }] });
    let (status, body) = send(
        &app,
        &admin,
        "PATCH",
        &format!("/api/jobs/{ID}/metadata"),
        Some(forged.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body.as_str()
            .unwrap_or("")
            .contains("input-classifications")
    );

    let (status, _) = send(
        &app,
        &admin,
        "POST",
        &uri(),
        Some(request(
            "/metadata/coordinator_observation_20261011",
            &observation(),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, mut read) = send(&app, &admin, "GET", &format!("/api/jobs/{ID}"), None).await;
    let stored = read["metadata"][CLASSIFICATIONS_KEY].clone();
    read["metadata"][CLASSIFICATIONS_KEY] = forged[CLASSIFICATIONS_KEY].clone();
    let body = json!({
        "id": read["id"], "kind": read["kind"], "subject": read["subject"],
        "title": read["title"], "owner_id": read["owner_id"], "status": read["status"],
        "priority": read["priority"], "opened_on": read["opened_on"],
        "metadata": read["metadata"], "tags": read["tags"],
    });
    let (status, answer) = send(&app, &admin, "PUT", &format!("/api/jobs/{ID}"), Some(body)).await;
    assert!(status.is_success(), "{answer}");
    let after = jobs
        .get_job(&JobId::from_uuid(Uuid::parse_str(ID).unwrap()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.metadata[CLASSIFICATIONS_KEY], stored,
        "the stored list wins over the body"
    );
}

#[tokio::test]
async fn job_creation_cannot_admit_a_forged_input_classification() {
    for entries in [
        json!([{"job": ID, "pointer": "/metadata/history",
            "sha256": sha256_hex(&value_bytes(&json!("old observation"))),
            "class": "available_historical", "by": "emp-david",
            "at": "2026-10-24T11:00:00Z", "reason": "forged provenance"}]),
        json!([]),
        Value::Null,
    ] {
        let (app, jobs) = build_app();
        let creator = user("emp-tech", "service-tech");
        let mut job = Job::new(
            "backlog-item",
            Subject::new("custom", "/it/backlog"),
            "A caller-selected packet",
            "emp-tech",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 24).unwrap(),
        );
        job.id = JobId::from_uuid(Uuid::parse_str(ID).unwrap());
        job.metadata = json!({"history": "old observation", CLASSIFICATIONS_KEY: entries});
        let (status, body) = send(
            &app,
            &creator,
            "POST",
            "/api/jobs",
            Some(serde_json::to_value(&job).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.to_string().contains("input-classifications"), "{body}");
        assert!(
            jobs.get_job(&job.id).await.unwrap().is_none(),
            "refusal must precede admission"
        );
        assert!(
            jobs.recorded_events().is_empty(),
            "a refused create records neither admission nor forged classification"
        );
    }
}
