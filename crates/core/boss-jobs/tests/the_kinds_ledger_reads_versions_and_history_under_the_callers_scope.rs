//! `GET /api/jobs/kinds` — every kind's packet ledger, for the caller.
//!
//! WHY IT EXISTS (backlogs 112c0535 and 5eacf6db, page audit 9da74410,
//! 2026-09-27). /it/registry showed each kind's active `v{n}` and an
//! "N in flight" chip, and nothing else about its packets. So two facts
//! an operator of the protocol layer must see were invisible there:
//!
//! - VERSION LAG. 374 of 540 open packets ran a version below their
//!   kind's active one (measured at triage) — a procedure edit that
//!   never reaches in-flight packets, drawn exactly like one that did.
//! - NEVER RUN. 16 of 64 active kinds had never had a packet, and were
//!   drawn exactly like ops-request with its 3847.
//!
//! The decision on both (`decided_2026-09-27_9da74410`) was a count the
//! jobs API serves rather than 540 rows fetched to the browser: per kind,
//! the open packets by the version each is pinned to, every packet ever,
//! and the newest terminal — the pair readiness already computes for a
//! department's own kinds.
//!
//! WHY A SCOPED ROUTE AND NOT `/api/jobs/live`. The decision named a
//! field beside `counts` on `/api/jobs/live`, but that endpoint is the
//! public landing window: it answers a headerless caller by design (the
//! PUBLIC list in `every_jobs_read_asks_policy_or_is_named_public.rs`),
//! and backlog 9274e151 is an open question about whether it should
//! answer that caller even what it does today. Packets-ever and the
//! newest terminal are CLOSED history — exactly what 19f08bd6 took out
//! of an anonymous caller's reach on `/api/jobs/summary`. So these facts
//! ride a read that asks `job_read_scope` like every packet read, and are
//! counted under the caller's scope; `/api/jobs/live` is pinned here as
//! not widened.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const BREWER: &str = "emp-brewer";
const OTHER: &str = "emp-other";
const URI: &str = "/api/jobs/kinds";

fn user(id: &str, role: &str, tier: AccessTier, department: Option<&str>) -> User {
    User {
        id: id.into(),
        role: role.into(),
        access_tier: tier,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: department.map(str::to_string),
    }
}

fn operator() -> Option<User> {
    Some(user(
        "emp-david",
        "platform-admin",
        AccessTier::Operator,
        Some("platform"),
    ))
}
fn guest() -> Option<User> {
    Some(user("visitor", "guest", AccessTier::User, None))
}
fn brewer() -> Option<User> {
    Some(user(BREWER, "brewer", AccessTier::User, Some("brewhouse")))
}
fn lead_outside_department() -> Option<User> {
    Some(user(
        "emp-lead-2",
        "brewhouse-lead",
        AccessTier::User,
        Some("sales"),
    ))
}

fn policy() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow("guest", Action::Read, Resource::workflow(), Scope::All)
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow("brewer", Action::Read, Resource::job(), Scope::Self_)
            .allow(
                "brewhouse-lead",
                Action::Read,
                Resource::job(),
                Scope::Department("brewhouse".into()),
            )
            .build(),
    )
}

/// `(kind, version, owner, status, closed_on day of September)`.
/// brew-day: three open over two versions, two closed — the brewer's
/// on the 21st, somebody else's on the 24th, so the newest terminal
/// the brewer may see is not the store's. sale: one open, never
/// closed. cellar-check: closed only.
const PACKETS: &[(&str, i32, &str, JobStatus, u32)] = &[
    ("brew-day", 1, BREWER, JobStatus::Open, 0),
    ("brew-day", 2, OTHER, JobStatus::Open, 0),
    ("brew-day", 2, OTHER, JobStatus::Open, 0),
    ("brew-day", 1, BREWER, JobStatus::Closed, 21),
    ("brew-day", 2, OTHER, JobStatus::Closed, 24),
    ("sale", 3, OTHER, JobStatus::Open, 0),
    ("cellar-check", 1, OTHER, JobStatus::Closed, 22),
];

fn packet(
    n: usize,
    (kind, version, owner, status, closed): (&str, i32, &str, JobStatus, u32),
) -> Job {
    let id = Uuid::parse_str(&format!("112c0535-0000-0000-0000-{n:012}")).expect("uuid");
    Job {
        id: JobId::from_uuid(id),
        kind: kind.into(),
        workflow_version: version,
        subject: Subject::new("asset", "FV-1"),
        title: format!("packet {n}"),
        owner_id: owner.into(),
        status,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 20).expect("day"),
        opened_at: None,
        due_on: None,
        closed_on: (status == JobStatus::Closed)
            .then(|| NaiveDate::from_ymd_opt(2026, 9, closed).expect("day")),
        metadata: if status == JobStatus::Closed {
            json!({ "outcome": format!("outcome-{n}") })
        } else {
            Value::Null
        },
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn id_of(n: usize) -> String {
    format!("112c0535-0000-0000-0000-{n:012}")
}

async fn app() -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    for (n, p) in PACKETS.iter().enumerate() {
        jobs.create_job(&packet(n, *p)).await.expect("packet");
    }
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState {
        step_registry: Arc::new(StepRegistry::v1()),
        ..JobsApiState::minimal(
            jobs,
            bus,
            publisher,
            policy(),
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

async fn send(app: &Router, uri: &str, who: &Option<User>) -> (StatusCode, String) {
    let mut req = Request::get(uri);
    if let Some(u) = who {
        req = req.header(
            "x-boss-user",
            serde_json::to_string(u).expect("a User serialises"),
        );
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn ledger(app: &Router, who: &Option<User>) -> Value {
    let (status, body) = send(app, URI, who).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(&body).expect("json body")
}

/// The one row for `kind`, or None when the ledger has none.
fn row<'a>(body: &'a Value, kind: &str) -> Option<&'a Value> {
    body["kinds"]
        .as_array()
        .expect("kinds is a list")
        .iter()
        .find(|r| r["kind"] == kind)
}

// ---------------------------------------------------------------------
// Refused: the ledger is a packet read, not the public window.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_headerless_or_guest_caller_is_refused_and_learns_no_kind() {
    let app = app().await;
    for who in [None, guest()] {
        let (code, body) = send(&app, URI, &who).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{who:?}: {body}");
        assert!(
            body.contains("reading packets is refused"),
            "{who:?}: the refusal is the packet-read one: {body}"
        );
        assert!(!body.contains("brew-day"), "{who:?} leaked a kind: {body}");
    }
}

// ---------------------------------------------------------------------
// Answered: versions, packets ever and the newest terminal.
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_all_scope_caller_reads_every_kinds_versions_and_history() {
    let body = ledger(&app().await, &operator()).await;

    let brew = row(&body, "brew-day").expect("brew-day");
    assert_eq!(brew["packets"], 5, "{brew}");
    assert_eq!(brew["open"], 3, "{brew}");
    assert_eq!(brew["open_by_version"], json!({ "1": 1, "2": 2 }), "{brew}");
    assert_eq!(brew["newest_terminal"]["id"], id_of(4), "{brew}");
    assert_eq!(brew["newest_terminal"]["outcome"], "outcome-4", "{brew}");
    assert_eq!(brew["newest_terminal"]["closed_on"], "2026-09-24", "{brew}");

    // Open, never closed: a ledger row with no terminal, not an absent row.
    let sale = row(&body, "sale").expect("sale");
    assert_eq!(sale["packets"], 1, "{sale}");
    assert_eq!(sale["open_by_version"], json!({ "3": 1 }), "{sale}");
    assert!(sale["newest_terminal"].is_null(), "{sale}");

    // Closed only: nothing in flight, so no version is lagging.
    let cellar = row(&body, "cellar-check").expect("cellar-check");
    assert_eq!(cellar["packets"], 1, "{cellar}");
    assert_eq!(cellar["open"], 0, "{cellar}");
    assert_eq!(cellar["open_by_version"], json!({}), "{cellar}");
    assert_eq!(cellar["newest_terminal"]["id"], id_of(6), "{cellar}");

    let kinds: Vec<&str> = body["kinds"]
        .as_array()
        .expect("list")
        .iter()
        .filter_map(|r| r["kind"].as_str())
        .collect();
    assert_eq!(
        kinds,
        ["brew-day", "cellar-check", "sale"],
        "sorted by kind"
    );
}

#[tokio::test]
async fn a_self_scoped_caller_counts_and_sees_only_its_own_packets() {
    let body = ledger(&app().await, &brewer()).await;
    let brew = row(&body, "brew-day").expect("brew-day");
    assert_eq!(brew["packets"], 2, "{brew}");
    assert_eq!(brew["open_by_version"], json!({ "1": 1 }), "{brew}");
    // The store's newest terminal is somebody else's; this caller's is
    // its own — the ledger never names a packet the list would not hand it.
    assert_eq!(brew["newest_terminal"]["id"], id_of(3), "{brew}");
    assert!(
        row(&body, "sale").is_none() && row(&body, "cellar-check").is_none(),
        "a kind the caller owns none of is absent: {body}"
    );
}

#[tokio::test]
async fn a_scope_that_admits_nothing_reads_an_empty_ledger() {
    let body = ledger(&app().await, &lead_outside_department()).await;
    assert_eq!(body["kinds"], json!([]), "{body}");
}

// ---------------------------------------------------------------------
// Unchanged: the public window did not widen (9274e151).
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_public_live_window_carries_none_of_the_ledger() {
    let (code, body) = send(&app().await, "/api/jobs/live", &None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let body: Value = serde_json::from_str(&body).expect("json");
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["counts", "open_total", "recent", "sim_clock"],
        "{body}"
    );
}
