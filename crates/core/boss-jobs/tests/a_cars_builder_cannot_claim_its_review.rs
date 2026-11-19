//! A car's builder cannot claim the car's review, at the claim door
//! itself (backlog 2b927025, design d32b58b7 step 3).
//!
//! THE DEFECT. Distinctness lived in two places that were not the door:
//! the station pull's filter skipped a review step for the actor its car
//! records as `built_by`, and a coordinator nominated someone else by
//! hand. Every other claim was admitted — measured 2026-11-17, about 20
//! review nominations were made by hand to keep builders off their own
//! cars, with 45 open cars waiting at review.
//!
//! THE RULE, pinned through the real router: a ready review on a car is
//! claimable by any seat the step admits EXCEPT the actor that built the
//! car. The door judges the HOLDER — the claimant, or the one it is
//! claimed for — in every spelling the agents registry names, because
//! `built_by` records the login a builder signed with while a claim may
//! carry the registered id. The refusal names the holder, the builder
//! and the rule; the step stays ready and nobody's. A step that is not a
//! review, and a review whose holder did not build the car, are claimed
//! as before.

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
use uuid::Uuid;

/// The seat that built the car, by the login `boss dispatch --started`
/// stamps as `built_by`, and its registered id.
const BUILDER_LOGIN: &str = "claude@algedonic.dev";
const BUILDER_ID: &str = "agent-claude";
/// Another seat, which may review it.
const OTHER_ID: &str = "agent-codex";
const OTHER_LOGIN: &str = "codex@algedonic.dev";

/// A held car whose build is recorded as `built_by`.
fn car(built_by: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        status: JobStatus::Open,
        metadata: serde_json::json!({ boss_jobs::car::BUILT_BY: built_by }),
        ..Job::new(
            boss_jobs::regions::CAR_KIND,
            Subject::new("custom", "bosspipeline"),
            "A car held at review",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 18).unwrap(),
        )
    }
}

/// A ready step on `job` carrying an agent block of `profile`.
fn agent_step(job: &Job, slug: &str, profile: &str) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::new_v4()),
        spec_slug: Some(slug.into()),
        status: StepStatus::Ready,
        metadata: serde_json::json!({
            "authority_role": "platform-admin",
            "agent_profile": profile,
            "agent_model": "opus-5-5",
            "agent_budget_usd": 3.0,
            "agent_effort": "high",
        }),
        ..Step::new(job.id, "task", slug, 1)
    }
}

async fn agents() -> Arc<InMemoryAgents> {
    let agents = Arc::new(InMemoryAgents::new());
    let row = |id: &str, login: &str| AgentInput {
        factory_rotation: Some(true),
        id: id.into(),
        display_name: id.into(),
        default_model: "opus-5-5".into(),
        model_class: None,
        aliases: vec![login.into()],
        role: Some("platform-admin".into()),
        department: None,
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
        stations: None,
    };
    agents
        .publish(
            &[row(BUILDER_ID, BUILDER_LOGIN), row(OTHER_ID, OTHER_LOGIN)],
            PublishMode::Take,
            &EventStamp::new("jobs", ActorId::Automation("test".into())),
        )
        .await
        .expect("the rows land");
    agents
}

/// The router as the live policy grants platform-admin: step writes in
/// every scope, and `step-assign` (the coordinator's claim-for).
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
    signer: &str,
    claimed_for: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let query = claimed_for
        .map(|who| format!("?claimed_for={who}"))
        .unwrap_or_default();
    let resp = TestRequest::post(format!(
        "/api/jobs/{}/steps/{}/claim{query}",
        step.job_id, step.id
    ))
    .as_user(signer, "platform-admin")
    .send(app)
    .await;
    let body = serde_json::from_slice(&resp.body_bytes).unwrap_or(serde_json::Value::Null);
    (resp.status, body)
}

async fn seeded(built_by: &str, profile: &str) -> (axum::Router, Arc<InMemoryJobs>, Step) {
    let jobs = Arc::new(InMemoryJobs::new());
    let job = car(built_by);
    jobs.create_job(&job).await.unwrap();
    let step = agent_step(&job, boss_jobs::car::REVIEW_SLUG, profile);
    jobs.add_step(&step).await.unwrap();
    (app(jobs.clone(), agents().await), jobs, step)
}

/// The builder, signing in either spelling, is refused its own car's
/// review; the refusal names the holder, the builder and the rule, and
/// the step stays ready and nobody's for another seat to pull.
#[tokio::test]
async fn the_builder_is_refused_its_own_cars_review_in_every_spelling() {
    for signer in [BUILDER_LOGIN, BUILDER_ID] {
        let (app, jobs, step) = seeded(BUILDER_LOGIN, "reviewer").await;
        let (status, body) = claim(&app, &step, signer, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "signed as {signer}: {body}");
        assert_eq!(body["built_by"], BUILDER_LOGIN, "{body}");
        assert_eq!(body["holder"], signer, "{body}");
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("built this car")),
            "the refusal says why: {body}"
        );
        let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
        assert_eq!(stored.status, StepStatus::Ready, "signed as {signer}");
        assert_eq!(stored.assignee_id, None, "signed as {signer}");
    }
}

/// A claim FOR the builder is judged on the holder, not the caller: a
/// coordinator cannot hand the builder its own review either.
#[tokio::test]
async fn a_claim_for_the_builder_is_refused_on_the_holder() {
    let (app, jobs, step) = seeded(OTHER_LOGIN, "reviewer").await;
    let (status, body) = claim(&app, &step, BUILDER_LOGIN, Some(OTHER_ID)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["holder"], OTHER_ID, "{body}");
    assert_eq!(body["built_by"], OTHER_LOGIN, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Ready);
}

/// Any other seat takes the review, for itself or claimed for it.
#[tokio::test]
async fn any_other_seat_claims_the_review() {
    let (app, jobs, step) = seeded(BUILDER_LOGIN, "reviewer").await;
    let (status, body) = claim(&app, &step, OTHER_LOGIN, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);

    let (app, jobs, step) = seeded(BUILDER_LOGIN, "reviewer").await;
    let (status, body) = claim(&app, &step, BUILDER_LOGIN, Some(OTHER_ID)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(OTHER_ID));
}

/// The rule is the review profile's alone: the builder still claims a
/// step of its own car that is not a review.
#[tokio::test]
async fn a_step_that_is_not_a_review_is_claimed_by_its_builder() {
    let (app, jobs, step) = seeded(BUILDER_LOGIN, "builder").await;
    let (status, body) = claim(&app, &step, BUILDER_LOGIN, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);
}
