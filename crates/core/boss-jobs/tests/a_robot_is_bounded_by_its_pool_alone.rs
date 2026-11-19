//! ONE MECHANISM BOUNDS A ROBOT: the `robots:<model>` pool its run's
//! `building` claim reserves a slot of (backlog 3e927996; David,
//! 2026-11-08, design a3c5e58a Q3).
//!
//! Until this car the claim door also judged a SECOND bound: the
//! actor's open `agent-run` packets against its agents row's
//! `max_concurrent_runs` (backlog 57c108c2). The station tree superseded
//! it once the pools carried concurrency (84f570a1, closed 2026-11-08),
//! and measured 2026-11-15 a robot was bounded twice — by its row (12, 6,
//! 12 on GET /api/agents) and by robots:opus 6 / robots:sol 2. Every
//! registered robot records a `model_class`, each names an active pool,
//! and `flow_resources::resolve_bill` refuses a class with no pool by
//! name, so retiring the row's bound leaves no robot unbounded.
//!
//! The pin: a row that still DECLARES a cap, already holding that many
//! open runs, is admitted at the claim door. The column survives only as
//! a stored declaration the tenant seed still writes; nothing judges it.

use std::sync::Arc;

use axum::http::StatusCode;
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
use serde_json::json;
use uuid::Uuid;

const ACTOR: &str = "agent-claude-b";
const LOGIN: &str = "claude-b@algedonic.dev";

fn job(kind: &str, metadata: serde_json::Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        status: JobStatus::Open,
        metadata,
        ..Job::new(
            kind,
            Subject::new("custom", "bosspipeline"),
            "work",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
        )
    }
}

fn agent_step(job: &Job, slug: &str, order: i32) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::new_v4()),
        spec_slug: Some(slug.into()),
        status: StepStatus::Ready,
        metadata: json!({
            "authority_role": "platform-admin",
            "agent_profile": "builder",
            "agent_model": "opus-5-5",
            "agent_budget_usd": 5.0,
            "agent_effort": "high",
        }),
        ..Step::new(job.id, "task", slug, order)
    }
}

/// The row as the tenant seed still declares it: a cap of one.
async fn agents_declaring_a_cap_of_one() -> Arc<InMemoryAgents> {
    let agents = Arc::new(InMemoryAgents::new());
    agents
        .publish(
            &[AgentInput {
                factory_rotation: Some(true),
                id: ACTOR.into(),
                display_name: "Claude B".into(),
                default_model: "opus-5-5".into(),
                model_class: Some("opus".into()),
                aliases: vec![LOGIN.into()],
                role: Some("platform-admin".into()),
                department: None,
                hourly_budget_usd_micros: None,
                max_concurrent_runs: Some(1),
                stations: None,
            }],
            PublishMode::Take,
            &EventStamp::new("jobs", ActorId::Automation("test".into())),
        )
        .await
        .expect("the row lands");
    agents
}

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

#[tokio::test]
async fn a_claim_past_the_rows_declared_run_cap_is_admitted() {
    let jobs = Arc::new(InMemoryJobs::new());
    let item = job("backlog-item", json!({}));
    jobs.create_job(&item).await.unwrap();
    let next = agent_step(&item, "build", 1);
    jobs.add_step(&next).await.unwrap();
    // Two runs in flight, neither handed to the gate, against a row that
    // declares a cap of one: the old door read 2 of 1 and refused.
    for _ in 0..2 {
        jobs.create_job(&job("agent-run", json!({ "agent": LOGIN })))
            .await
            .unwrap();
    }
    let app = app(jobs.clone(), agents_declaring_a_cap_of_one().await);

    let resp = TestRequest::post(format!("/api/jobs/{}/steps/{}/claim", next.job_id, next.id))
        .as_user(ACTOR, "platform-admin")
        .send(&app)
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&resp.body_bytes).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "the row's max_concurrent_runs bounds nothing; only a robots pool does: {body}"
    );
    assert_eq!(body["assignee_id"], ACTOR, "{body}");
    assert_eq!(body["status"], "active", "{body}");
}
