//! A packet records who filed it — whatever the door (backlog 958edca6).
//!
//! Measured 2026-09-27: all 984 ops-requests closed in two days read
//! owner `emp-david`, including the ones `agent-claude` filed and the
//! ones a dispatcher rule spawned. Nothing was lost in transit: admission
//! resolves an automation- or agent-shaped owner to a responsible HUMAN
//! (subject-model Q7, `owner_resolution.rs`) — correct, because the owner
//! is who is accountable for the packet (the Self/Team policy scope, the
//! `notify_on_done` wait-is-over signal, the terminal notification) — and
//! then the filer survived only as the create event's actor. Read off the
//! packet, a human was credited with an agent's filing.
//!
//! So admission stamps `metadata.opened_by` itself, from the actor that
//! SIGNED the create — the same actor the `jobs.job.created` event names —
//! for every packet, through every door. What is only visible here:
//!
//!   1. Filed as `agent-claude`, the packet reads back `opened_by:
//!      agent-claude` while its owner is still resolved to a human.
//!   2. Filed by a rule (the dispatcher signs `rule:<name>`), it reads back
//!      the rule, spelled as the log spells it.
//!   3. A body naming a different filer is refused, and nothing lands.
//!   4. A body naming the signer (the shape `boss job file` sends) is
//!      admitted.
//!   5. Neither the job PUT nor the metadata PATCH can move it afterwards.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The one person on the roster, and the one `platform-admin` holder — so
/// every agent- or rule-filed ops-request resolves its OWNER to him, the
/// shape measured live.
const PERSON: &str = "emp-david";

struct OnePersonRoster;

#[async_trait::async_trait]
impl RosterLookup for OnePersonRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec![PERSON.to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == PERSON)
    }
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
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
                Resource::job(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(OnePersonRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

/// The `x-boss-user` a caller signs with — the platform-admin pair every
/// door that files an ops-request presents (the dispatcher's is built by
/// `dispatcher_actor_header`, with `rule:<name>` as the id).
fn signed_as(id: &str) -> String {
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

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    signer: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", signed_as(signer))
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string())
        })
    };
    (status, json)
}

/// The body `boss ops` sends, owner = the signer (ops_request.rs), with
/// whatever metadata the case needs.
fn ops_request(owner: &str, metadata: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "kind": "ops-request",
        "subject": {"subject_kind": "custom", "id": "boss-gcp"},
        "title": "reclaim the build cache",
        "owner_id": owner,
        "status": "open",
        "priority": "standard",
        "tags": [],
        "metadata": metadata,
    })
}

/// Files the body as `signer` and reads the packet back as the person —
/// the read is the fact, the 201 only a claim.
async fn file(app: &axum::Router, signer: &str, body: serde_json::Value) -> serde_json::Value {
    let (status, created) = send(app, "POST", "/api/jobs", signer, body).await;
    assert_eq!(status, StatusCode::CREATED, "filing as {signer}: {created}");
    let id = created["id"].as_str().expect("created id").to_string();
    read(app, &id).await
}

async fn read(app: &axum::Router, id: &str) -> serde_json::Value {
    let (status, job) = send(
        app,
        "GET",
        &format!("/api/jobs/{id}"),
        PERSON,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reading {id} back: {job}");
    job
}

/// The actor the packet's own `jobs.job.created` event names.
async fn created_event_actor(app: &axum::Router, id: &str) -> serde_json::Value {
    let (status, events) = send(
        app,
        "GET",
        &format!("/api/jobs/{id}/events"),
        PERSON,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "events read: {events}");
    events["data"]
        .as_array()
        .expect("data is an array")
        .iter()
        .find(|r| r["kind"] == "jobs.job.created")
        .unwrap_or_else(|| panic!("the admission is in the packet's history: {events}"))["actor"]
        .clone()
}

#[tokio::test]
async fn filed_as_an_agent_it_reads_back_the_agent_while_a_person_owns_it() {
    let (app, _) = app();
    let job = file(
        &app,
        "agent-claude",
        ops_request("agent-claude", serde_json::json!({})),
    )
    .await;
    assert_eq!(
        job["metadata"]["opened_by"], "agent-claude",
        "the packet names the agent that filed it: {job}"
    );
    // Q7 still holds: the accountable owner is a person, and that is a
    // different fact from who filed it.
    assert_eq!(job["owner_id"], PERSON, "{job}");
    let id = job["id"].as_str().expect("id");
    assert_eq!(
        created_event_actor(&app, id).await,
        job["metadata"]["opened_by"],
        "the packet and its admission event name one filer"
    );
}

#[tokio::test]
async fn filed_by_a_rule_it_reads_back_the_rule() {
    let (app, _) = app();
    // The dispatcher's spawn: signed `rule:<name>`, owner the same id.
    let rule = "rule:ops-judge-files-a-reclaim";
    let job = file(&app, rule, ops_request(rule, serde_json::json!({}))).await;
    assert_eq!(
        job["metadata"]["opened_by"], "automation:rule:ops-judge-files-a-reclaim",
        "the packet names the rule that spawned it, as the log spells it: {job}"
    );
    assert_eq!(job["owner_id"], PERSON, "{job}");
    let id = job["id"].as_str().expect("id");
    assert_eq!(
        created_event_actor(&app, id).await,
        job["metadata"]["opened_by"],
        "the packet and its admission event name one filer"
    );
}

#[tokio::test]
async fn filed_by_a_person_it_reads_back_the_person() {
    let (app, _) = app();
    let job = file(&app, PERSON, ops_request(PERSON, serde_json::json!({}))).await;
    assert_eq!(job["metadata"]["opened_by"], PERSON, "{job}");
}

#[tokio::test]
async fn a_body_naming_another_filer_is_refused_and_nothing_lands() {
    let (app, jobs) = app();
    let (status, body) = send(
        &app,
        "POST",
        "/api/jobs",
        "agent-claude",
        ops_request("agent-claude", serde_json::json!({"opened_by": PERSON})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.to_string().contains("opened_by"),
        "the refusal names the key: {body}"
    );
    let (all, _) = jobs
        .list_jobs(&Default::default(), 50, 0)
        .await
        .expect("list");
    assert!(all.is_empty(), "a refused filing lands no packet");
}

#[tokio::test]
async fn a_body_naming_its_own_signer_is_admitted() {
    // `boss job file` sends the filer it signs as (item_source.rs).
    let (app, _) = app();
    let job = file(
        &app,
        "agent-claude",
        ops_request(
            "agent-claude",
            serde_json::json!({"opened_by": "agent-claude"}),
        ),
    )
    .await;
    assert_eq!(job["metadata"]["opened_by"], "agent-claude", "{job}");
}

#[tokio::test]
async fn a_body_with_no_metadata_still_records_its_filer() {
    let (app, _) = app();
    let mut body = ops_request("agent-claude", serde_json::json!({}));
    body["metadata"] = serde_json::Value::Null;
    let job = file(&app, "agent-claude", body).await;
    assert_eq!(job["metadata"]["opened_by"], "agent-claude", "{job}");
}

#[tokio::test]
async fn a_later_put_cannot_move_who_filed_it() {
    let (app, _) = app();
    let job = file(
        &app,
        "agent-claude",
        ops_request("agent-claude", serde_json::json!({})),
    )
    .await;
    let id = job["id"].as_str().expect("id").to_string();

    // A full body that rewrites the key, and one that drops it.
    for metadata in [
        serde_json::json!({"opened_by": PERSON}),
        serde_json::json!({}),
    ] {
        let mut put = job.clone();
        put["metadata"] = metadata;
        let (status, body) = send(&app, "PUT", &format!("/api/jobs/{id}"), PERSON, put).await;
        assert!(
            status.is_success(),
            "the PUT itself is admitted: {status} {body}"
        );
        let after = read(&app, &id).await;
        assert_eq!(
            after["metadata"]["opened_by"], "agent-claude",
            "the stored filer wins over the body: {after}"
        );
    }
}

#[tokio::test]
async fn the_metadata_patch_refuses_the_key() {
    let (app, _) = app();
    let job = file(
        &app,
        "agent-claude",
        ops_request("agent-claude", serde_json::json!({})),
    )
    .await;
    let id = job["id"].as_str().expect("id").to_string();
    for value in [serde_json::json!(PERSON), serde_json::Value::Null] {
        let (status, body) = send(
            &app,
            "PATCH",
            &format!("/api/jobs/{id}/metadata"),
            PERSON,
            serde_json::json!({"opened_by": value}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            read(&app, &id).await["metadata"]["opened_by"],
            "agent-claude"
        );
    }
}
