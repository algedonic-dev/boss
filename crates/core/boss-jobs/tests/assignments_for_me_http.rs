//! `GET /api/jobs/assignments?for=me` — the viewer's own queue, whoever
//! the registry says the viewer is (backlog 74569e94, design ea906603
//! Q2, decided by David 2026-09-27).
//!
//! Measured the day the item was filed: the bare read as
//! `claude@algedonic.dev` answered 0 while `assignee_id=agent-claude`
//! answered 199 — the same actor under two spellings — and the only
//! thing that knew both was `boss orient`, on the CLI. The top board's
//! NEEDS YOU row needs the same answer in a browser, so the expansion
//! moved into the jobs API. What this pins, through the real router:
//!
//! 1. `for=me` reads for the viewer AND every id the agents registry
//!    ties to it, plus the roles the viewer holds (the request's
//!    platform role and the registry row's), each row once — and says
//!    whom it read for, in `for`.
//! 2. A viewer nobody named is refused, never answered with an empty
//!    queue that reads as "nothing needs you".
//! 3. A registry that cannot be read is a 503, never the viewer read
//!    alone as if it had no aliases.
//! 4. A `for=` the endpoint does not know, or `for=me` beside an
//!    explicit selector, is a 400 — never silently the other read.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publish::PublishMode;
use boss_core::publisher::DomainPublisher;
use boss_core::publisher::EventStamp;
use boss_jobs::agent_budget::BudgetDoor;
use boss_jobs::agent_runs::InMemoryAgentRuns;
use boss_jobs::agents::{
    AgentInput, AgentRow, AgentsBatchOutcome, AgentsError, AgentsRegistry, InMemoryAgents,
};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const AGENT: &str = "agent-claude";
const ALIAS: &str = "claude@algedonic.dev";

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

fn user_header(id: &str, role: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

/// A registry that cannot answer.
struct DarkRegistry;

#[async_trait]
impl AgentsRegistry for DarkRegistry {
    async fn resolve_login(&self, _: &str) -> Result<Option<String>, AgentsError> {
        Err(AgentsError::Storage("registry dark".into()))
    }
    async fn list(&self) -> Result<Vec<AgentRow>, AgentsError> {
        Err(AgentsError::Storage("registry dark".into()))
    }
    async fn publish(
        &self,
        _: &[AgentInput],
        _: PublishMode,
        _: &EventStamp,
    ) -> Result<AgentsBatchOutcome, AgentsError> {
        Err(AgentsError::Storage("registry dark".into()))
    }
}

fn app(agents: Arc<dyn AgentsRegistry>) -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow("guest", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        agent_budget: Some(Arc::new(BudgetDoor {
            agents,
            runs: Arc::new(InMemoryAgentRuns::new(Vec::new())),
        })),
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

fn registry() -> Arc<dyn AgentsRegistry> {
    Arc::new(InMemoryAgents::new().with_agent(AGENT, [ALIAS]))
}

/// One open packet with a step for each case: on the alias, on the
/// agent's id, claimable by the viewer's role, someone else's, and
/// claimable by a role the viewer does not hold.
async fn seed(jobs: &InMemoryJobs) {
    let job = Job {
        id: JobId::new(),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "s"),
        title: "an item".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 27).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    };
    jobs.create_job_at(&job, t("2026-09-27T00:00:00Z"), &[])
        .await
        .unwrap();
    let step = |title: &str, order: i32, assignee: Option<&str>, status: StepStatus, md: Value| {
        let mut s = Step::new(job.id, "task", title, order);
        s.id = StepId::new();
        s.spec_slug = Some(title.replace(' ', "-"));
        s.assignee_id = assignee.map(str::to_string);
        s.status = status;
        s.metadata = md;
        s
    };
    for s in [
        step("on the alias", 0, Some(ALIAS), StepStatus::Ready, json!({})),
        step("on the id", 1, Some(AGENT), StepStatus::Active, json!({})),
        step(
            "claimable",
            2,
            None,
            StepStatus::Ready,
            json!({ "authority_role": "platform-admin" }),
        ),
        step("theirs", 3, Some("emp-david"), StepStatus::Ready, json!({})),
        step(
            "not my role",
            4,
            None,
            StepStatus::Ready,
            json!({ "authority_role": "brewer" }),
        ),
    ] {
        jobs.add_step_at(&s, t("2026-09-27T00:00:00Z"), &[])
            .await
            .unwrap();
    }
}

async fn get(app: &axum::Router, uri: &str, header: Option<String>) -> (StatusCode, Value) {
    let mut req = Request::get(uri);
    if let Some(h) = header {
        req = req.header("x-boss-user", h);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()));
    (status, v)
}

fn titles(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = v["data"]
        .as_array()
        .unwrap_or_else(|| panic!("no data in {v}"))
        .iter()
        .filter_map(|r| r["step"]["title"].as_str().map(str::to_string))
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn for_me_reads_the_viewer_its_aliases_and_the_roles_it_holds() {
    let (app, jobs) = app(registry());
    seed(&jobs).await;
    let (status, v) = get(
        &app,
        "/api/jobs/assignments?for=me",
        Some(user_header(AGENT, "platform-admin")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        titles(&v),
        ["claimable", "on the alias", "on the id"],
        "{v}"
    );
    assert_eq!(v["total"], 3, "{v}");
    assert_eq!(
        v["for"],
        json!({ "ids": [AGENT, ALIAS], "roles": ["platform-admin"] }),
        "the read says whom it read for"
    );

    // Control on the same connection: the bare selector for the id
    // alone misses the alias's row — the gap `for=me` closes.
    let (_, bare) = get(
        &app,
        &format!("/api/jobs/assignments?assignee_id={AGENT}"),
        Some(user_header(AGENT, "platform-admin")),
    )
    .await;
    assert_eq!(titles(&bare), ["on the id"], "{bare}");
}

#[tokio::test]
async fn a_viewer_nobody_named_is_refused_not_answered_empty() {
    let (app, jobs) = app(registry());
    seed(&jobs).await;
    let (status, v) = get(&app, "/api/jobs/assignments?for=me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
}

#[tokio::test]
async fn a_dark_registry_is_a_503_not_the_viewer_read_alone() {
    let (app, jobs) = app(Arc::new(DarkRegistry));
    seed(&jobs).await;
    let (status, v) = get(
        &app,
        "/api/jobs/assignments?for=me",
        Some(user_header(AGENT, "platform-admin")),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert!(v.to_string().contains("agents registry"), "{v}");
}

#[tokio::test]
async fn an_unknown_for_or_one_beside_a_selector_is_refused() {
    let (app, _) = app(registry());
    let who = || Some(user_header(AGENT, "platform-admin"));
    let (status, v) = get(&app, "/api/jobs/assignments?for=everyone", who()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, v) = get(
        &app,
        "/api/jobs/assignments?for=me&assignee_id=emp-david",
        who(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
}
