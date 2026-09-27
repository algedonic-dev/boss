//! Opening a packet asks the policy for Create on job.
//!
//! Backlog f306b249 (from the builder of the sim-bypass car, 85e7f10f,
//! 2026-09-25; triage measured it on origin/main dbcb3854): `POST
//! /api/jobs` asked the policy nothing. Its one check was that the
//! subject exists, so any identified caller that was not read-only at
//! the gateway — and any caller at all on a door behind it — could open
//! a packet of any kind. Every other write door on this service asks
//! (Update and Close on the job PUT, Update on the step PUT, Create on
//! a workflow draft); admission was the one that did not.
//!
//! The grant this asks for is the one the platform already ships:
//! `boss_policy_client::defaults` grants Create on job to
//! `platform-admin` and `break-glass`, and the live policy on
//! 2026-09-27 holds exactly those two rows for it. So these tests run
//! against THOSE rules, not a hand-picked allow list — a caller refused
//! here is one the live instance would refuse.
//!
//! Before landing, every live opener was enumerated and read against
//! those rows (the record of `jobs.job.created` by actor over 36 hours,
//! 4,218 opens): each signs `platform-admin` — the CLI, the conductor,
//! the dispatcher's handlers, the maintenance timer, the ops runner,
//! the gateway's inquiry door, the tenant seed, David's session —
//! except the dispatcher's `jobs.spawn`, which signed role `system`
//! and rides this car signed as the dispatcher's one identity.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::JobId;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    Action, Decision, FakePolicyClient, PolicyClient, PolicyClientError, Predicate, Resource, User,
};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const CREATED: &str = "jobs.job.created";

/// The rules core ships — the ones a fresh instance is seeded with and
/// the live instance's job rows match.
fn shipped_policy() -> Arc<dyn PolicyClient> {
    let builder = boss_policy_client::defaults::default_rules()
        .into_iter()
        .filter(|r| r.active)
        .fold(FakePolicyClient::builder(), |b, r| {
            b.allow(r.role, r.action, r.resource, r.scope)
        });
    Arc::new(builder.build())
}

fn harness(
    policy: Arc<dyn PolicyClient>,
) -> (axum::Router, Arc<InMemoryJobs>, Arc<RecordingEventBus>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus.clone(),
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs, bus)
}

fn header(id: &str, role: &str) -> String {
    json!({
        "id": id,
        "role": role,
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn body(id: &str) -> Value {
    json!({
        "id": id,
        "kind": "backlog-item",
        "subject": { "subject_kind": "custom", "id": "/system/policy" },
        "title": "A packet someone opens",
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard",
        "status": "open",
        "metadata": {},
        "tags": [],
    })
}

async fn open(app: &axum::Router, who: Option<String>, body: &Value) -> (StatusCode, String) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/jobs")
        .header("content-type", "application/json");
    if let Some(h) = who {
        req = req.header("x-boss-user", h);
    }
    let resp = app
        .clone()
        .oneshot(
            req.body(Body::from(body.to_string()))
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn id(n: u8) -> String {
    format!("f306b249-0000-4000-8000-0000000000{n:02}")
}

/// `jobs.job.created` events recorded anywhere admission writes them:
/// the repository's outbox (the in-memory analogue of the Pg adapter's
/// in-transaction write) and the bus.
fn created(jobs: &InMemoryJobs, bus: &RecordingEventBus) -> usize {
    jobs.recorded_events()
        .iter()
        .filter(|e| e.kind == CREATED)
        .count()
        + bus.events_by_kind(CREATED).len()
}

async fn admitted(jobs: &InMemoryJobs, id: &str) -> bool {
    let id = JobId::from_uuid(uuid::Uuid::parse_str(id).expect("fixture id is a uuid"));
    jobs.get_job(&id).await.expect("in-memory read").is_some()
}

#[tokio::test]
async fn a_caller_the_policy_grants_no_create_on_job_opens_nothing() {
    // Every role the shipped rules give no Create on job — including
    // `system`, which `jobs.spawn` signed as until this car, and a
    // request that names no one at all (the extractor's guest).
    let refused: Vec<(&str, Option<String>)> = vec![
        ("system", Some(header("rule:some-rule", "system"))),
        (
            "audit-readonly",
            Some(header("automation:a-reader", "audit-readonly")),
        ),
        (
            "smoke-tester",
            Some(header("automation:smoke", "smoke-tester")),
        ),
        ("visitor", Some(header("someone", "visitor"))),
        ("a tenant role", Some(header("emp-clerk", "clerk"))),
        ("no header", None),
    ];
    for (n, (label, who)) in refused.into_iter().enumerate() {
        let (app, jobs, bus) = harness(shipped_policy());
        let packet = id(n as u8);
        let (status, text) = open(&app, who, &body(&packet)).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{label}: opening a packet must ask policy for Create on job; got {status}: {text}"
        );
        assert!(
            !admitted(&jobs, &packet).await,
            "{label}: a refused open admitted the packet"
        );
        assert!(
            created(&jobs, &bus) == 0,
            "{label}: a refused open recorded {CREATED}"
        );
    }
}

#[tokio::test]
async fn every_role_the_live_openers_sign_with_is_admitted() {
    // platform-admin: the CLI, the conductor, the dispatcher (its
    // handlers and, with this car, jobs.spawn), every infra script,
    // the gateway's own identity and David's session. break-glass: the
    // emergency session, which holds Create on job by design.
    for (n, role) in ["platform-admin", "break-glass"].into_iter().enumerate() {
        let (app, jobs, bus) = harness(shipped_policy());
        let packet = id(50 + n as u8);
        let (status, text) = open(
            &app,
            Some(header("automation:opener", role)),
            &body(&packet),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{role}: {text}");
        assert!(admitted(&jobs, &packet).await, "{role}: not admitted");
        assert_eq!(created(&jobs, &bus), 1, "{role}");
    }
}

#[tokio::test]
async fn a_refused_caller_learns_nothing_about_an_id_already_admitted() {
    // The already-admitted answer (558396ff) reads the packet an id
    // names and reports which fields differ. Asked of a caller with no
    // Create, that is a read the policy never granted: the refusal comes
    // first, and it is the same 403 whether or not the id exists.
    let (app, _jobs, _bus) = harness(shipped_policy());
    let packet = id(90);
    let admin = Some(header("automation:opener", "platform-admin"));
    let (status, text) = open(&app, admin, &body(&packet)).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    let mut differing = body(&packet);
    differing["title"] = json!("Something else entirely");
    let (status, text) = open(&app, Some(header("someone", "visitor")), &differing).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert!(
        !text.contains("differing_fields") && !text.contains("already_admitted"),
        "a refused caller was told about the packet: {text}"
    );
}

/// A policy service that cannot be asked.
struct DarkPolicy;

#[async_trait]
impl PolicyClient for DarkPolicy {
    async fn check(
        &self,
        _user: &User,
        _action: Action,
        _resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        Err(PolicyClientError::Unreachable("dark".into()))
    }
    async fn scope_predicate(
        &self,
        _user: &User,
        _resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        Err(PolicyClientError::Unreachable("dark".into()))
    }
}

#[tokio::test]
async fn an_open_during_a_policy_outage_is_503_and_admits_nothing() {
    // Fail closed, and say which kind of closed (backlog 45553536): an
    // outage is not a permission fact, so it is not a 403.
    let (app, jobs, bus) = harness(Arc::new(DarkPolicy));
    let packet = id(99);
    let (status, text) = open(
        &app,
        Some(header("automation:opener", "platform-admin")),
        &body(&packet),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    assert!(!admitted(&jobs, &packet).await);
    assert_eq!(created(&jobs, &bus), 0);
}
