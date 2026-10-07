//! A step PUT that carries `metadata` is refused, and the refusal
//! routes to the merge door (backlog e39a9d2a; design 93d2bddb, the
//! decided end state; Stage 2's last car).
//!
//! `PUT /api/jobs/{id}/steps/{step_id}` overlays the body onto the
//! stored step, so a body `metadata` REPLACES the stored metadata
//! wholesale. Three keys were carved out of that replace, each after
//! someone lost it in production — `authority_role`, `human_only` and
//! `agent_run` (b91a2103: a completer that sent `metadata` without
//! merging erased the run edge, and the run died four hours later on
//! the silence clock as though the agent had gone quiet). The list grew
//! by incident.
//!
//! Stage 1 refused a body that OMITTED a stored key, which removed the
//! silent wipe but left the PUT a second metadata writer — a
//! read-merge-write that raced a concurrent key was caught, not
//! avoided. Every writer in the tree now sends its keys through
//! `PATCH .../steps/{id}/metadata` and PUTs the status alone, so the
//! PUT refuses ANY metadata body, 409, naming this step's merge door:
//! one writer of step metadata, and the class is gone rather than
//! guarded. A status-only PUT keeps working; a terminal step whose
//! metadata a body would CHANGE still answers with the record's own
//! refusal, which names the doors that work on a record.

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
use tower::ServiceExt;

const ADMIN_ID: &str = "emp-bootstrap-admin";
const RUN_A: &str = "5b1d2c3e-0000-4000-8000-00000000000a";
const RUN_B: &str = "5b1d2c3e-0000-4000-8000-00000000000b";

struct AdminRoster;

#[async_trait::async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec![ADMIN_ID.to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == ADMIN_ID)
    }
}

fn app() -> axum::Router {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
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
            jobs.clone(),
            bus.clone(),
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

const ADMIN: &str = r#"{"id":"emp-bootstrap-admin","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
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

fn req(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", ADMIN)
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// A ship-a-change packet and its ready `scope` step — the shape a
/// dispatched run claims.
async fn open_job(app: &axum::Router) -> (String, String) {
    let (status, job) = send(
        app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": "ship-a-change",
                "subject": {"subject_kind": "custom", "id": "feat/x"},
                "title": "t", "owner_id": ADMIN_ID,
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create: {status} {job}");
    let id = job["id"].as_str().expect("job id").to_string();
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    let step = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == "scope")
        .expect("the scope step")
        .clone();
    assert_eq!(step["status"], "ready", "precondition: scope is ready");
    (id, step["id"].as_str().expect("step id").to_string())
}

/// The step's stored metadata, read back the way every consumer reads
/// it — through the API, not the adapter.
async fn stored_metadata(app: &axum::Router, job: &str, step: &str) -> serde_json::Value {
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{job}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["id"] == step)
        .expect("the step")["metadata"]
        .clone()
}

/// The edge as `boss dispatch` writes it: the merge door, one key.
async fn write_edge(app: &axum::Router, job: &str, step: &str, run: &str) {
    let (status, body) = send(
        app,
        req(
            "PATCH",
            &format!("/api/jobs/{job}/steps/{step}/metadata"),
            serde_json::json!({ boss_jobs::agent_runs::EDGE_KEY: run }),
        ),
    )
    .await;
    assert!(status.is_success(), "edge patch: {status} {body}");
}

async fn put_step(
    app: &axum::Router,
    job: &str,
    step: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    send(
        app,
        req("PUT", &format!("/api/jobs/{job}/steps/{step}"), body),
    )
    .await
}

/// The step's whole row, read back through the API.
async fn stored_row(app: &axum::Router, job: &str, step: &str) -> serde_json::Value {
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{job}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["id"] == step)
        .expect("the step")
        .clone()
}

/// Asserts the refusal's shape: 409, this step's merge door named,
/// the one hint every double of this API also answers with.
fn assert_routed_to_the_merge_door(
    status: StatusCode,
    body: &serde_json::Value,
    job: &str,
    step: &str,
) {
    assert_eq!(status, StatusCode::CONFLICT, "refused: {body}");
    assert_eq!(
        body["merge_door"],
        format!("/api/jobs/{job}/steps/{step}/metadata"),
        "names THIS step's merge door, ready to call: {body}",
    );
    assert_eq!(
        body["hint"],
        boss_jobs::step_metadata_write::METADATA_BODY_HINT,
        "in the one sentence every double of this API answers with: {body}",
    );
}

/// THE DEFECT CLASS, refused. What a hand-built completion sends — the
/// keys the caller cares about beside the status — used to erase the
/// run edge. It is refused, loudly, and nothing is written: neither the
/// keys nor the status flip.
#[tokio::test]
async fn a_put_carrying_metadata_is_refused_and_writes_nothing() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;
    let before = stored_row(&app, &job, &step).await;

    let (status, body) = put_step(
        &app,
        &job,
        &step,
        serde_json::json!({
            "status": "completed",
            "metadata": {
                "summary": "what the change does",
                "excludes": "what it deliberately leaves alone",
            },
        }),
    )
    .await;
    assert_routed_to_the_merge_door(status, &body, &job, &step);
    assert_eq!(
        stored_row(&app, &job, &step).await,
        before,
        "a refused write writes nothing"
    );
}

/// THE TIGHTEN. Under Stage 1 a read-merge-write — every stored key
/// sent back — landed. It is refused now too: the PUT is no longer a
/// metadata writer at all, so a body that merely re-sends the stored
/// metadata is routed to the door like any other.
#[tokio::test]
async fn a_read_merge_write_put_is_refused_too() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;
    let before = stored_row(&app, &job, &step).await;

    let mut md = stored_metadata(&app, &job, &step).await;
    md["summary"] = serde_json::json!("what the change does");
    let (status, body) = put_step(&app, &job, &step, serde_json::json!({ "metadata": md })).await;
    assert_routed_to_the_merge_door(status, &body, &job, &step);

    let unchanged = stored_metadata(&app, &job, &step).await;
    let (status, body) = put_step(
        &app,
        &job,
        &step,
        serde_json::json!({ "status": "ready", "metadata": unchanged }),
    )
    .await;
    assert_routed_to_the_merge_door(status, &body, &job, &step);
    assert_eq!(stored_row(&app, &job, &step).await, before);
}

/// The required form: the keys through the merge door, then the status
/// alone. The edge survives because nobody sent metadata that could
/// replace it.
#[tokio::test]
async fn the_merge_door_then_a_status_only_put_completes_the_step() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;

    let row = complete_scope(&app, &job, &step).await;
    assert_eq!(row["status"], "completed", "{row}");
    assert_eq!(row["metadata"]["summary"], "what the change does", "{row}");
    assert_eq!(
        row["metadata"].get(boss_jobs::agent_runs::EDGE_KEY),
        Some(&serde_json::json!(RUN_A)),
        "{row}"
    );
}

/// A PUT with no `metadata` key at all — a status-only flip — touches
/// no metadata and is not judged by this rule.
#[tokio::test]
async fn a_put_without_metadata_is_not_judged() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;
    let before = stored_metadata(&app, &job, &step).await;

    // `ready`, not `active`: a step becomes Active only through the
    // claim door, which a PUT to `active` is refused naming (backlog
    // 6ef4a36b) — and a claim is not a PUT, so it is not what this pins.
    let (status, body) =
        put_step(&app, &job, &step, serde_json::json!({ "status": "ready" })).await;
    assert!(status.is_success(), "status-only PUT: {status} {body}");
    assert_eq!(stored_metadata(&app, &job, &step).await, before);
}

/// The door the refusal names does what it says. A step re-claimed by a
/// different run must name the run that now holds it, and `boss
/// dispatch` writes that through this door right after the claim; an
/// explicit `null` is how a key is cleared on purpose.
#[tokio::test]
async fn the_merge_door_still_overwrites_and_clears_the_run_edge() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;

    // A re-claim: the next dispatch names its own run.
    write_edge(&app, &job, &step, RUN_B).await;
    let stored = stored_metadata(&app, &job, &step).await;
    assert_eq!(
        stored.get(boss_jobs::agent_runs::EDGE_KEY),
        Some(&serde_json::json!(RUN_B)),
        "no stale run id is pinned onto a step a new run holds: {stored}",
    );

    let (status, body) = send(
        &app,
        req(
            "PATCH",
            &format!("/api/jobs/{job}/steps/{step}/metadata"),
            serde_json::json!({ boss_jobs::agent_runs::EDGE_KEY: serde_json::Value::Null }),
        ),
    )
    .await;
    assert!(status.is_success(), "clearing patch: {status} {body}");
    let stored = stored_metadata(&app, &job, &step).await;
    assert!(
        stored.get(boss_jobs::agent_runs::EDGE_KEY).is_none(),
        "an explicit null still deletes the edge: {stored}",
    );
}

/// Complete the scope step the required way — read, merge, write — and
/// Complete the scope step the required way — its keys through the
/// merge door, then the status alone — and hand back the stored row.
async fn complete_scope(app: &axum::Router, job: &str, step: &str) -> serde_json::Value {
    let (status, body) = send(
        app,
        req(
            "PATCH",
            &format!("/api/jobs/{job}/steps/{step}/metadata"),
            serde_json::json!({
                "summary": "what the change does",
                "excludes": "what it leaves alone",
            }),
        ),
    )
    .await;
    assert!(status.is_success(), "scope's keys: {status} {body}");
    let (status, body) = put_step(app, job, step, serde_json::json!({"status": "completed"})).await;
    assert!(status.is_success(), "completing scope: {status} {body}");
    stored_row(app, job, step).await
}

/// A TERMINAL step answers a PUT that would CHANGE its metadata with
/// the terminal refusal, not this one. The merge door refuses a real
/// change to a terminal step too, so routing the caller there would
/// send it from one 409 to another; the terminal hint names the doors
/// that work on a record (the corrections door, the job's own metadata).
#[tokio::test]
async fn a_terminal_step_answers_a_changing_put_with_the_terminal_refusal() {
    let app = app();
    let (job, step) = open_job(&app).await;
    let before = complete_scope(&app, &job, &step).await;

    let (status, body) = put_step(
        &app,
        &job,
        &step,
        serde_json::json!({"status": "completed", "metadata": {"summary": "rewritten"}}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["hint"],
        boss_jobs::corrections::TERMINAL_STEP_HINT,
        "the record's own refusal speaks: {body}"
    );
    assert_eq!(stored_row(&app, &job, &step).await, before);
}

/// And an UNCHANGED metadata re-send to a terminal step is routed to
/// the merge door like any other metadata body — the door answers that
/// re-send 204 (below), so the route works, and the PUT stays out of
/// the metadata business on every status.
#[tokio::test]
async fn a_terminal_step_routes_an_unchanged_metadata_resend_to_the_merge_door() {
    let app = app();
    let (job, step) = open_job(&app).await;
    let before = complete_scope(&app, &job, &step).await;

    let (status, body) = put_step(
        &app,
        &job,
        &step,
        serde_json::json!({"status": "completed", "metadata": before["metadata"].clone()}),
    )
    .await;
    assert_routed_to_the_merge_door(status, &body, &job, &step);
    assert_eq!(stored_row(&app, &job, &step).await, before);

    let (status, body) = put_step(
        &app,
        &job,
        &step,
        serde_json::json!({"status": "completed"}),
    )
    .await;
    assert!(
        status.is_success(),
        "a status-only re-send stays the no-op the freeze allows: {status} {body}"
    );
}

/// THE IDEMPOTENT RE-SEND, AT THE MERGE DOOR. The writers moved onto
/// the merge door (e39a9d2a: the gate verdict, auto-park, boss park,
/// boss prove, boss design, the triage on park) re-send on a
/// redelivery or a retry. The PUT they used to send answered an
/// unchanged re-send to a terminal step with success — the freeze is
/// scoped to a real change — so the door they moved to must answer it
/// the same way, or a redelivery that changes nothing turns into a 409.
/// A re-send that DOES change a key is still refused, and writes
/// nothing.
#[tokio::test]
async fn the_merge_door_answers_an_unchanged_resend_to_a_terminal_step_with_success() {
    let app = app();
    let (job, step) = open_job(&app).await;
    let before = complete_scope(&app, &job, &step).await;
    let door = format!("/api/jobs/{job}/steps/{step}/metadata");

    let (status, body) = send(
        &app,
        req(
            "PATCH",
            &door,
            serde_json::json!({"summary": "what the change does", "never_set": null}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an unchanged re-send lands as the no-op it is: {body}"
    );

    let (status, body) = send(
        &app,
        req("PATCH", &door, serde_json::json!({"summary": "rewritten"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a real change is refused: {body}"
    );
    assert_eq!(body["hint"], boss_jobs::corrections::TERMINAL_STEP_HINT);
    assert_eq!(stored_row(&app, &job, &step).await, before);
}
