//! A gate hand-off is written only as the gate receipted it (backlog
//! a99dd722; adversarial finding 1 of run 77fa2b8e; Codex review a4f47f9d).
//!
//! `boss gate` copies the server's receipt of its durable submission onto
//! the run as `handed_to_gate`, and completes the run's `building` handed,
//! which exits its `robots:<model>` claim. Until backlog 3e927996 the
//! record also released a slot of the per-actor run cap, and these tests
//! observed it through that cap's refusals; the cap is retired (a robot is
//! bounded by its pool alone), so they observe the stored record itself.
//!
//! Through the real router, on the in-memory adapters:
//!
//! 1. A HAND-WRITTEN record is refused at the merge door: one with no
//!    gate-run behind it, one whose gate names another run, or one whose
//!    receipt event id was altered. A job PUT keeps the stored record,
//!    and a packet cannot be filed already handed.
//! 2. A job PUT that read the run BEFORE its hand-off landed writes
//!    nothing: it judges the record as it read it under the write's lock,
//!    so it cannot save over a hand-off committed in between. The gate
//!    submissions here are real: bound through the in-memory adapter's
//!    own first-binding door.

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publish::PublishMode;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::agent_budget::BudgetDoor;
use boss_jobs::agent_runs::InMemoryAgentRuns;
use boss_jobs::agents::types::AgentInput;
use boss_jobs::agents::{AgentsRegistry, InMemoryAgents};
use boss_jobs::conditional_metadata::{Command, Receipt};
use boss_jobs::gate_reservation::{LaunchIntent, Submission};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestRequest};
use chrono::NaiveDate;
use serde_json::json;
use uuid::Uuid;

/// The registered builder and the login its runs are filed under.
const ACTOR: &str = "agent-claude-b";
const LOGIN: &str = "claude-b@algedonic.dev";

/// An open run filed under the builder's login — what `boss dispatch`
/// files right after an admitted claim.
fn open_run() -> Job {
    Job {
        id: JobId::new(),
        status: JobStatus::Open,
        metadata: json!({ "agent": LOGIN }),
        ..Job::new(
            "agent-run",
            Subject::new("custom", "bosspipeline"),
            "run",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(),
        )
    }
}

async fn agents() -> Arc<InMemoryAgents> {
    let agents = Arc::new(InMemoryAgents::new());
    agents
        .publish(
            &[AgentInput {
                factory_rotation: Some(true),
                id: ACTOR.into(),
                display_name: "Claude B".into(),
                default_model: "opus-5-5".into(),
                model_class: None,
                aliases: vec![LOGIN.into()],
                role: Some("platform-admin".into()),
                department: None,
                hourly_budget_usd_micros: None,
                max_concurrent_runs: None,
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
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
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

/// The jobs store with the one protocol the gate's submission is bound
/// under: a `gate-run` whose metadata declares `gate_submission`
/// immutable, as `infra/platform/workflows/gate-run.toml` does. The
/// adapter's own first-binding door then writes the immutable index and
/// retains the receipt, so nothing in these tests plants either.
fn store() -> Arc<InMemoryJobs> {
    use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
    let registry = Arc::new(boss_jobs::registry::InMemoryWorkflows::new());
    // A store with a registry admits only kinds it declares, so the two
    // other kinds these tests file are declared beside it, plainly.
    for (kind, schema) in [
        (
            "gate-run",
            json!({"type": "object", "immutable_fields": ["gate_submission"]}),
        ),
        ("agent-run", json!({"type": "object"})),
        ("backlog-item", json!({"type": "object"})),
    ] {
        let mut spec = WorkflowSpec::platform_seed(
            kind,
            "Fixture",
            "platform",
            vec!["custom".into()],
            vec![StepSpec {
                title: "done".into(),
                kind: "outcome".into(),
                ready_when: "false".into(),
                terminal: Some(Terminal {
                    outcome: "completed".into(),
                }),
                metadata_defaults: json!({"outcome_kind": "completed"}),
                ..Default::default()
            }],
        );
        spec.metadata_schema = schema;
        registry.seed(spec).expect("the fixture protocol seeds");
    }
    Arc::new(InMemoryJobs::new().with_workflows(registry))
}

/// A real durable submission: a gate-run filed for `run`, and its
/// `gate_submission` bound through the adapter's conditional door as
/// `submitter` — the server's receipt is what this returns.
async fn submit_gate(jobs: &InMemoryJobs, run: &str, submitter: &str) -> Receipt {
    let mut gate = Job::new(
        "gate-run",
        Subject::new("custom", "bosspipeline"),
        "gate: feat/x",
        "emp-owner",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(),
    );
    gate.status = JobStatus::Open;
    gate.metadata = json!({"branch": "feat/x", "sha": "abc123", "agent_run": run,
        "worktree": "/scratch/wt"});
    jobs.create_job(&gate).await.expect("the gate-run is filed");
    let command = Uuid::new_v4();
    let submission = Submission {
        contract_version: 1,
        command,
        submitted_by: submitter.into(),
        supervisor: "automation:train-conductor".into(),
        intent: LaunchIntent {
            contract_version: 1,
            packet: gate.id.to_string(),
            namespace: "boss-gates".into(),
            name: "gate-x".into(),
            head: "abc123".into(),
            mode: "auto".into(),
            transition_generation: None,
            file_id: "file".into(),
            sha256: "0".repeat(64),
            size_bytes: 1,
        },
        source: gate.metadata.as_object().cloned().unwrap(),
    };
    let command: Command = serde_json::from_value(json!({
        "contract_version": 2, "idempotency_key": command,
        "expected": {"gate_submission": {"present": false}},
        "patch": {"gate_submission": serde_json::to_value(&submission).unwrap()},
        "immutable_keys": ["gate_submission"]}))
    .unwrap();
    let stamp = EventStamp::new("jobs", ActorId::human(submitter));
    jobs.conditional_job_metadata_at(&gate.id, &command, None, &stamp)
        .await
        .expect("the server accepts the submission")
}

/// PATCH a hand-off record onto `run` through the metadata merge door,
/// signed by the builder — what `boss gate` sends.
async fn patch_hand_off(
    app: &axum::Router,
    run: &JobId,
    record: &serde_json::Value,
) -> (StatusCode, String) {
    let resp = TestRequest::new(Method::PATCH, format!("/api/jobs/{run}/metadata"))
        .as_user(ACTOR, "platform-admin")
        .json(&json!({ boss_jobs::gate_handoff::KEY: record }))
        .send(app)
        .await;
    (
        resp.status,
        String::from_utf8_lossy(&resp.body_bytes).into_owned(),
    )
}

/// Hand `run`'s car to the gate the way `boss gate` records it: a real
/// submission, then its receipt copied onto the run.
async fn hand_to_gate(app: &axum::Router, jobs: &InMemoryJobs, run: &Job, submitter: &str) {
    let receipt = submit_gate(jobs, &run.id.to_string(), submitter).await;
    let record = boss_jobs::gate_handoff::record(&run.id.to_string(), &receipt)
        .expect("an exact acceptance naming the run");
    let (status, body) = patch_hand_off(app, &run.id, &record).await;
    assert!(
        status.is_success(),
        "the merge door takes the verified hand-off: {status} {body}"
    );
}

/// A record shaped exactly like a hand-off, typed by hand, with no
/// gate-run behind it.
fn forged(run: &Job) -> serde_json::Value {
    json!({"schema": 1, "run": run.id.to_string(), "gate_run": Uuid::new_v4().to_string(),
        "command": Uuid::new_v4().to_string(), "accepted_at": "2026-11-06T17:00:00Z",
        "accepted_by": ACTOR, "event_id": Uuid::new_v4().to_string(),
        "receipt_event_id": Uuid::new_v4().to_string(), "branch": "feat/x", "sha": "abc123"})
}

/// One open run of the builder's, and the app to write it through.
async fn a_run() -> (Arc<InMemoryJobs>, axum::Router, Job) {
    let jobs = store();
    let waiting = open_run();
    jobs.create_job(&waiting).await.unwrap();
    let app = app(jobs.clone(), agents().await);
    (jobs, app, waiting)
}

/// The run's row carries no hand-off.
async fn unhanded(jobs: &InMemoryJobs, run: &Job) {
    let stored = jobs.get_job(&run.id).await.unwrap().unwrap();
    assert!(
        stored.metadata.get(boss_jobs::gate_handoff::KEY).is_none(),
        "no hand-off was recorded: {}",
        stored.metadata
    );
}

/// A HAND-WRITTEN HAND-OFF ESCAPES NOTHING (adversarial finding 1 of run
/// 77fa2b8e). A record shaped exactly like the gate's, with no gate-run
/// behind it, is refused at the merge door, and nothing is recorded.
#[tokio::test]
async fn a_hand_written_hand_off_with_no_gate_run_behind_it_is_refused() {
    let (jobs, app, waiting) = a_run().await;
    let (status, body) = patch_hand_off(&app, &waiting.id, &forged(&waiting)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    unhanded(&jobs, &waiting).await;
}

/// A real gate submission for ANOTHER run, its record copied onto this one
/// with the run rewritten: the gate's own submission names the other run,
/// so the door refuses it.
#[tokio::test]
async fn a_hand_off_whose_gate_names_another_run_is_refused() {
    let (jobs, app, waiting) = a_run().await;
    // The other run is named only by its id.
    let other = open_run();
    let receipt = submit_gate(&jobs, &other.id.to_string(), ACTOR).await;
    let mut borrowed = boss_jobs::gate_handoff::record(&other.id.to_string(), &receipt).unwrap();
    borrowed["run"] = json!(waiting.id.to_string());
    let (status, body) = patch_hand_off(&app, &waiting.id, &borrowed).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // The right gate-run, with one copied field altered: the receipt's
    // event id no longer matches what the server retained.
    let receipt = submit_gate(&jobs, &waiting.id.to_string(), ACTOR).await;
    let mut altered = boss_jobs::gate_handoff::record(&waiting.id.to_string(), &receipt).unwrap();
    altered["receipt_event_id"] = json!(Uuid::new_v4().to_string());
    let (status, body) = patch_hand_off(&app, &waiting.id, &altered).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    unhanded(&jobs, &waiting).await;
}

/// The two other doors that write job metadata cannot carry it either: a
/// job PUT keeps the stored (absent) record, and a packet cannot be filed
/// already handed.
#[tokio::test]
async fn the_put_and_create_doors_cannot_write_a_hand_off() {
    let (jobs, app, waiting) = a_run().await;
    let mut body = serde_json::to_value(&waiting).unwrap();
    body["metadata"][boss_jobs::gate_handoff::KEY] = forged(&waiting);
    let resp = TestRequest::put(format!("/api/jobs/{}", waiting.id))
        .as_user(ACTOR, "platform-admin")
        .json(&body)
        .send(&app)
        .await;
    assert!(
        resp.status.is_success() || resp.status.is_client_error(),
        "{} {}",
        resp.status,
        String::from_utf8_lossy(&resp.body_bytes)
    );
    unhanded(&jobs, &waiting).await;

    let mut born = open_run();
    born.metadata[boss_jobs::gate_handoff::KEY] = forged(&born);
    let resp = TestRequest::post("/api/jobs")
        .as_user(ACTOR, "platform-admin")
        .json(&born)
        .send(&app)
        .await;
    assert_eq!(
        resp.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&resp.body_bytes)
    );
    assert!(jobs.get_job(&born.id).await.unwrap().is_none());
}

/// A job PUT that READ the run before its hand-off landed cannot erase it
/// (Codex review a4f47f9d, R1 P2 on car ff3b1733). The PUT replaces the
/// whole metadata, and carrying the stored record forward from its own
/// read is not enough: a verified hand-off committing between that read
/// and the write was saved over, the PUT answered 204, and the next
/// claim at the cap was refused for a run that had handed its car to the
/// gate. The PUT now sends the record as it read it as a precondition,
/// judged under the write's lock. A row that moved is refused 409,
/// nothing is written, and the hand-off stands.
#[tokio::test]
async fn a_put_that_read_the_run_before_its_hand_off_landed_writes_nothing() {
    let (jobs, app, waiting) = a_run().await;
    let receipt = submit_gate(&jobs, &waiting.id.to_string(), ACTOR).await;
    let record = boss_jobs::gate_handoff::record(&waiting.id.to_string(), &receipt).unwrap();
    boss_jobs::gate_handoff::verify(&waiting, &record, &receipt.job, &receipt).unwrap();
    let landing = record.clone();
    jobs.change_job_after_next_read(&waiting.id, move |row| {
        row.metadata[boss_jobs::gate_handoff::KEY] = landing;
    });
    let resp = TestRequest::put(format!("/api/jobs/{}", waiting.id))
        .as_user(ACTOR, "platform-admin")
        .json(&serde_json::to_value(&waiting).unwrap())
        .send(&app)
        .await;
    assert_eq!(
        resp.status,
        StatusCode::CONFLICT,
        "a PUT judged on a row without the hand-off must not save over one: {}",
        String::from_utf8_lossy(&resp.body_bytes)
    );
    let stored = jobs.get_job(&waiting.id).await.unwrap().unwrap();
    assert_eq!(
        stored.metadata.get(boss_jobs::gate_handoff::KEY),
        Some(&record),
        "the hand-off that landed after the PUT's read was erased: {}",
        stored.metadata
    );
}

/// The sequential control: a PUT that read the run already handed keeps
/// the record and is written.
#[tokio::test]
async fn a_put_that_read_the_run_already_handed_keeps_its_hand_off() {
    let (jobs, app, waiting) = a_run().await;
    hand_to_gate(&app, &jobs, &waiting, ACTOR).await;
    let before = jobs.get_job(&waiting.id).await.unwrap().unwrap();
    let resp = TestRequest::put(format!("/api/jobs/{}", waiting.id))
        .as_user(ACTOR, "platform-admin")
        .json(&serde_json::to_value(&before).unwrap())
        .send(&app)
        .await;
    assert!(
        resp.status.is_success(),
        "{} {}",
        resp.status,
        String::from_utf8_lossy(&resp.body_bytes)
    );
    let after = jobs.get_job(&waiting.id).await.unwrap().unwrap();
    assert!(after.metadata.get(boss_jobs::gate_handoff::KEY).is_some());
    assert_eq!(
        after.metadata.get(boss_jobs::gate_handoff::KEY),
        before.metadata.get(boss_jobs::gate_handoff::KEY)
    );
}
