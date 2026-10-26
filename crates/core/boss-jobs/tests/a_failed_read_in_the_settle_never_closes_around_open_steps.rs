//! A step write whose settle cannot read what it judges answers a 500,
//! never a close around open steps and never a 204 over an unsettled
//! packet (backlog 140e7984).
//!
//! WHY THIS FILE EXISTS. After a step write commits, the settle closes
//! the packet two ways, and each read a failure as an answer:
//!
//! - the declared-terminal close (`close_job_on_terminal`) skipped the
//!   packet's open steps under `if let Ok(steps) = list_steps_versioned`
//!   and then closed it regardless, so a steps read the store could not
//!   answer left a CLOSED packet with open steps — the closure property
//!   of the correctness protocol, broken;
//! - the all-steps-terminal catch-all read the packet under
//!   `if let Ok(Some(job)) = get_job`, so a failed read was "nothing to
//!   close" and the write answered 204 over a packet left open with
//!   every step finished — the shape de9b48de fixed for the catch-all's
//!   steps read one line above it.
//!
//! Builder run 68429685 found both while building de9b48de. Neither is
//! reachable with a one-shot failure: the readiness pass reads the steps
//! before the terminal close does, and the step door reads the packet
//! before the catch-all does, so the first failure lands on an earlier
//! read that already refuses. So each sweep fails EVERY read position in
//! turn — the in-memory adapter's counted injections, the next `n`
//! reads answering and every later one failing — and holds the same
//! three facts at each: a closed packet has no open step, a success
//! answer means the packet settled, and a refusal names the packet and
//! is repaired by the re-send it asks for. A position nobody knew about
//! is covered the day it is added.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{JobId, JobStatus, StepStatus};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Two declared terminals, both ready at admission: completing the
/// first closes the packet through `close_job_on_terminal`, which must
/// skip the second.
const BY_TERMINAL: &str = "closes-by-terminal";
/// One step that is no terminal: completing it closes the packet
/// through the all-steps-terminal catch-all.
const BY_CATCH_ALL: &str = "closes-by-catch-all";

fn step(title: &str, outcome: Option<&str>) -> StepSpec {
    StepSpec {
        title: title.into(),
        kind: "task".into(),
        ready_when: "true".into(),
        title_template: title.into(),
        authority_role: Some("platform-admin".into()),
        terminal: outcome.map(|o| Terminal { outcome: o.into() }),
        ..Default::default()
    }
}

fn spec(kind: &str, steps: Vec<StepSpec>) -> WorkflowSpec {
    let mut spec =
        WorkflowSpec::platform_seed(kind, kind, "platform", vec!["custom".into()], steps);
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec
}

struct AdminRoster;

#[async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-bootstrap-admin",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds
        .seed(spec(
            BY_TERMINAL,
            vec![step("work", Some("done")), step("abort", Some("aborted"))],
        ))
        .expect("seed the terminal kind");
    kinds
        .seed(spec(BY_CATCH_ALL, vec![step("work", None)]))
        .expect("seed the catch-all kind");
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
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
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

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A fresh store holding one open packet of `kind`, and the id of its
/// first step — the one the write completes.
async fn open(kind: &str) -> (axum::Router, Arc<InMemoryJobs>, JobId, String) {
    let (app, jobs) = app();
    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": kind,
                    "subject": { "subject_kind": "custom", "id": "/system/flow" },
                    "title": format!("A {kind} packet"),
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": {},
                    "tags": ["test"],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {body}");
    let job: serde_json::Value = serde_json::from_str(&body).expect("job json");
    let id =
        JobId::from_uuid(uuid::Uuid::parse_str(job["id"].as_str().expect("id")).expect("uuid"));
    let steps = jobs.list_steps(&id).await.expect("steps");
    let first = steps.first().expect("a materialised step").id.to_string();
    (app, jobs, id, first)
}

async fn complete(app: &axum::Router, id: &JobId, step: &str) -> (StatusCode, String) {
    send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{id}/steps/{step}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed" }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

#[derive(Clone, Copy, Debug)]
enum Read {
    Steps,
    Packet,
}

fn reads(jobs: &InMemoryJobs, id: &JobId, read: Read) -> usize {
    match read {
        Read::Steps => jobs.steps_reads(id),
        Read::Packet => jobs.job_reads(id),
    }
}

fn fail_after(jobs: &InMemoryJobs, id: &JobId, read: Read, n: usize) {
    match read {
        Read::Steps => jobs.fail_steps_read_after(id, n),
        Read::Packet => jobs.fail_job_read_after(id, n),
    }
}

fn restore(jobs: &InMemoryJobs, id: &JobId, read: Read) {
    match read {
        Read::Steps => jobs.restore_steps_read(id),
        Read::Packet => jobs.restore_job_read(id),
    }
}

/// The packet as the store holds it: its status, and the steps still
/// open (`Pending` / `Ready` / `Active`) by title.
async fn held(jobs: &InMemoryJobs, id: &JobId) -> (JobStatus, Vec<String>) {
    let job = jobs
        .get_job(id)
        .await
        .expect("job read")
        .expect("job exists");
    let open = jobs
        .list_steps(id)
        .await
        .expect("steps read")
        .into_iter()
        .filter(|s| {
            matches!(
                s.status,
                StepStatus::Pending | StepStatus::Ready | StepStatus::Active
            )
        })
        .map(|s| format!("{} ({:?})", s.title, s.status))
        .collect();
    (job.status, open)
}

/// How many `read`s of the packet one clean completion makes — the
/// positions a sweep fails in turn — after checking that clean
/// completion settles the packet (the control every sweep leans on).
async fn positions(kind: &str, read: Read) -> usize {
    let (app, jobs, id, step) = open(kind).await;
    let before = reads(&jobs, &id, read);
    let (status, body) = complete(&app, &id, &step).await;
    let made = reads(&jobs, &id, read) - before;
    assert_eq!(status, StatusCode::NO_CONTENT, "control {kind}: {body}");
    let (job, open) = held(&jobs, &id).await;
    assert_eq!(
        (job, open.as_slice()),
        (JobStatus::Closed, &[] as &[String]),
        "control {kind}: a clean completion closes the packet with nothing open"
    );
    assert!(
        made > 0,
        "control {kind}: the write reads the {read:?} at all"
    );
    made
}

/// Fail each `read` position of a completion of a `kind` packet in turn
/// and return every position at which the three facts did not hold,
/// each named in full.
async fn sweep(kind: &str, read: Read) -> Vec<String> {
    let mut breaches = Vec::new();
    for n in 0..positions(kind, read).await {
        let at = format!("{kind}, {read:?} read #{} failed", n + 1);
        let (app, jobs, id, step) = open(kind).await;
        fail_after(&jobs, &id, read, n);
        let (status, body) = complete(&app, &id, &step).await;
        restore(&jobs, &id, read);
        let (job, open) = held(&jobs, &id).await;
        if job == JobStatus::Closed && !open.is_empty() {
            breaches.push(format!(
                "{at}: CLOSED with open steps {open:?} (answered {status})"
            ));
        }
        if status.is_success() {
            if job != JobStatus::Closed || !open.is_empty() {
                breaches.push(format!(
                    "{at}: answered {status} over an unsettled packet ({job:?}, open {open:?})"
                ));
            }
            continue;
        }
        if !body.contains(&id.to_string()) {
            breaches.push(format!(
                "{at}: the {status} does not name the packet: {body}"
            ));
        }
        let (again, again_body) = complete(&app, &id, &step).await;
        let (job, open) = held(&jobs, &id).await;
        if !again.is_success() || job != JobStatus::Closed || !open.is_empty() {
            breaches.push(format!(
                "{at}: the re-send its {status} asked for did not settle the packet \
                 (answered {again}: {again_body}; {job:?}, open {open:?})"
            ));
        }
    }
    breaches
}

#[tokio::test]
async fn a_failed_steps_read_never_closes_a_terminal_packet_around_open_steps() {
    let breaches = sweep(BY_TERMINAL, Read::Steps).await;
    assert!(breaches.is_empty(), "{breaches:#?}");
}

#[tokio::test]
async fn a_failed_packet_read_never_answers_success_over_an_unsettled_packet() {
    let breaches = sweep(BY_CATCH_ALL, Read::Packet).await;
    assert!(breaches.is_empty(), "{breaches:#?}");
}

#[tokio::test]
async fn a_failed_steps_read_never_answers_success_over_a_catch_all_packet() {
    let breaches = sweep(BY_CATCH_ALL, Read::Steps).await;
    assert!(breaches.is_empty(), "{breaches:#?}");
}

#[tokio::test]
async fn a_failed_packet_read_never_closes_a_terminal_packet_around_open_steps() {
    let breaches = sweep(BY_TERMINAL, Read::Packet).await;
    assert!(breaches.is_empty(), "{breaches:#?}");
}
