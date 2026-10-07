//! `/api/jobs/assignments` carries each step kind's completion
//! contract, so My Day can separate what needs a person from what
//! does not.
//!
//! David, 2026-08-16: *"it probably makes sense to have a special
//! separation between jobs that are in a queue with a human-only
//! policy with jobs that agents are also eligible for as a practical
//! consideration"* — with the reason it matters: *"We intentionally do
//! not want many protocols where policy requires a human because that
//! is slow."*
//!
//! The separation needs no new concept. `StepType::completion` is
//! already the closed axis of the step alphabet, and it is already
//! registry data. The only thing missing was carrying it on the row,
//! and the alternative — letting the frontend keep a list of which
//! kinds are agent-completed — is CLAUDE.md §9a's fact living twice,
//! in the language least able to notice when the registry moves.

use boss_policy_client::types::{AccessTier, User};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

// SERIALISE THE REAL TYPE, never a copy of its wire shape. A test
// that hand-builds the header is testing a copy: if a field loses its
// serde default or a new required one lands, a hand-built payload keeps
// passing here while production rejects it — the failure surfaces as a
// live 4xx instead of a red test, which is the wrong way round
// (7c3649e2).
fn user_header() -> String {
    serde_json::to_string(&User {
        id: "emp-ceo".to_string(),
        role: "ceo".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

/// One protocol carrying all three interesting contracts at once:
/// `sign-off` is human, `demand-gate` is agent, and `holler` is a kind
/// no registry knows.
fn mixed_kind() -> WorkflowSpec {
    let step = |title: &str, kind: &str| StepSpec {
        title: title.into(),
        kind: kind.into(),
        ready_when: "true".into(),
        title_template: format!("{title} it"),
        authority_role: Some("bookkeeper".into()),
        ..Default::default()
    };
    WorkflowSpec::platform_seed(
        "mixed",
        "Mixed contracts",
        "test",
        vec!["custom".into()],
        vec![
            step("approve", "sign-off"),
            step("gate", "demand-gate"),
            step("holler", "not-a-registered-kind"),
        ],
    )
}

/// Three `checklist` steps — one kind, one `completion` (human) — that
/// differ ONLY in what the step itself declares: an agent block with
/// `human_only = false` (the publish review step of 246d597a), an
/// agent block the protocol still marks human-only, and no block.
fn agent_blocks() -> WorkflowSpec {
    let step = |title: &str, agent: bool, human_only: Option<serde_json::Value>| StepSpec {
        title: title.into(),
        kind: "checklist".into(),
        ready_when: "true".into(),
        title_template: format!("{title} it"),
        authority_role: Some("platform-admin".into()),
        agent: agent.then(|| boss_jobs::agent_spec::AgentSpec {
            executor_provenance: Default::default(),
            profile: "analyst".into(),
            model: "opus-5[1m]".into(),
            budget_usd: 2.0,
            effort: boss_jobs::agent_spec::Effort::Medium,
        }),
        metadata_defaults: human_only
            .map(|h| serde_json::json!({ "human_only": h }))
            .unwrap_or(serde_json::Value::Null),
        ..Default::default()
    };
    WorkflowSpec::platform_seed(
        "agent-blocks",
        "Agent blocks",
        "test",
        vec!["custom".into()],
        vec![
            step("review", true, Some(serde_json::json!("False"))),
            step("sign", true, Some(serde_json::json!(true))),
            step("tick", false, None),
        ],
    )
}

fn app() -> axum::Router {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(mixed_kind()).expect("seed");
    kinds.seed(agent_blocks()).expect("seed");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    router(JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

async fn open_packet(app: &axum::Router) {
    open_packet_of(app, "mixed").await;
}

async fn open_packet_of(app: &axum::Router, kind: &str) {
    let body = serde_json::json!({
        "kind": kind,
        "subject": { "subject_kind": "custom", "id": "s1" },
        "title": "a mixed packet",
        "owner_id": "emp-ceo",
        "status": "open",
        "priority": "standard",
        "opened_on": "2026-08-16",
        "metadata": {},
        "tags": [],
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header())
                .body(Body::from(serde_json::to_vec(&body).expect("json")))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}

/// step kind -> the `completion` the endpoint reported.
async fn completions_by_kind(app: &axum::Router) -> Vec<(String, serde_json::Value)> {
    let resp = app
        .clone()
        .oneshot(
            Request::get("/api/jobs/assignments?roles=bookkeeper")
                .header("x-boss-user", user_header())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    v["data"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|r| {
            (
                r["step"]["kind"].as_str().unwrap_or_default().to_string(),
                r["step"]["completion"].clone(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_row_carries_the_step_kinds_completion_contract() {
    let app = app();
    open_packet(&app).await;
    let rows = completions_by_kind(&app).await;
    assert!(!rows.is_empty(), "the role query returned no steps");

    let find = |kind: &str| {
        rows.iter()
            .find(|(k, _)| k == kind)
            .unwrap_or_else(|| panic!("no `{kind}` row in {rows:#?}"))
            .1
            .clone()
    };

    // A sign-off is a person's job — this is the row that belongs in
    // "up for grabs".
    assert_eq!(find("sign-off"), serde_json::json!("human"));

    // A gate is not. The dispatcher executes it on `step.ready` and
    // the human workforce never pulls it, so offering it to somebody
    // to claim is how a protocol silently becomes manual.
    assert_eq!(find("demand-gate"), serde_json::json!("agent"));
}

/// An unregistered kind reports `null`, not a guess.
///
/// A tenant protocol can name a step kind this deployment has not
/// registered. Defaulting it to `"human"` on the wire would be the
/// server inventing a contract it does not have — the reader is free
/// to TREAT unknown as human (My Day does, deliberately), but that is
/// the reader's safe choice and not a fact the API should assert.
#[tokio::test]
async fn an_unregistered_kind_reports_null_rather_than_guessing() {
    let app = app();
    open_packet(&app).await;
    let rows = completions_by_kind(&app).await;
    let unknown = rows
        .iter()
        .find(|(k, _)| k == "not-a-registered-kind")
        .expect("the unregistered step should still be listed");
    assert_eq!(
        unknown.1,
        serde_json::Value::Null,
        "an unknown kind must not be reported as a known contract"
    );
}

/// Every row carries the field, present or null.
///
/// A reader that has to handle "sometimes absent" ends up with a
/// default in two places. One shape on the wire, always.
#[tokio::test]
async fn every_row_carries_the_field() {
    let app = app();
    open_packet(&app).await;
    let rows = completions_by_kind(&app).await;
    assert_eq!(rows.len(), 3, "expected all three steps: {rows:#?}");
    for (kind, completion) in &rows {
        assert!(
            completion.is_string() || completion.is_null(),
            "`{kind}` reported {completion:?} — expected a string or null"
        );
    }
}

/// The row says when the STEP'S OWN agent block hands it to an agent
/// (backlog 1dd6d7ad). David, 2026-09-27: the publish review step of
/// 246d597a — kind `checklist`, so `completion = human` by kind, but
/// carrying an agent block and `human_only = false` — sat in his queue,
/// and he opened it expecting it to be his: "I thought you were ready
/// for me because it was in my backlog." The kind's contract cannot
/// tell those apart, because all three steps below share one kind; only
/// the step's own declaration can.
#[tokio::test]
async fn the_row_says_when_the_steps_own_agent_block_hands_it_to_an_agent() {
    let app = app();
    open_packet_of(&app, "agent-blocks").await;
    let resp = app
        .clone()
        .oneshot(
            Request::get("/api/jobs/assignments?roles=platform-admin")
                .header("x-boss-user", user_header())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let rows = v["data"].as_array().expect("data array");
    let takes = |title: &str| {
        rows.iter()
            .find(|r| r["step"]["title"] == format!("{title} it"))
            .unwrap_or_else(|| panic!("no `{title}` row in {rows:#?}"))["step"]["agent_takes"]
            .clone()
    };
    // An agent block and `human_only = false` (the live spelling,
    // "False"): an agent takes it, whatever the kind's contract says.
    assert_eq!(takes("review"), serde_json::json!(true));
    // The protocol still requires a person: the block does not win.
    assert_eq!(takes("sign"), serde_json::json!(false));
    // No block: a person's, as before — one shape on the wire.
    assert_eq!(takes("tick"), serde_json::json!(false));
}
