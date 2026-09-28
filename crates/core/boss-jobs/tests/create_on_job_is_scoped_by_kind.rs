//! Create on job is scoped by kind (design 222fc982, decided by David
//! 2026-09-27: option (a); backlog 6dc75abd).
//!
//! Since train #734 `POST /api/jobs` asked Create on `job`, and live only
//! platform-admin and break-glass hold it, so every tenant role lost the
//! feedback widget and packet creation. Granting a role plain Create on
//! `job` would have admitted every kind — an `ops-request` as readily as
//! a `user-feedback`. So admission asks for the KIND being opened: a rule
//! grants Create on `job:<kind>`, and plain `job` stays the all-kinds
//! grant the platform roles already hold.
//!
//! These run against the rules core ships PLUS one narrow grant, so the
//! platform half is the live instance's own rows, not a hand-picked list.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::JobId;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const CREATED: &str = "jobs.job.created";

/// A tenant role that files feedback and nothing else.
const FILER: &str = "taproom-server";

/// The rules core ships, plus Create on `job:user-feedback` for
/// [`FILER`] — the grant a tenant seed writes for each of its roles.
fn policy() -> Arc<dyn PolicyClient> {
    let builder = boss_policy_client::defaults::default_rules()
        .into_iter()
        .filter(|r| r.active)
        .fold(FakePolicyClient::builder(), |b, r| {
            b.allow(r.role, r.action, r.resource, r.scope)
        })
        .allow(
            FILER,
            Action::Create,
            Resource::job_of_kind("user-feedback"),
            Scope::All,
        );
    Arc::new(builder.build())
}

fn harness() -> (axum::Router, Arc<InMemoryJobs>, Arc<RecordingEventBus>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus.clone(),
        DomainPublisher::new(bus_dyn, "jobs"),
        policy(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs, bus)
}

fn header(id: &str, role: &str) -> Option<String> {
    Some(
        json!({
            "id": id,
            "role": role,
            "access_tier": "user",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "taproom",
        })
        .to_string(),
    )
}

fn body(id: &str, kind: &str) -> Value {
    json!({
        "id": id,
        "kind": kind,
        "subject": { "subject_kind": "custom", "id": "/me" },
        "title": format!("A {kind} someone opens"),
        "owner_id": "emp-filer",
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
    format!("6dc75abd-0000-4000-8000-0000000000{n:02}")
}

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
async fn a_role_holding_create_on_job_user_feedback_files_feedback() {
    let (app, jobs, bus) = harness();
    let packet = id(1);
    let (status, text) = open(
        &app,
        header("emp-filer", FILER),
        &body(&packet, "user-feedback"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert!(admitted(&jobs, &packet).await);
    assert_eq!(created(&jobs, &bus), 1);
}

#[tokio::test]
async fn the_same_role_is_refused_every_other_kind_and_told_which_grant() {
    for (n, kind) in ["ops-request", "backlog-item", "user-feedback-2"]
        .into_iter()
        .enumerate()
    {
        let (app, jobs, bus) = harness();
        let packet = id(10 + n as u8);
        let (status, text) = open(&app, header("emp-filer", FILER), &body(&packet, kind)).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{kind}: a user-feedback grant opened another kind: {text}"
        );
        // The refusal names the grant a tenant would add, so the fix is
        // one read away rather than a re-derivation.
        assert!(
            text.contains(&format!("job:{kind}")),
            "{kind}: the refusal does not name job:{kind}: {text}"
        );
        assert!(!admitted(&jobs, &packet).await, "{kind}: admitted");
        assert_eq!(created(&jobs, &bus), 0, "{kind}: recorded {CREATED}");
    }
}

#[tokio::test]
async fn platform_admin_still_files_any_kind_on_its_all_kinds_grant() {
    // Every live opener signs platform-admin (the CLI, the conductor,
    // the dispatcher's rules, the ops runner, David's session), and
    // break-glass holds the same grant for the rollback lever. Neither
    // holds a single `job:<kind>` row; plain `job` is every kind.
    for (n, role) in ["platform-admin", "break-glass"].into_iter().enumerate() {
        for (m, kind) in ["user-feedback", "ops-request", "backlog-item"]
            .into_iter()
            .enumerate()
        {
            let (app, jobs, bus) = harness();
            let packet = id(20 + (n * 3 + m) as u8);
            let (status, text) = open(
                &app,
                header("automation:opener", role),
                &body(&packet, kind),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{role} / {kind}: {text}");
            assert!(admitted(&jobs, &packet).await, "{role} / {kind}");
            assert_eq!(created(&jobs, &bus), 1, "{role} / {kind}");
        }
    }
}

#[tokio::test]
async fn a_role_with_no_grant_of_either_shape_is_still_refused() {
    let (app, jobs, _bus) = harness();
    let packet = id(40);
    let (status, text) = open(
        &app,
        header("emp-clerk", "clerk"),
        &body(&packet, "user-feedback"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert!(!admitted(&jobs, &packet).await);
}

#[tokio::test]
async fn a_body_with_no_kind_asks_the_all_kinds_grant_and_is_refused_first() {
    // No kind to scope by: a narrow holder is asked for the grant it does
    // not hold, and learns that before any shape error about the body.
    let (app, _jobs, _bus) = harness();
    let mut no_kind = body(&id(41), "user-feedback");
    no_kind.as_object_mut().expect("object").remove("kind");
    let (status, text) = open(&app, header("emp-filer", FILER), &no_kind).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
}

#[tokio::test]
async fn a_narrow_holder_learns_nothing_of_a_packet_of_another_kind_under_its_id() {
    // The already-admitted answer (558396ff) names the fields a re-sent
    // body differs in. Asked by a caller that may open `user-feedback`
    // about an id that names an `ops-request`, that is a read of a
    // packet the caller could never have opened: refused, with none of
    // its fields named.
    let (app, _jobs, _bus) = harness();
    let packet = id(50);
    let (status, text) = open(
        &app,
        header("automation:opener", "platform-admin"),
        &body(&packet, "ops-request"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    let (status, text) = open(
        &app,
        header("emp-filer", FILER),
        &body(&packet, "user-feedback"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert!(
        !text.contains("differing_fields")
            && !text.contains("already_admitted")
            && !text.contains("ops-request"),
        "a caller that may not open the packet's kind was told about it: {text}"
    );
}

#[tokio::test]
async fn a_narrow_holder_re_sending_its_own_feedback_is_answered_as_before() {
    // The idempotent re-send still answers 200 for the kind it may open.
    let (app, jobs, bus) = harness();
    let packet = id(60);
    let who = header("emp-filer", FILER);
    let first = body(&packet, "user-feedback");
    let (status, text) = open(&app, who.clone(), &first).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, text) = open(&app, who, &first).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("already_admitted"), "{text}");
    assert_eq!(created(&jobs, &bus), 1);
}
