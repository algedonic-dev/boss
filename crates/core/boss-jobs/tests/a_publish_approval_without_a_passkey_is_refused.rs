//! Approving a publish to the PUBLIC GitHub mirror demands a passkey.
//!
//! THE DEFECT (backlog 02b65d81, David 2026-09-27: "Approved the
//! publish, but did not get a passkey check"). Measured on publish
//! 8d7a3507 (publish-to-github, registry v5): the `approve` step, kind
//! sign-off, completed at 13:46:58Z by emp-david with
//!
//! ```text
//!   sign_offs          = []
//!   sign_offs_required = []
//!   metadata           = { decision: approved }
//! ```
//!
//! and the forge then opened a pull request carrying 660 commits to a
//! public repository — a publication that cannot be taken back. An ops
//! approval in front of a disk verb needs a passkey-signed plan; this
//! one was a click, because the row declared neither half of the
//! requirement ops-request's approve declares.
//!
//! These tests drive the REAL router against the REAL platform bundle —
//! a fixture copy of the spec would keep passing while the shipped kind
//! stayed a click (the reason `backlog_item_build_can_refute` gives).
//! The refusal itself lives in `update_step` and is pinned for every
//! presence step by `an_assurance_holds_on_every_path`; what is pinned
//! HERE is that the publish approval is one of them, on the path it is
//! actually completed through, and that the tree the passkey signs can
//! reach the step before the ceremony.

use std::sync::Arc;

use async_trait::async_trait;
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

const PERSON: &str = "emp-bootstrap-admin";
const AGENT: &str = "agent-claude";
const SOURCE_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

struct AdminRoster;

#[async_trait]
impl RosterLookup for AdminRoster {
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

fn header(id: &str) -> String {
    json!({
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
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
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
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    as_id: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-boss-user", header(as_id));
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn open_publish(app: &axum::Router) -> String {
    let (status, job) = send(
        app,
        "POST",
        "/api/jobs",
        PERSON,
        Some(json!({
            "kind": "publish-to-github",
            "subject": { "subject_kind": "custom", "id": "github-mirror" },
            "title": "Publish to the public GitHub mirror",
            "owner_id": PERSON,
            "priority": "standard",
            "status": "open",
            "metadata": {},
            "tags": [],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    job["id"].as_str().expect("job id").to_string()
}

async fn read(app: &axum::Router, job_id: &str) -> Value {
    let (status, body) = send(app, "GET", &format!("/api/jobs/{job_id}"), PERSON, None).await;
    assert_eq!(status, StatusCode::OK, "read failed: {body}");
    body
}

fn step_of<'a>(job: &'a Value, slug: &str) -> &'a Value {
    job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("no step `{slug}` on the packet: {job:#?}"))
}

fn status_of(job: &Value, slug: &str) -> String {
    step_of(job, slug)["status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn step_uri(job_id: &str, job: &Value, slug: &str) -> String {
    format!(
        "/api/jobs/{job_id}/steps/{}",
        step_of(job, slug)["id"].as_str().expect("step id")
    )
}

/// Complete `slug` as `as_id`, writing `extra` first. Since e39a9d2a
/// (Stage 2's last car) the step PUT writes no metadata, so the keys go
/// through the step's merge door — only the keys named, a `null`
/// deleting one — and the completion is the status alone. A refused
/// merge is returned as the answer, so a caller sees the first refusal.
async fn try_complete(
    app: &axum::Router,
    job_id: &str,
    slug: &str,
    as_id: &str,
    extra: Value,
) -> (StatusCode, Value) {
    let job = read(app, job_id).await;
    let uri = step_uri(job_id, &job, slug);
    if extra.as_object().is_some_and(|keys| !keys.is_empty()) {
        let (status, body) =
            send(app, "PATCH", &format!("{uri}/metadata"), as_id, Some(extra)).await;
        if !status.is_success() {
            return (status, body);
        }
    }
    send(
        app,
        "PUT",
        &uri,
        as_id,
        Some(json!({ "status": "completed" })),
    )
    .await
}

async fn complete(app: &axum::Router, job_id: &str, slug: &str, as_id: &str, extra: Value) {
    let (status, body) = try_complete(app, job_id, slug, as_id, extra).await;
    assert!(
        status.is_success(),
        "completing `{slug}` as {as_id} failed {status}: {body}"
    );
}

fn measured() -> Value {
    json!({
        // The checklist kind's own required field (StepType registry).
        "items": [{ "label": "prep-github-publish.sh --json", "checked": true }],
        "commits_ahead": "660",
        "files_changed": "1319",
        "secrets_scan": "clean",
        "newly_public": "0",
        "has_drift": "true",
        "source_sha": SOURCE_SHA,
        "scanned_sha": SOURCE_SHA,
    })
}

/// A publish packet with measure and review done by the AGENT, the way
/// v8 hands them out — and the two shas written onto the approve step by
/// the same agent, through the merge door, before anyone signs.
async fn at_approve(app: &axum::Router) -> String {
    let job_id = open_publish(app).await;
    let job = read(app, &job_id).await;
    if matches!(status_of(&job, "opened").as_str(), "ready" | "active") {
        complete(app, &job_id, "opened", PERSON, json!({})).await;
    }
    complete(app, &job_id, "measure", AGENT, measured()).await;

    let job = read(app, &job_id).await;
    let (status, body) = send(
        app,
        "PATCH",
        &format!("{}/metadata", step_uri(&job_id, &job, "approve")),
        AGENT,
        Some(json!({ "source_sha": SOURCE_SHA, "scanned_sha": SOURCE_SHA })),
    )
    .await;
    assert!(
        status.is_success(),
        "the measuring agent must be able to put the measured tree on the approve step — \
         they are filer fields, context for the person — or the passkey can sign no tree: \
         {status} {body}"
    );

    complete(
        app,
        &job_id,
        "review",
        AGENT,
        json!({
            "items": [{ "label": "read each newly public file", "checked": true }],
            "newly_public_review": "publish: no newly public files",
        }),
    )
    .await;
    let job = read(app, &job_id).await;
    assert_eq!(
        status_of(&job, "approve"),
        "ready",
        "approve opens on review done: {job:#?}"
    );
    job_id
}

/// THE BUG, in the shape it was measured: the approver's own click —
/// decision approved, no passkey ceremony — must be refused, and the
/// step must stay open so nothing downstream opens the pull request.
#[tokio::test]
async fn a_publish_approval_without_a_passkey_is_refused() {
    let app = app();
    let job_id = at_approve(&app).await;

    let (status, body) = try_complete(
        &app,
        &job_id,
        "approve",
        PERSON,
        json!({ "decision": "approved" }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "approving a publish to the public mirror with no presence stamp must be refused \
         (02b65d81: 8d7a3507 completed with sign_offs []); body: {body}"
    );
    assert!(
        body.to_string().contains("assurance"),
        "the refusal must name what is missing: {body}"
    );
    let job = read(&app, &job_id).await;
    assert_eq!(
        status_of(&job, "approve"),
        "ready",
        "the refused write must not have completed approve"
    );
    assert_eq!(
        status_of(&job, "open-pr"),
        "pending",
        "and the machine step that publishes must not have opened"
    );

    // What the ceremony would sign is already on the step: the measured
    // tree, as the approve step's own keys.
    let md = &step_of(&job, "approve")["metadata"];
    assert_eq!(md["source_sha"], SOURCE_SHA, "{md}");
    assert_eq!(md["scanned_sha"], SOURCE_SHA, "{md}");
}

/// A SKIP is a completion to every predicate that reads `.done`, so it
/// must not walk past the approval either — or the bypass moves one word
/// over. (Two guards answer it today, the hand-skip refusal and the
/// assurance check; either is enough, so this asserts the refusal and
/// not which of them spoke.)
#[tokio::test]
async fn a_publish_approval_cannot_be_skipped_past_either() {
    let app = app();
    let job_id = at_approve(&app).await;
    let job = read(&app, &job_id).await;
    let (status, body) = send(
        &app,
        "PUT",
        &step_uri(&job_id, &job, "approve"),
        PERSON,
        Some(json!({ "status": "skipped" })),
    )
    .await;
    assert!(
        !status.is_success(),
        "skipping the publish approval is approving it by another name: {status} {body}"
    );
    assert_eq!(status_of(&read(&app, &job_id).await, "approve"), "ready");
}

/// The measurement must NAME the tree it measured (0677a618): a measure
/// completion without `source_sha` / `scanned_sha` is refused, so an
/// approval can never follow a measurement that vouched for no commit.
#[tokio::test]
async fn a_measurement_that_names_no_tree_is_refused() {
    let app = app();
    let job_id = open_publish(&app).await;
    let job = read(&app, &job_id).await;
    if matches!(status_of(&job, "opened").as_str(), "ready" | "active") {
        complete(&app, &job_id, "opened", PERSON, json!({})).await;
    }
    for missing in ["source_sha", "scanned_sha"] {
        // `null` deletes the key at the merge door: the attempt before
        // this one wrote it, and a completion must meet the step without
        // it, not with the value a refused attempt left behind.
        let mut md = measured();
        md[missing] = Value::Null;
        let (status, body) = try_complete(&app, &job_id, "measure", AGENT, md).await;
        assert!(
            !status.is_success(),
            "measure completed without `{missing}` — the scan's tree went unnamed: {body}"
        );
        assert!(
            body.to_string().contains(missing),
            "the refusal must name `{missing}`: {body}"
        );
    }
}
