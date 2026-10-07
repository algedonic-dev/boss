//! The tenant Workflow walk writes step metadata through the step merge
//! door, never through the step PUT (backlog e39a9d2a, design 93d2bddb,
//! Stage 2).
//!
//! `boss_jobs::bootstrap::publish_workflows` is what every tenant's
//! prepare (and `boss tenant publish`) runs: it opens a
//! `workflow-design` packet per kind and walks its steps to closure,
//! the terminal `workflow-publish` step landing the spec in the
//! registry. Until this car its `walk_step` read each step and PUT the
//! WHOLE metadata back beside the status (a read-merge-write): correct
//! under the Stage 1 rule, which refuses only a body that OMITS a stored
//! key, but a lost update against any concurrent writer and refused by
//! the decided end state, where the step PUT refuses ANY metadata body.
//!
//! So this test drives the REAL walk (blocking reqwest, as prepare runs
//! it) against the REAL router and platform bundle, served on a socket,
//! behind one layer that applies that end state: a step PUT carrying a
//! `metadata` key is refused 409. The walk must still publish, and the
//! record the walk leaves must be what it was — the approval's truthful
//! decision and signer, the validate step's synthesized required field,
//! and the materialized `authority_role` it never needed to re-send.
//! Once the tighten car moves the refusal into `update_step`, the layer
//! is redundant and this stays true without it.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use serde_json::Value;

const KIND: &str = "merge-door-walk-probe";

const SEEDS: &str = r#"
[[workflow]]
kind = "merge-door-walk-probe"
label = "Merge door walk probe"
category = "platform"
subject_kinds = ["custom"]
description = "One step, one terminal: the smallest kind the walk can publish."

[[workflow.step]]
title = "do"
kind = "task"
ready_when = "true"
title_template = "Do it"
terminal = { outcome = "done" }
"#;

/// The decided end state of design 93d2bddb, as a layer: a PUT to a
/// step (not its `/metadata` merge door, not its `/sign-offs`) whose
/// body carries `metadata` is refused. Everything else passes through
/// to the real router untouched.
async fn refuse_a_step_put_carrying_metadata(req: Request, next: Next) -> Response {
    let segments: Vec<&str> = req.uri().path().trim_matches('/').split('/').collect();
    let is_step_put = req.method() == axum::http::Method::PUT
        && matches!(segments.as_slice(), ["api", "jobs", _, "steps", _]);
    if !is_step_put {
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .expect("buffer step PUT body");
    let carries_metadata = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| v.get("metadata").cloned())
        .is_some();
    if carries_metadata {
        return (
            StatusCode::CONFLICT,
            format!(
                "END-STATE REFUSAL: step PUT {} carried a metadata body; write it through \
                 PATCH .../steps/{{step_id}}/metadata and PUT the status alone",
                parts.uri.path()
            ),
        )
            .into_response();
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

async fn serve() -> (String, Arc<InMemoryWorkflows>) {
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
                Resource::job(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Read,
                Resource::workflow(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::SignOff,
                Resource::new("step-signoff:workflow-approver"),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds.clone() as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            jobs,
            bus.clone(),
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let app = router(state).layer(axum::middleware::from_fn(
        refuse_a_step_put_carrying_metadata,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{addr}"), kinds)
}

fn get(base: &str, path: &str) -> Value {
    let header = serde_json::json!({
        "id": "automation:bootstrap", "role": "platform-admin", "access_tier": "operator",
        "territory_account_ids": [], "direct_report_ids": [], "department": "platform",
    })
    .to_string();
    let resp = reqwest::blocking::Client::new()
        .get(format!("{base}{path}"))
        .header("x-boss-user", header)
        .send()
        .expect("GET");
    assert!(resp.status().is_success(), "GET {path} → {}", resp.status());
    resp.json().expect("json")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_walk_publishes_with_no_step_put_carrying_metadata() {
    let (base, kinds) = serve().await;
    let dir = boss_testing::scratch_dir("tenant-walk-merge-door");
    let seeds = dir.join("workflows.toml");
    boss_testing::write_file(&seeds, SEEDS);

    let walk_base = base.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        // A FIXED source holding no token, never the process's live one:
        // a failing test prints the request heads it captured, and a
        // mounted Secret must not reach a gate log that way (backlog
        // 2ee29275, F2).
        let client = boss_core::machine_token::BlockingClient::build_with_source(
            reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(30)),
            Arc::new(boss_core::machine_token::Source::fixed(None)),
        )
        .expect("a blocking client");
        boss_jobs::bootstrap::publish_workflows(
            &client, &walk_base, &seeds, "platform", true, false, None,
        )
    })
    .await
    .expect("walk thread");
    let outcome = outcome.unwrap_or_else(|e| panic!("the walk must publish: {e:#}"));
    assert_eq!(outcome.published, [KIND]);

    // The registry holds the spec, authored by the packet the walk made.
    let live = kinds.get_active(KIND).await.expect("published");
    let design_id = live
        .authoring_job_id
        .expect("published by a workflow-design packet");

    // The record the walk left, read back from the router.
    let (base_for_read, id) = (base.clone(), design_id.to_string());
    let steps =
        tokio::task::spawn_blocking(move || get(&base_for_read, &format!("/api/jobs/{id}/steps")))
            .await
            .expect("read thread");
    let steps = steps.as_array().expect("steps list");
    let step = |slug: &str| -> &Value {
        steps
            .iter()
            .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
            .unwrap_or_else(|| panic!("step `{slug}` on the design packet: {steps:#?}"))
    };
    let md = |slug: &str, key: &str| step(slug)["metadata"].get(key).cloned();

    assert_eq!(step("publish")["status"], "completed");
    assert_eq!(step("not-published")["status"], "skipped");
    assert_eq!(
        md("approve", "decision"),
        Some(Value::from("approved")),
        "the walk IS the approval, and says so"
    );
    assert_eq!(
        md("approve", "signed_by"),
        Some(Value::from("automation:bootstrap"))
    );
    assert_eq!(
        md("approve", "authority_role"),
        Some(Value::from("workflow-approver")),
        "the materialized key survives a walk that never re-sends it"
    );
    assert!(
        md("validate", "sign_off_context").is_some(),
        "the live-required field is synthesized: {:#?}",
        step("validate")
    );
    assert_eq!(
        md("publish", "workflow_spec").and_then(|s| s.get("kind").cloned()),
        Some(Value::from(KIND))
    );
}
