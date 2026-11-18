//! A claim on a step that names its executor clears only for that kind
//! with that capability (factory taxonomy T2c, backlog 4839d6a1; item
//! 0a78f8f8, decided 2026-11-03 "The eligibility predicate").
//!
//! T2b let a step's `executor` name a KIND — `role:<slug>`,
//! `model:<class>`, `equipment:<station>` — and refused an undeclared one
//! at publish, but the claim door did not read it: anyone the packet's
//! scope admitted took the step. This is the claim route itself, over
//! the in-memory adapters, as the matrix the item asks for: per kind,
//! admitted when the claimant's kind AND capability match, refused on
//! each half alone with the half named; an equipment station with no
//! free bay refuses its own automaton; a step naming no executor, or a
//! credential principal, is judged exactly as before.
//!
//! The kind is read off the holder's CANONICAL id (its agents row, by id
//! or alias — 43ca3b3a), never off the spelling it signed with. The
//! model half reads the class the robot's agents row RECORDS (T2d,
//! backlog 06d8988b): a row recording the demanded class, whose
//! `robots:<class>` pool is active, is admitted; a row recording
//! none, a class no active pool declares, or another class is refused
//! (conductor decision 20261104T0939 — nothing parses `opus-5-5[1m]` to
//! guess one).

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
use boss_jobs::field_writer::CredentialedCaller;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::pools::{InMemoryPools, PoolRegistry, PoolSpec};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::stations::{StationKind, StationRegistry, StationSpec};
use boss_jobs::{
    InMemoryJobs, InMemoryWorkflows, JobsRepository, StationPredicate, WorkflowRegistry,
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestRequest};
use chrono::{NaiveDate, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

const KIND: &str = "executor-fixture";
/// A person who holds the `auditor` role Class.
const AUDITOR: &str = "emp-auditor";
/// A person who holds no role the steps name.
const CLERK: &str = "emp-clerk";
/// A registered robot, and the login that signs as it.
const ROBOT: &str = "agent-robo";
const ROBOT_LOGIN: &str = "robo@algedonic.dev";
/// Robots whose rows record a model class: the one `by-model` demands,
/// one no active pool declares, and another declared one.
const OPUS_ROBOT: &str = "agent-opus";
const OPUS_LOGIN: &str = "opus-robot@algedonic.dev";
const UNPOOLED_ROBOT: &str = "agent-unpooled";
const SONNET_ROBOT: &str = "agent-sonnet";
/// The kettle station's automaton, and one that operates nothing here.
const KETTLE_RUNNER: &str = "automation:kettle-runner";
const OTHER_AUTOMATON: &str = "automation:other";

struct Roster;

#[async_trait]
impl RosterLookup for Roster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "auditor" => vec![AUDITOR.to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == AUDITOR || id == CLERK)
    }
}

fn step_spec(slug: &str, executor: Option<&str>) -> StepSpec {
    StepSpec {
        title: slug.into(),
        kind: "task".into(),
        ready_when: "true".into(),
        title_template: slug.into(),
        executor: executor.map(str::to_string),
        ..Default::default()
    }
}

fn protocol() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        KIND,
        "Executor fixture",
        "test",
        vec!["custom".into()],
        vec![
            step_spec("by-role", Some("role:auditor")),
            step_spec("by-model", Some("model:opus")),
            step_spec("by-either", Some("model:opus|sonnet")),
            step_spec("by-equipment", Some("equipment:kettle")),
            step_spec("by-principal", Some("runner:ops")),
            step_spec("by-anyone", None),
            // Held by the kettle's automaton, but demanding no station:
            // it occupies no kettle bay.
            step_spec("elsewhere", None),
        ],
    )
}

fn kettle(bays: i32) -> StationSpec {
    StationSpec {
        status: boss_jobs::registry::WorkflowStatus::Active,
        automaton: Some(KETTLE_RUNNER.into()),
        bays: Some(bays),
        ..StationSpec::draft(
            "kettle",
            "The kettle",
            StationKind::Equipment,
            StationPredicate {
                kind: Some(KIND.into()),
                ..Default::default()
            },
            Utc::now(),
        )
    }
}

struct Fixture {
    app: axum::Router,
    credentialed: axum::Router,
    jobs: Arc<InMemoryJobs>,
}

async fn fixture(bays: i32) -> Fixture {
    let workflows = Arc::new(InMemoryWorkflows::for_fixture());
    workflows.seed(protocol()).unwrap();
    let stations = Arc::new(boss_jobs::stations::InMemoryStations::new());
    stations.seed(kettle(bays)).unwrap();
    let agents = Arc::new(InMemoryAgents::new());
    let robot = |id: &str, login: Option<&str>, class: Option<&str>| AgentInput {
        factory_rotation: Some(true),
        id: id.into(),
        display_name: id.into(),
        default_model: "opus-5-5[1m]".into(),
        model_class: class.map(str::to_string),
        aliases: login.into_iter().map(str::to_string).collect(),
        role: Some("auditor".into()),
        department: None,
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
        stations: None,
    };
    // Straight through the adapter, not the batch door: the door refuses
    // a class no active pool declares, but a pool can retire after its
    // class was recorded, and the claim must still refuse that row.
    agents
        .publish(
            &[
                robot(ROBOT, Some(ROBOT_LOGIN), None),
                robot(OPUS_ROBOT, Some(OPUS_LOGIN), Some("opus")),
                robot(UNPOOLED_ROBOT, None, Some("haiku")),
                robot(SONNET_ROBOT, None, Some("sonnet")),
            ],
            PublishMode::Take,
            &EventStamp::new("jobs", ActorId::Automation("test".into())),
        )
        .await
        .unwrap();
    // The robot pools: `opus` and `sonnet` are declared classes,
    // `haiku` is not.
    let pools = Arc::new(InMemoryPools::default());
    for class in ["opus", "sonnet"] {
        let spec: PoolSpec = serde_json::from_value(json!({
            "name": format!("robots:{class}"), "version": 1, "status": "active",
            "unit": "slot", "kind": "flow", "capacity": { "state": "unknown" },
        }))
        .unwrap();
        pools
            .publish_declared(spec, &ActorId::Automation("test".into()), Utc::now())
            .await
            .unwrap();
    }
    let jobs = Arc::new(InMemoryJobs::new().with_resource_pools(pools));
    let mut policy = FakePolicyClient::builder();
    for role in ["platform-admin", "system"] {
        policy = policy
            .allow(role, Action::Update, Resource::step(), Scope::All)
            .allow(role, Action::Read, Resource::job(), Scope::All);
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let app = router(JobsApiState {
        kind_registry: Some(workflows as Arc<dyn WorkflowRegistry>),
        stations: Some(stations as Arc<dyn StationRegistry>),
        roster: Some(Arc::new(Roster)),
        agent_budget: Some(Arc::new(BudgetDoor {
            agents,
            runs: Arc::new(InMemoryAgentRuns::new(vec![])),
        })),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    let credentialed = app.clone().layer(axum::Extension(CredentialedCaller {
        principal: "runner:ops".into(),
        actor_id: "automation:ops-runner".into(),
        host: Some("forge".into()),
    }));
    Fixture {
        app,
        credentialed,
        jobs,
    }
}

/// A packet pinned to the fixture protocol, with one READY step at
/// `slug` (`credential_executor` materialised on the principal's, as the
/// materialiser projects it).
async fn packet(jobs: &InMemoryJobs, slug: &str) -> Step {
    let job = Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        status: JobStatus::Open,
        metadata: json!({ "host": "forge" }),
        ..Job::new(
            KIND,
            Subject::new("custom", "kettle"),
            "Executor fixture",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
        )
        .with_workflow_version(1)
    };
    jobs.create_job(&job).await.unwrap();
    let step = Step {
        id: StepId::from_uuid(Uuid::new_v4()),
        spec_slug: Some(slug.into()),
        status: StepStatus::Ready,
        metadata: if slug == "by-principal" {
            json!({ "credential_executor": "runner:ops" })
        } else {
            json!({})
        },
        ..Step::new(job.id, "task", slug, 0)
    };
    jobs.add_step(&step).await.unwrap();
    step
}

async fn claim(app: &axum::Router, step: &Step, signer: &str) -> (StatusCode, Value) {
    let resp = TestRequest::post(format!("/api/jobs/{}/steps/{}/claim", step.job_id, step.id))
        .as_user(signer, "platform-admin")
        .send(app)
        .await;
    let body = serde_json::from_slice(&resp.body_bytes).unwrap_or(Value::Null);
    (resp.status, body)
}

/// Admitted: the step is Active and held by the claimant.
async fn admitted(f: &Fixture, step: &Step, signer: &str, holder: &str) {
    let (status, body) = claim(&f.app, step, signer).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{signer} on {:?}: {body}",
        step.spec_slug
    );
    let stored = f.jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);
    assert_eq!(stored.assignee_id.as_deref(), Some(holder));
}

/// Refused, naming the half that failed, and the step never entered the
/// race.
async fn refused(f: &Fixture, step: &Step, signer: &str, half: &str) -> Value {
    let (status, body) = claim(&f.app, step, signer).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{signer} on {:?}: {body}",
        step.spec_slug
    );
    assert_eq!(
        body["error"], "the claimant is not the executor this step names",
        "{body}"
    );
    assert_eq!(body["half"], half, "{body}");
    let stored = f.jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        stored.status,
        StepStatus::Ready,
        "it never entered the race"
    );
    assert_eq!(stored.assignee_id, None);
    body
}

#[tokio::test]
async fn a_role_step_clears_only_for_a_person_holding_the_role() {
    let f = fixture(1).await;
    let step = packet(&f.jobs, "by-role").await;

    // A robot whose own row names the role is still not a person.
    let body = refused(&f, &step, ROBOT_LOGIN, "kind").await;
    assert_eq!(body["executor"], "role:auditor");
    assert_eq!(body["demanded"], "person");
    assert_eq!(body["found"], "robot", "read off the canonical row: {body}");
    assert_eq!(body["holder"], ROBOT, "{body}");

    // A person, but not one the roster lists as holding `auditor`.
    let body = refused(&f, &step, CLERK, "capability").await;
    assert_eq!(body["demanded"], "role auditor");
    assert!(
        body["found"].as_str().unwrap().contains("does not hold"),
        "{body}"
    );

    admitted(&f, &step, AUDITOR, AUDITOR).await;
}

#[tokio::test]
async fn a_model_step_refuses_a_person_and_a_robot_with_no_model_class() {
    let f = fixture(1).await;
    let step = packet(&f.jobs, "by-model").await;

    let body = refused(&f, &step, AUDITOR, "kind").await;
    assert_eq!(body["demanded"], "robot");
    assert_eq!(body["found"], "person");

    // The robot runs `opus-5-5[1m]`; no registry records that model's
    // class, and nothing guesses one from the id.
    let body = refused(&f, &step, ROBOT_LOGIN, "capability").await;
    assert_eq!(body["demanded"], "model class opus");
    assert!(
        body["found"]
            .as_str()
            .unwrap()
            .contains("records no model class"),
        "{body}"
    );
}

/// The cases T2c could not reach (T2d, backlog 06d8988b): the class is
/// read off the robot's row, and judged against the pool registry at
/// the claim — not only when the row was declared.
#[tokio::test]
async fn a_model_step_clears_for_a_robot_whose_row_records_that_class() {
    let f = fixture(1).await;
    let step = packet(&f.jobs, "by-model").await;

    // A recorded class whose pool no longer exists is not a class held.
    let body = refused(&f, &step, UNPOOLED_ROBOT, "capability").await;
    assert_eq!(body["demanded"], "model class opus");
    let found = body["found"].as_str().unwrap();
    assert!(
        found.contains("haiku") && found.contains("no active pool robots:haiku"),
        "names the class it records and the pool it lacks: {body}"
    );

    // A declared class, but not the one the step demands.
    let body = refused(&f, &step, SONNET_ROBOT, "capability").await;
    assert!(
        body["found"]
            .as_str()
            .unwrap()
            .contains("runs model class sonnet"),
        "{body}"
    );

    // Its row records `opus`, `robots:opus` is active: admitted,
    // judged by its canonical row though it signs as its login (held
    // under that spelling, as the in-memory CAS stores it).
    admitted(&f, &step, OPUS_LOGIN, OPUS_LOGIN).await;
}

/// A set of classes (backlog 446ab559): a robot of ANY class in it is
/// admitted, and the refusal names every class the step demands.
#[tokio::test]
async fn a_model_set_step_clears_for_a_robot_of_any_class_in_it() {
    let f = fixture(1).await;
    let step = packet(&f.jobs, "by-either").await;
    let body = refused(&f, &step, UNPOOLED_ROBOT, "capability").await;
    assert_eq!(body["demanded"], "model class opus or sonnet", "{body}");
    assert_eq!(body["executor"], "model:opus|sonnet", "{body}");
    admitted(&f, &step, SONNET_ROBOT, SONNET_ROBOT).await;

    let step = packet(&f.jobs, "by-either").await;
    admitted(&f, &step, OPUS_LOGIN, OPUS_LOGIN).await;
}

#[tokio::test]
async fn an_equipment_step_clears_only_for_its_automaton_with_a_free_bay() {
    let f = fixture(1).await;
    let step = packet(&f.jobs, "by-equipment").await;

    let body = refused(&f, &step, AUDITOR, "kind").await;
    assert_eq!(body["demanded"], "automaton");
    assert_eq!(body["found"], "person");

    let body = refused(&f, &step, OTHER_AUTOMATON, "capability").await;
    assert_eq!(
        body["demanded"],
        "the automaton of equipment station kettle"
    );
    assert!(
        body["found"].as_str().unwrap().contains(KETTLE_RUNNER),
        "names who operates it: {body}"
    );

    // A hold of the automaton's that demands no station is no bay.
    let elsewhere = packet(&f.jobs, "elsewhere").await;
    admitted(&f, &elsewhere, KETTLE_RUNNER, KETTLE_RUNNER).await;
    admitted(&f, &step, KETTLE_RUNNER, KETTLE_RUNNER).await;

    // The one bay is now occupied by that claim: the next kettle step
    // waits for it.
    let next = packet(&f.jobs, "by-equipment").await;
    let body = refused(&f, &next, KETTLE_RUNNER, "capability").await;
    assert!(
        body["found"]
            .as_str()
            .unwrap()
            .contains("no free bay: 1 of 1 occupied"),
        "{body}"
    );
}

#[tokio::test]
async fn a_second_bay_admits_a_second_hold() {
    let f = fixture(2).await;
    for _ in 0..2 {
        let step = packet(&f.jobs, "by-equipment").await;
        admitted(&f, &step, KETTLE_RUNNER, KETTLE_RUNNER).await;
    }
    let third = packet(&f.jobs, "by-equipment").await;
    refused(&f, &third, KETTLE_RUNNER, "capability").await;
}

#[tokio::test]
async fn a_step_naming_no_kind_is_judged_as_before() {
    let f = fixture(1).await;
    // No executor: a robot, a person and an automaton each take it —
    // held under the spelling it signed with, as the in-memory CAS
    // always stored it (the PG CAS rewrites an alias to the id).
    for signer in [ROBOT_LOGIN, CLERK, OTHER_AUTOMATON] {
        let step = packet(&f.jobs, "by-anyone").await;
        admitted(&f, &step, signer, signer).await;
    }
    // A credential principal: the credential door's, as before — the
    // credentialed caller executes it, an uncredentialed one is refused
    // by that door (409), not by this one.
    let step = packet(&f.jobs, "by-principal").await;
    let (status, body) = claim(&f.app, &step, CLERK).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = claim(&f.credentialed, &step, CLERK).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = f.jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some("automation:ops-runner"));
}

/// ONE CLAIM TAKES AN EQUIPMENT STATION'S LAST BAY (backlog 5a571b1a).
///
/// The bay cases above go through the door, one claim at a time — and
/// the door counted the bays from a plain read BEFORE the claim's write
/// (`step_executor::occupied_bays`), with nothing holding the count
/// still until the write. Measured on origin/main 2026-11-10: two
/// concurrent claims by the same automaton could each read the last bay
/// free and both be admitted, so a station ran over its bays.
///
/// The count is now made inside the claim's own write: under the
/// station's lineage lock in the PostgreSQL adapter (the
/// `pg_advisory_xact_lock` a station publish takes), and under the state
/// lock in the in-memory one. These fire the claims through the port
/// itself, concurrently, on both adapters: exactly as many win as there
/// are bays, and every loser is refused naming the station and its bays,
/// with nothing written.
mod one_claim_takes_the_last_bay {
    use std::sync::Arc;

    use boss_core::actor::ActorId;
    use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
    use boss_core::publisher::EventStamp;
    use boss_jobs::registry::{WorkflowRegistry, WorkflowSpec};
    use boss_jobs::stations::{StationKind, StationSpec};
    use boss_jobs::{JobsRepository, StationPredicate};
    use serde_json::json;

    const KIND: &str = "bay-contract";
    const STATION: &str = "kettle";
    const RUNNER: &str = "automation:kettle-runner";

    struct Fixture<J> {
        jobs: Arc<J>,
        stamp: EventStamp,
    }

    fn stamp() -> EventStamp {
        EventStamp::new("jobs", ActorId::automation("bay-contract"))
    }

    /// The kettle: an equipment station `RUNNER` operates, with `bays`.
    fn kettle(bays: i32) -> StationSpec {
        StationSpec {
            automaton: Some(RUNNER.into()),
            bays: Some(bays),
            ..StationSpec::draft(
                STATION,
                "The kettle",
                StationKind::Equipment,
                StationPredicate {
                    kind: Some(KIND.into()),
                    ..Default::default()
                },
                chrono::Utc::now(),
            )
        }
    }

    /// The protocol: `boil` demands the kettle, `elsewhere` demands no
    /// station, so a hold of it occupies no kettle bay.
    fn protocol(stamp: &EventStamp) -> WorkflowSpec {
        serde_json::from_value(json!({
            "kind": KIND, "label": "Bay contract", "category": "test", "version": 1,
            "status": "active", "subject_kinds": ["custom"], "owning_team": "platform",
            "created_at": stamp.timestamp,
            "steps": [
                {"title": "start", "kind": "trigger", "ready_when": "true"},
                {"title": "boil", "kind": "task", "ready_when": "steps.start.done",
                 "executor": format!("equipment:{STATION}")},
                {"title": "elsewhere", "kind": "task", "ready_when": "steps.start.done"},
                {"title": "done", "kind": "outcome",
                 "ready_when": "steps.boil.done AND steps.elsewhere.done",
                 "terminal": {"outcome": "done"}}
            ]
        }))
        .unwrap()
    }

    async fn publish(workflows: &dyn WorkflowRegistry, stamp: &EventStamp) {
        workflows
            .publish_authored(
                protocol(stamp),
                JobId::new(),
                stamp.actor(),
                stamp.timestamp,
            )
            .await
            .unwrap();
    }

    async fn in_memory(bays: i32) -> Fixture<boss_jobs::InMemoryJobs> {
        let stamp = stamp();
        let workflows = Arc::new(boss_jobs::registry::InMemoryWorkflows::for_fixture());
        publish(workflows.as_ref(), &stamp).await;
        let mut row = kettle(bays);
        row.status = boss_jobs::registry::WorkflowStatus::Active;
        let jobs = Arc::new(
            boss_jobs::InMemoryJobs::new()
                .with_workflows(workflows)
                .with_station_rows([row]),
        );
        Fixture { jobs, stamp }
    }

    async fn postgres(bays: i32) -> (Fixture<boss_jobs::PgJobs>, boss_testing::TestDb) {
        use boss_jobs::agents::AgentsRegistry;
        use boss_jobs::stations::StationRegistry;
        let stamp = stamp();
        let db = boss_testing::TestDb::new().await;
        // The publish door refuses an equipment station whose automaton
        // no `automation_actors` row holds (backlog d0cf2cdc), so the
        // kettle's runner is registered before the kettle is published.
        boss_jobs::agents::PgAgents::new(db.pool.clone())
            .declare_automations(
                &[boss_jobs::agents::AutomationActor {
                    id: RUNNER.into(),
                    role: "platform-admin".into(),
                    description: "Operates the kettle's bays".into(),
                    signs_for: None,
                    access_tier: None,
                    station: None,
                }],
                &stamp,
            )
            .await
            .unwrap();
        let stations = boss_jobs::stations::PgStations::new(db.pool.clone());
        let row = kettle(bays);
        stations
            .create_draft(row.clone(), stamp.actor(), stamp.timestamp)
            .await
            .unwrap();
        stations
            .publish(&row.name, stamp.actor(), stamp.timestamp)
            .await
            .unwrap();
        publish(
            &boss_jobs::registry::PgWorkflows::for_fixture(db.pool.clone()),
            &stamp,
        )
        .await;
        let jobs = Arc::new(boss_jobs::PgJobs::new(db.pool.clone()));
        (Fixture { jobs, stamp }, db)
    }

    /// A packet pinned to the protocol with one READY step at `slug`.
    async fn ready<J: JobsRepository>(f: &Fixture<J>, slug: &str) -> Step {
        let mut job = Job::new(
            KIND,
            Subject::new("custom", "bay-contract"),
            "Bay contract",
            "emp-owner",
            Priority::Standard,
            f.stamp.timestamp.date_naive(),
        );
        job.status = JobStatus::Open;
        job.workflow_version = 1;
        f.jobs
            .create_job_at(
                &job,
                f.stamp.timestamp,
                &[f.stamp.event(boss_jobs::events::JOB_CREATED, json!(job))],
            )
            .await
            .unwrap();
        let mut step = Step::new(job.id, "task", slug, 1);
        step.spec_slug = Some(slug.into());
        step.status = StepStatus::Ready;
        f.jobs
            .add_step_at(
                &step,
                f.stamp.timestamp,
                &[f.stamp.event(boss_jobs::events::STEP_CREATED, json!(step))],
            )
            .await
            .unwrap();
        step
    }

    /// The step a refused claim left: still READY and held by no one.
    async fn untouched<J: JobsRepository>(f: &Fixture<J>, step: &Step) {
        let stored = f.jobs.get_step(&step.id).await.unwrap().unwrap();
        assert_eq!(stored.status, StepStatus::Ready, "the loser wrote nothing");
        assert_eq!(stored.assignee_id, None, "the loser wrote nothing");
    }

    /// Every refusal names the station's bays as the claim counted them.
    fn names_the_bay<T: std::fmt::Debug>(
        refused: &Result<T, boss_jobs::port::JobsError>,
        full: &str,
    ) {
        let Err(error) = refused else {
            panic!("expected a refusal, got {refused:?}");
        };
        let said = error.to_string();
        assert!(
            said.contains(STATION) && said.contains(&format!("no free bay: {full}")),
            "the refusal names the bay: {said}"
        );
    }

    /// The last free bay: the automaton already holds a step demanding no
    /// station (no bay), and two claims for the one kettle bay race.
    async fn two_claims_for_the_last_free_bay_admit_exactly_one<J: JobsRepository>(
        f: &Fixture<J>,
        adapter: &str,
    ) {
        let elsewhere = ready(f, "elsewhere").await;
        f.jobs
            .claim_step_at(&elsewhere.id, RUNNER, &f.stamp, &[])
            .await
            .unwrap();
        let a = ready(f, "boil").await;
        let b = ready(f, "boil").await;

        let (one, two) = tokio::join!(
            f.jobs.claim_step_at(&a.id, RUNNER, &f.stamp, &[]),
            f.jobs.claim_step_at(&b.id, RUNNER, &f.stamp, &[]),
        );
        assert_eq!(
            usize::from(one.is_ok()) + usize::from(two.is_ok()),
            1,
            "{adapter}: one bay admits one claim — {one:?} / {two:?}"
        );
        let loser = if one.is_ok() { (&two, &b) } else { (&one, &a) };
        names_the_bay(loser.0, "1 of 1 occupied");
        untouched(f, loser.1).await;

        // The winner's re-claim is idempotent: its own hold is not
        // counted against it.
        let winner = if one.is_ok() { &a } else { &b };
        f.jobs
            .claim_step_at(&winner.id, RUNNER, &f.stamp, &[])
            .await
            .unwrap();
    }

    /// Two bays, three concurrent claims: exactly two win.
    async fn three_claims_for_two_bays_admit_exactly_two<J: JobsRepository>(
        f: &Fixture<J>,
        adapter: &str,
    ) {
        let steps = [
            ready(f, "boil").await,
            ready(f, "boil").await,
            ready(f, "boil").await,
        ];
        let (one, two, three) = tokio::join!(
            f.jobs.claim_step_at(&steps[0].id, RUNNER, &f.stamp, &[]),
            f.jobs.claim_step_at(&steps[1].id, RUNNER, &f.stamp, &[]),
            f.jobs.claim_step_at(&steps[2].id, RUNNER, &f.stamp, &[]),
        );
        let results = [one, two, three];
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            2,
            "{adapter}: two bays admit two claims — {results:?}"
        );
        for (result, step) in results.iter().zip(&steps) {
            if result.is_err() {
                names_the_bay(result, "2 of 2 occupied");
                untouched(f, step).await;
            }
        }
    }

    /// Each case on both adapters (the file is declared to Cargo with
    /// `required-features = ["postgres"]`, which `PgJobs` needs).
    macro_rules! both_adapters {
        ($($case:ident => $bays:expr),+ $(,)?) => {
            $(
                mod $case {
                    #[tokio::test(flavor = "multi_thread")]
                    async fn in_memory() {
                        let f = super::in_memory($bays).await;
                        super::$case(&f, "in_memory").await;
                    }

                    #[tokio::test(flavor = "multi_thread")]
                    async fn postgres() {
                        let (f, _db) = super::postgres($bays).await;
                        super::$case(&f, "postgres").await;
                    }
                }
            )+
        };
    }

    both_adapters! {
        two_claims_for_the_last_free_bay_admit_exactly_one => 1,
        three_claims_for_two_bays_admit_exactly_two => 2,
    }
}
