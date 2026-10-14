//! A step `boss dispatch --launch` hands to another harness is claimed
//! FOR that harness's actor, and the launch's later writes still land
//! (review b149c579 of the Codex launcher car a63c6d3d, N2; backlog
//! 167a877c).
//!
//! THE DEFECT. The launcher files the run under the harness's actor
//! (`agent-codex`) but claimed the step it works on for ITSELF — a plain
//! claim, no `claimed_for` — so the claim door judged the COORDINATOR's
//! agents row: its concurrency cap and its hourly budget. Measured on the
//! live registry 2026-10-10: agent-claude `max_concurrent_runs` 12, hourly
//! $40; agent-codex 12, $10. A launched Codex run was capped by Claude's
//! row and priced against Claude's hour, and Codex's own cap counted the
//! run (filed under agent-codex) without ever being asked at a claim.
//!
//! THE FIX is the door that already existed (design 611fbffd): the
//! coordinator holds `step-assign` (platform-admin, live rule
//! `platform-admin:step-assign:update`) and claims `?claimed_for=` the
//! harness's actor. This file pins, through the real router, the three
//! writes the launch then makes onto a step it no longer holds:
//!
//! 1. The claim for the harness actor is judged on THAT actor's row: at
//!    its cap it is a 409 naming it, while the coordinator's own claim on
//!    the same packet would have been admitted (the shape the verb sent).
//!    Its over-budget reading names the harness actor.
//! 2. Admitted, the step is held by the harness actor and the marker
//!    names both (`claimed_by` the coordinator, `claimed_for` the actor).
//! 3. The coordinator's run-edge PATCH (`agent_run`) and its status-only
//!    completion both land on the step the harness actor holds — admitted
//!    by the coordinator's scope, as `boss dispatch` and the launcher's
//!    "complete the step it was dispatched for" both need.

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publish::PublishMode;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::agent_budget::BudgetDoor;
use boss_jobs::agent_runs::InMemoryAgentRuns;
use boss_jobs::agents::types::AgentInput;
use boss_jobs::agents::{AgentsRegistry, InMemoryAgents};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestRequest};
use chrono::NaiveDate;
use uuid::Uuid;

/// The coordinator, by the login `boss dispatch` signs with.
const COORDINATOR: &str = "claude@algedonic.dev";
const COORDINATOR_ID: &str = "agent-claude";
/// The harness actor a launched run is filed under.
const HARNESS: &str = "agent-codex";
const RUN: &str = "5b1d2c3e-0000-4000-8000-0000000000aa";

fn packet() -> Job {
    Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            "backlog-item",
            Subject::new("custom", "bosspipeline"),
            "A step launched on another harness",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 10).unwrap(),
        )
    }
}

/// An open run filed under `agent` — what the cap counts.
fn open_run(agent: &str) -> Job {
    Job {
        id: JobId::new(),
        status: JobStatus::Open,
        metadata: serde_json::json!({ "agent": agent }),
        ..Job::new(
            "agent-run",
            Subject::new("custom", "bosspipeline"),
            "run",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 10).unwrap(),
        )
    }
}

/// A ready step carrying an agent block whose model the harness runs.
fn launched_step(job: &Job, slug: &str) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::new_v4()),
        spec_slug: Some(slug.into()),
        status: StepStatus::Ready,
        metadata: serde_json::json!({
            "authority_role": "platform-admin",
            "agent_profile": "reviewer",
            "agent_model": "gpt-6.1-sol",
            "agent_budget_usd": 2.0,
            "agent_effort": "medium",
        }),
        ..Step::new(job.id, "task", slug, 1)
    }
}

/// The live registry's two rows, the harness's capped at `codex_cap`
/// runs and a $1 hour (under the step's $2 budget, so a claim on its
/// row reads over budget); the coordinator's uncapped.
async fn agents(codex_cap: i32) -> Arc<InMemoryAgents> {
    let agents = Arc::new(InMemoryAgents::new());
    let row =
        |id: &str, login: &str, model: &str, cap: Option<i32>, hour: Option<i64>| AgentInput {
            id: id.into(),
            display_name: id.into(),
            default_model: model.into(),
            aliases: vec![login.into()],
            role: Some("platform-admin".into()),
            department: None,
            hourly_budget_usd_micros: hour,
            max_concurrent_runs: cap,
        };
    agents
        .publish(
            &[
                row(COORDINATOR_ID, COORDINATOR, "opus-5-5", None, None),
                row(
                    HARNESS,
                    "codex@algedonic.dev",
                    "gpt-6.1-sol",
                    Some(codex_cap),
                    Some(1_000_000),
                ),
            ],
            PublishMode::Take,
            &EventStamp::new("jobs", ActorId::Automation("test".into())),
        )
        .await
        .expect("the rows land");
    agents
}

/// The router as the live policy grants platform-admin: step writes in
/// every scope, and `step-assign`.
fn app(jobs: Arc<InMemoryJobs>, agents: Arc<InMemoryAgents>) -> axum::Router {
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step_assign(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    router(JobsApiState {
        agent_budget: Some(Arc::new(BudgetDoor {
            agents,
            runs: Arc::new(InMemoryAgentRuns::new(vec![])),
        })),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

async fn claim(
    app: &axum::Router,
    step: &Step,
    claimed_for: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let query = claimed_for
        .map(|who| format!("?claimed_for={who}"))
        .unwrap_or_default();
    let resp = TestRequest::post(format!(
        "/api/jobs/{}/steps/{}/claim{query}",
        step.job_id, step.id
    ))
    .as_user(COORDINATOR, "platform-admin")
    .send(app)
    .await;
    let body = serde_json::from_slice(&resp.body_bytes).unwrap_or(serde_json::Value::Null);
    (resp.status, body)
}

#[tokio::test]
async fn the_claim_for_the_harness_actor_is_judged_on_its_own_row() {
    let jobs = Arc::new(InMemoryJobs::new());
    let job = packet();
    jobs.create_job(&job).await.unwrap();
    let (mine, theirs) = (launched_step(&job, "review"), launched_step(&job, "build"));
    jobs.add_step(&mine).await.unwrap();
    jobs.add_step(&theirs).await.unwrap();
    // The harness already has a launched run in flight; its cap is one.
    jobs.create_job(&open_run(HARNESS)).await.unwrap();
    let app = app(jobs.clone(), agents(1).await);

    // THE SHAPE THE VERB USED TO SEND: a claim for itself, judged on the
    // coordinator's uncapped row — admitted, though the run it is for
    // would be the harness's second against a cap of one.
    let (status, body) = claim(&app, &mine, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The claim FOR the harness is judged on the harness's row.
    let (status, body) = claim(&app, &theirs, Some(HARNESS)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "claim past the agent's concurrent-run cap");
    assert_eq!(body["actor"], HARNESS, "{body}");
    let stored = jobs.get_step(&theirs.id).await.unwrap().unwrap();
    assert_eq!(
        stored.status,
        StepStatus::Ready,
        "it never entered the race"
    );
}

#[tokio::test]
async fn a_step_claimed_for_the_harness_takes_the_launchers_edge_and_completion() {
    let jobs = Arc::new(InMemoryJobs::new());
    let job = packet();
    jobs.create_job(&job).await.unwrap();
    let step = launched_step(&job, "build");
    jobs.add_step(&step).await.unwrap();
    let app = app(jobs.clone(), agents(1).await);

    // Under its cap: admitted, held by the harness actor.
    let (status, body) = claim(&app, &step, Some(HARNESS)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);
    assert_eq!(stored.assignee_id.as_deref(), Some(HARNESS));

    let events = jobs.recorded_events();
    let sid = step.id.to_string();
    let marker = events
        .iter()
        .find(|e| e.kind.starts_with("step.assigned.") && e.payload["step_id"] == sid.as_str())
        .map(|e| e.payload.clone())
        .expect("a claim for someone else records its marker");
    assert_eq!(marker["claimed_by"], COORDINATOR);
    assert_eq!(marker["claimed_for"], HARNESS);
    // The budget reading is the HARNESS's hour: $2 of block against $1.
    let reading = events
        .iter()
        .find(|e| e.kind == boss_jobs::agent_budget::CLAIM_OVER_BUDGET)
        .map(|e| e.payload.clone())
        .expect("a claim over the holder's hour leaves its reading");
    assert_eq!(reading["actor"], HARNESS, "{reading}");

    // THE RUN EDGE `boss dispatch` writes next, signed by the coordinator,
    // onto a step it no longer holds.
    let resp = TestRequest::new(
        Method::PATCH,
        format!("/api/jobs/{}/steps/{}/metadata", job.id, step.id),
    )
    .as_user(COORDINATOR, "platform-admin")
    .json(&serde_json::json!({ "agent_run": RUN }))
    .send(&app)
    .await;
    assert!(resp.status.is_success(), "{}", resp.body_text());
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.metadata["agent_run"], RUN);
    assert_eq!(stored.assignee_id.as_deref(), Some(HARNESS));

    // AND THE COMPLETION the launcher tells the coordinator to make: the
    // status alone, by the actor that claimed it for the harness.
    let resp = TestRequest::put(format!("/api/jobs/{}/steps/{}", job.id, step.id))
        .as_user(COORDINATOR, "platform-admin")
        .json(&serde_json::json!({ "status": "completed" }))
        .send(&app)
        .await;
    assert!(resp.status.is_success(), "{}", resp.body_text());
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Completed);
    assert_eq!(stored.metadata["agent_run"], RUN, "the edge survives it");
}
