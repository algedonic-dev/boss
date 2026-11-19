//! A packet's end that could not skip one of its open steps, or could
//! not read the protocol it is pinned to, answers a 500 naming what it
//! could not do — never a close around an open step, never a close
//! without the outcome its completed terminal names, and never a 204
//! over a packet left open (backlog 0887d2a9).
//!
//! WHY THIS FILE EXISTS. Builder run 13147333 found these beside the
//! failed reads car 140e7984 refuses (the sweeps in
//! `a_failed_read_in_the_settle_never_closes_around_open_steps.rs`):
//!
//! - `skip_open_step` returned nothing. A skip write the store refused,
//!   a step that kept moving under all three attempts, or a re-read
//!   that failed after it moved was a `tracing::warn!` and a return —
//!   and both of its callers closed the packet regardless: the
//!   declared-terminal close (`close_job_on_terminal`), leaving a CLOSED
//!   packet with an open step that no re-send can repair, because the
//!   re-send finds the packet closed; and the job door's close/cancel,
//!   answering 204 over the same thing.
//! - The settle read the packet's pinned protocol under `if let Ok(..)`
//!   twice: a registry error before the declared-terminal close read as
//!   "the completed step is no terminal", so the write answered 204 with
//!   the terminal completed and the packet open; and one in the
//!   catch-all close read as "no outcome", so a packet whose terminal
//!   had completed closed without saying how it ended.
//!
//! Each sweep fails every position it can name — every open step's skip
//! in each of the three ways, every protocol read of the write in turn —
//! and holds the same facts at each: a closed packet has no open step
//! and carries its terminal's outcome, a success answer means the
//! packet settled, and a refusal names the packet (and the step it
//! could not skip) and is repaired by the re-send it asks for.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{JobId, JobStatus, StepStatus};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::in_memory::StepWriteFault;
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Two declared terminals and a step that is none, all ready at
/// admission: completing `work` closes the packet through
/// `close_job_on_terminal`, which must skip `abort` and `note`.
const TERMINAL: &str = "closes-by-terminal";
/// A terminal and a step that is none: with `work` completed and its
/// close not landed — the race of 228c9a7d — completing `note` closes
/// the packet through the catch-all, which names `work`'s outcome.
const LATE: &str = "closes-late-by-catch-all";

const FAULTS: [StepWriteFault; 3] = [
    StepWriteFault::Refused,
    StepWriteFault::KeepsMoving,
    StepWriteFault::MovedThenUnreadable,
];

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

struct World {
    app: axum::Router,
    jobs: Arc<InMemoryJobs>,
    kinds: Arc<InMemoryWorkflows>,
    id: JobId,
    kind: &'static str,
}

fn app() -> (axum::Router, Arc<InMemoryJobs>, Arc<InMemoryWorkflows>) {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds
        .seed(spec(
            TERMINAL,
            vec![
                step("work", Some("done")),
                step("abort", Some("aborted")),
                step("note", None),
            ],
        ))
        .expect("seed the terminal kind");
    kinds
        .seed(spec(
            LATE,
            vec![step("work", Some("done")), step("note", None)],
        ))
        .expect("seed the late kind");
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
            .allow("platform-admin", Action::Close, Resource::job(), Scope::All)
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
        kind_registry: Some(kinds.clone() as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs, kinds)
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

/// A fresh store holding one open packet of `kind`.
async fn open(kind: &'static str) -> World {
    let (app, jobs, kinds) = app();
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
    World {
        app,
        jobs,
        kinds,
        id,
        kind,
    }
}

/// The id of the packet's step titled `title`.
async fn step_id(w: &World, title: &str) -> boss_core::job::StepId {
    w.jobs
        .list_steps(&w.id)
        .await
        .expect("steps")
        .into_iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("no step {title}"))
        .id
}

async fn complete(w: &World, title: &str) -> (StatusCode, String) {
    let step = step_id(w, title).await;
    send(
        &w.app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{}/steps/{step}", w.id))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed" }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

/// End the packet through the job door: the row as stored, with `end`
/// as its status.
async fn end(w: &World, end: JobStatus) -> (StatusCode, String) {
    let mut job = w
        .jobs
        .get_job(&w.id)
        .await
        .expect("job read")
        .expect("job exists");
    job.status = end;
    send(
        &w.app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{}", w.id))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(serde_json::to_string(&job).expect("job body")))
            .unwrap(),
    )
    .await
}

/// The packet as the store holds it: its status, the steps still open
/// (`Pending` / `Ready` / `Active`) by title, and its outcome.
async fn held(w: &World) -> (JobStatus, Vec<String>, Option<String>) {
    let job = w
        .jobs
        .get_job(&w.id)
        .await
        .expect("job read")
        .expect("job exists");
    let open = w
        .jobs
        .list_steps(&w.id)
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
    let outcome = job
        .metadata
        .get("outcome")
        .and_then(|o| o.as_str())
        .map(str::to_string);
    (job.status, open, outcome)
}

/// The facts a step write that settles the packet holds at one failed
/// position, whatever it answered: a closed packet has no open step and
/// names `outcome`; a success means it closed; a refusal names the
/// packet and each of `named`, and the re-send it asks for — `resend` —
/// closes it whole.
async fn settled<F, Fut>(
    at: &str,
    w: &World,
    (status, body): (StatusCode, String),
    named: &[String],
    outcome: &str,
    resend: F,
) -> Vec<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = (StatusCode, String)>,
{
    let mut breaches = Vec::new();
    let (job, open, got) = held(w).await;
    if job == JobStatus::Closed && (!open.is_empty() || got.as_deref() != Some(outcome)) {
        breaches.push(format!(
            "{at}: CLOSED with open steps {open:?} and outcome {got:?} (answered {status})"
        ));
    }
    if status.is_success() {
        if job != JobStatus::Closed {
            breaches.push(format!(
                "{at}: answered {status} over an unsettled packet ({job:?}, open {open:?})"
            ));
        }
        return breaches;
    }
    for name in std::iter::once(&w.id.to_string()).chain(named) {
        if !body.contains(name.as_str()) {
            breaches.push(format!("{at}: the {status} does not name {name}: {body}"));
        }
    }
    let (again, again_body) = resend().await;
    let (job, open, got) = held(w).await;
    if !again.is_success()
        || job != JobStatus::Closed
        || !open.is_empty()
        || got.as_deref() != Some(outcome)
    {
        breaches.push(format!(
            "{at}: the re-send its {status} asked for did not settle the packet \
             (answered {again}: {again_body}; {job:?}, open {open:?}, outcome {got:?})"
        ));
    }
    breaches
}

#[tokio::test]
async fn a_skip_the_terminal_close_could_not_write_refuses_the_close() {
    let mut breaches = Vec::new();
    for fault in FAULTS {
        for victim in ["abort", "note"] {
            let at = format!("terminal close, {victim}'s skip {fault:?}");
            let w = open(TERMINAL).await;
            let victim_id = step_id(&w, victim).await;
            w.jobs.fault_step_write(&victim_id, fault);
            let answer = complete(&w, "work").await;
            w.jobs.restore_step_write(&victim_id);
            breaches.extend(
                settled(&at, &w, answer, &[victim_id.to_string()], "done", || {
                    complete(&w, "work")
                })
                .await,
            );
        }
    }
    assert!(breaches.is_empty(), "{breaches:#?}");
}

#[tokio::test]
async fn a_skip_the_job_door_could_not_write_refuses_the_end() {
    let mut breaches = Vec::new();
    for ending in [JobStatus::Closed, JobStatus::Cancelled] {
        for fault in FAULTS {
            for victim in ["work", "abort", "note"] {
                let at = format!("job door {ending:?}, {victim}'s skip {fault:?}");
                let w = open(TERMINAL).await;
                let victim_id = step_id(&w, victim).await;
                w.jobs.fault_step_write(&victim_id, fault);
                let (status, body) = end(&w, ending).await;
                w.jobs.restore_step_write(&victim_id);
                let (job, open, _) = held(&w).await;
                if status.is_success() {
                    if job != ending || !open.is_empty() {
                        breaches.push(format!(
                            "{at}: answered {status} with the packet {job:?}, open {open:?}"
                        ));
                    }
                    continue;
                }
                for name in [w.id.to_string(), victim_id.to_string()] {
                    if !body.contains(&name) {
                        breaches.push(format!("{at}: the {status} does not name {name}: {body}"));
                    }
                }
                let (again, again_body) = end(&w, ending).await;
                let (job, open, _) = held(&w).await;
                if !again.is_success() || job != ending || !open.is_empty() {
                    breaches.push(format!(
                        "{at}: the re-send its {status} asked for did not end the packet whole \
                         (answered {again}: {again_body}; {job:?}, open {open:?})"
                    ));
                }
            }
        }
    }
    assert!(breaches.is_empty(), "{breaches:#?}");
}

/// How many protocol reads one clean completion of `write` makes on a
/// [`prepared`] `kind` packet — the positions a sweep fails in turn —
/// after checking that the clean write closes the packet with `work`'s
/// outcome (the control the sweep leans on).
async fn positions(kind: &'static str, write: &str) -> usize {
    let w = prepared(kind).await;
    let before = w.kinds.version_reads(kind);
    let (status, body) = complete(&w, write).await;
    let made = w.kinds.version_reads(kind) - before;
    assert!(status.is_success(), "control {kind}: {status} {body}");
    let (job, open, outcome) = held(&w).await;
    assert_eq!(
        (job, open.as_slice(), outcome.as_deref()),
        (JobStatus::Closed, &[] as &[String], Some("done")),
        "control {kind}: a clean write closes the packet whole, with its outcome"
    );
    assert!(
        made > 0,
        "control {kind}: the write reads the protocol at all"
    );
    made
}

/// A fresh packet of `kind` in the state its sweep's write starts from:
/// a TERMINAL packet as admitted; a LATE packet with `work` completed
/// and its close not landed — the second completer's view in the race
/// of 228c9a7d, written straight to the store so no settle runs.
async fn prepared(kind: &'static str) -> World {
    let w = open(kind).await;
    if kind == LATE {
        let work = step_id(&w, "work").await;
        let (mut row, version) = w
            .jobs
            .get_step_versioned(&work)
            .await
            .expect("work read")
            .expect("work exists");
        row.status = StepStatus::Completed;
        w.jobs
            .update_step_if_unchanged_at(&row, version, chrono::Utc::now(), &[])
            .await
            .expect("work completed in the store");
    }
    w
}

/// Fail each protocol read of `write` on a `kind` packet in turn — the
/// next `n` reads answering and every later one failing — and return
/// every position at which the facts did not hold, each named in full.
async fn sweep(kind: &'static str, write: &'static str) -> Vec<String> {
    let mut breaches = Vec::new();
    for n in 0..positions(kind, write).await {
        let at = format!(
            "{kind}, completing {write}, protocol read #{} failed",
            n + 1
        );
        let w = prepared(kind).await;
        w.kinds.fail_version_read_after(w.kind, n);
        let answer = complete(&w, write).await;
        w.kinds.restore_version_read(w.kind);
        breaches.extend(settled(&at, &w, answer, &[], "done", || complete(&w, write)).await);
    }
    breaches
}

#[tokio::test]
async fn a_failed_protocol_read_never_answers_success_over_a_completed_terminal() {
    let breaches = sweep(TERMINAL, "work").await;
    assert!(breaches.is_empty(), "{breaches:#?}");
}

#[tokio::test]
async fn a_failed_protocol_read_never_closes_without_the_terminals_outcome() {
    let breaches = sweep(LATE, "note").await;
    assert!(breaches.is_empty(), "{breaches:#?}");
}
