//! A CLAIM MADE FOR SOMEONE ELSE IS JUDGED AT ITS STATION, ON THE NOMINEE
//! (D1 of design 305cf4ce, "Claims stand at stations", backlog 5662d4b6).
//!
//! Until this car the claim door refused any station on a claim made for
//! someone else (`?claimed_for=`), 422 "a station's capability judges the
//! claimant, not the nominee" — so a coordinator starting a robot's step
//! could never say where the work was taken from, and every such claim
//! stood at no station or a generated one. The decided answer is the other
//! way round: the door judges the NOMINEE against the station — the
//! packet must be in that station's queue, and the station's capability
//! must admit the one who will hold the step — and the caller's own roles
//! play no part, since the caller is not the one who will do the work.
//!
//! The nominee's roles are what the registries hold for it: an agent's
//! `role` on its agents row, and a person's on the roster. Through the
//! real router and the in-memory adapters.

use std::sync::Arc;

use async_trait::async_trait;
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
use boss_jobs::events::{CLAIM_STATION_KEY, STEP_UPDATED};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, WorkflowSpec, WorkflowStatus};
use boss_jobs::station_queue::StepMatch;
use boss_jobs::{
    InMemoryJobs, InMemoryStations, InMemoryWorkflows, JobsRepository, StationCapability,
    StationKind, StationPredicate, StationRegistry, StationSpec, WorkflowRegistry,
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestRequest};
use chrono::NaiveDate;
use uuid::Uuid;

const KIND: &str = "door-kind";
/// The caller: a person holding `step-assign`, whose own role the
/// `build` station does not admit.
const CALLER: &str = "emp-coordinator";
const CALLER_ROLE: &str = "platform-admin";
/// The role the `build` station admits.
const BUILDER_ROLE: &str = "engineering-agent";

const COMPANY: &str = "0b5e0000-0000-4000-8000-0000000000a1";
const ENGINEERING: &str = "0b5e0000-0000-4000-8000-0000000000a2";
const BUILD: &str = "0b5e0000-0000-4000-8000-0000000000a3";

/// The roster: one person holding the builder role, one who does not.
struct Roster;

#[async_trait]
impl RosterLookup for Roster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            BUILDER_ROLE => vec!["emp-ada".to_string()],
            "brewer" => vec!["emp-bob".to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(matches!(id, "emp-ada" | "emp-bob" | CALLER))
    }
}

fn tree_row(name: &str, id: &str, parent: Option<&str>) -> StationSpec {
    let mut s = StationSpec::draft(
        name,
        name,
        StationKind::Group,
        StationPredicate {
            tags_any: vec![format!("station:{name}")],
            ..Default::default()
        },
        chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
    );
    s.status = WorkflowStatus::Active;
    s.id = Some(id.into());
    s.parent = parent.map(str::to_owned);
    s
}

/// The company, engineering, and `build` — which holds the packet's
/// build step and admits only the builder role.
fn tree() -> Vec<StationSpec> {
    let mut build = tree_row("build", BUILD, Some(ENGINEERING));
    build.predicate = StationPredicate {
        kind: Some(KIND.into()),
        step: Some(StepMatch {
            slug: Some("build".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    build.capability = Some(StationCapability {
        roles: vec![BUILDER_ROLE.into()],
        ..Default::default()
    });
    vec![
        tree_row("company", COMPANY, None),
        tree_row("engineering", ENGINEERING, Some(COMPANY)),
        build,
    ]
}

fn protocol() -> WorkflowSpec {
    let mut wf = WorkflowSpec::platform_seed(
        KIND,
        "Door kind",
        "test",
        vec!["custom".into()],
        vec![StepSpec {
            title: "build".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Build".into(),
            ..Default::default()
        }],
    );
    wf.metadata = serde_json::json!({ "owner": ENGINEERING });
    wf
}

async fn agents() -> Arc<InMemoryAgents> {
    let agents = Arc::new(InMemoryAgents::new());
    let row = |id: &str, role: &str| AgentInput {
        factory_rotation: Some(true),
        id: id.into(),
        display_name: id.into(),
        default_model: "opus-5-5".into(),
        model_class: None,
        aliases: vec![format!("{id}@example.test")],
        role: Some(role.into()),
        department: None,
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
        stations: None,
    };
    agents
        .publish(
            &[
                row("agent-builder", BUILDER_ROLE),
                row("agent-brewer", "brewer"),
            ],
            PublishMode::Take,
            &EventStamp::new("jobs", ActorId::Automation("test".into())),
        )
        .await
        .expect("the rows land");
    agents
}

struct Fixture {
    app: axum::Router,
    jobs: Arc<InMemoryJobs>,
    step: Step,
}

async fn fixture() -> Fixture {
    let jobs = Arc::new(InMemoryJobs::new());
    let job = Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            KIND,
            Subject::new("custom", "door"),
            "A packet whose build is claimed for someone else",
            CALLER,
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 13).unwrap(),
        )
    };
    jobs.create_job(&job).await.unwrap();
    let step = Step {
        id: StepId::from_uuid(Uuid::new_v4()),
        spec_slug: Some("build".into()),
        status: StepStatus::Ready,
        metadata: serde_json::json!({ "authority_role": CALLER_ROLE }),
        ..Step::new(job.id, "task", "build", 1)
    };
    jobs.add_step(&step).await.unwrap();

    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(protocol()).unwrap();
    let stations = InMemoryStations::new();
    for row in tree() {
        stations.seed(row).unwrap();
    }
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(CALLER_ROLE, Action::Update, Resource::step(), Scope::All)
            .allow(
                CALLER_ROLE,
                Action::Update,
                Resource::step_assign(),
                Scope::All,
            )
            .allow(CALLER_ROLE, Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let app = router(JobsApiState {
        agent_budget: Some(Arc::new(BudgetDoor {
            agents: agents().await,
            runs: Arc::new(InMemoryAgentRuns::new(vec![])),
        })),
        stations: Some(Arc::new(stations) as Arc<dyn StationRegistry>),
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(Roster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    Fixture { app, jobs, step }
}

async fn claim(f: &Fixture, query: &str) -> (StatusCode, serde_json::Value) {
    let resp = TestRequest::post(format!(
        "/api/jobs/{}/steps/{}/claim{query}",
        f.step.job_id, f.step.id
    ))
    .as_user(CALLER, CALLER_ROLE)
    .send(&f.app)
    .await;
    let body = serde_json::from_slice(&resp.body_bytes).unwrap_or(serde_json::Value::Null);
    (resp.status, body)
}

async fn stored(f: &Fixture) -> Step {
    f.jobs.get_step(&f.step.id).await.unwrap().unwrap()
}

/// The station is no longer refused: the claim for `agent-builder` names
/// `build`, is judged on the builder's role there, and stands at `build`
/// — while the caller's own claim naming the same station is refused,
/// because the station does not admit the CALLER's role. So the verdict
/// that admits the claim for someone else is the nominee's.
#[tokio::test]
async fn a_claim_for_someone_else_may_name_its_station_and_is_judged_on_the_nominee() {
    let f = fixture().await;
    let (status, body) = claim(&f, "?station=build").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "role not admitted by station capability");
    assert_eq!(stored(&f).await.status, StepStatus::Ready);

    for nominee in ["agent-builder", "emp-ada"] {
        let f = fixture().await;
        let (status, body) = claim(&f, &format!("?station=build&claimed_for={nominee}")).await;
        assert_eq!(status, StatusCode::OK, "{nominee}: {body}");
        let step = stored(&f).await;
        assert_eq!(step.status, StepStatus::Active);
        assert_eq!(step.assignee_id.as_deref(), Some(nominee));
        let events = f.jobs.recorded_events();
        let fact = events
            .iter()
            .find(|e| e.kind == STEP_UPDATED)
            .expect("the claim records its fact");
        assert_eq!(fact.payload[CLAIM_STATION_KEY], "build", "{}", fact.payload);
        let marker = events
            .iter()
            .find(|e| e.kind.starts_with("step.assigned."))
            .expect("a claim for someone else records its marker");
        assert_eq!(marker.payload["claimed_for"], nominee);
        assert_eq!(marker.payload["claimed_by"], CALLER);
    }
}

/// A nominee the station does not admit is refused there, 403, naming the
/// nominee and the roles the door read for it — an agent's from its row;
/// for a person, which of the station's roles the roster says they hold
/// (none) — and the step never enters the race.
#[tokio::test]
async fn a_nominee_the_station_does_not_admit_is_refused_naming_the_nominee() {
    for (nominee, read) in [
        ("agent-brewer", serde_json::json!(["brewer"])),
        ("emp-bob", serde_json::json!([])),
    ] {
        let f = fixture().await;
        let (status, body) = claim(&f, &format!("?station=build&claimed_for={nominee}")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{nominee}: {body}");
        assert_eq!(body["error"], "role not admitted by station capability");
        assert_eq!(body["claimed_for"], nominee, "{body}");
        assert_eq!(body["actor_roles"], read, "{body}");
        assert_eq!(
            body["allowed_roles"],
            serde_json::json!([BUILDER_ROLE]),
            "{body}"
        );
        let step = stored(&f).await;
        assert_eq!(step.status, StepStatus::Ready, "{nominee}");
        assert_eq!(step.assignee_id, None, "{nominee}");
    }
}
