//! Q's derived stations, through the HTTP surface that serves them.
//!
//! The projection's unit tests prove `constraints_of` and
//! `derived_stations` are right about a slice of `WorkflowSpec`. They
//! cannot prove the thing that matters to an operator: that a station
//! nobody authored shows up in `GET /api/stations` and that asking for
//! its queue returns packets. Both halves have to work, and the second
//! is where the naming defect lived — a name with a `/` in it stores
//! fine, lists fine, and 404s on the queue route.
//!
//! Three claims, all at the consuming layer:
//!
//! - a constrained step in a published protocol produces a station in
//!   the listing that no `stations` row declares;
//! - that station's queue answers, and holds the packets waiting on
//!   the step;
//! - an authored row of the same name wins, and appears once.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::station_queue::StationPredicate;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{
    InMemoryJobs, InMemoryStations, InMemoryWorkflows, StationKind, StationRegistry, StationSpec,
    WorkflowRegistry,
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The name the projection must produce for the protocol below.
/// Written out literally rather than computed, so that a change to
/// `station_name` fails HERE — at the URL an operator's browser asks
/// for — and not only in the unit test that agrees with itself.
const DERIVED: &str = "q.bookkeeper.sign-off";

fn user_header(id: &str, role: &str) -> String {
    serde_json::json!({
        "id": id,
        "role": role,
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "finance",
    })
    .to_string()
}

/// A protocol with a constrained step and an unconstrained one. Only
/// the constrained step declares a queue; `file` is the control.
fn bill_kind() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "vendor-bill",
        "Vendor bill",
        "test",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "file".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                title_template: "File the bill".into(),
                ..Default::default()
            },
            StepSpec {
                title: "approve".into(),
                kind: "sign-off".into(),
                ready_when: "true".into(),
                title_template: "Approve the bill".into(),
                authority_role: Some("bookkeeper".into()),
                ..Default::default()
            },
        ],
    )
}

/// One authored row, so the listing is never trivially all-derived and
/// the merge has something to merge with.
fn authored_dock() -> StationSpec {
    let mut s = StationSpec::draft(
        "loading-dock",
        "Loading dock",
        StationKind::Batch,
        StationPredicate {
            kind: Some("vendor-bill".into()),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s
}

fn app(extra: Option<StationSpec>) -> axum::Router {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(bill_kind()).unwrap();
    let stations = Arc::new(InMemoryStations::new());
    stations.seed(authored_dock()).unwrap();
    if let Some(s) = extra {
        stations.seed(s).unwrap();
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: Some(stations as Arc<dyn StationRegistry>),
        jobs: Arc::new(InMemoryJobs::new()),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

async fn get_json(app: &axum::Router, path: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::get(path)
                .header("x-boss-user", user_header("emp-ceo", "ceo"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn post_bill(app: &axum::Router, id: &str) {
    let body = serde_json::json!({
        "kind": "vendor-bill",
        "subject": { "subject_kind": "custom", "id": id },
        "title": format!("bill {id}"),
        "owner_id": "emp-ceo",
        "status": "open",
        "priority": "standard",
        "opened_on": "2026-08-16",
        "metadata": {},
        "tags": [],
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header("emp-ceo", "ceo"))
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let st = resp.status();
    let b = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(st, StatusCode::CREATED, "{}", String::from_utf8_lossy(&b));
}

fn row<'a>(v: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    v["data"].as_array()?.iter().find(|s| s["name"] == name)
}

/// The listing carries a station no `stations` row declares.
#[tokio::test]
async fn the_listing_includes_a_station_nobody_authored() {
    let app = app(None);
    let (status, v) = get_json(&app, "/api/stations").await;
    assert_eq!(status, StatusCode::OK);

    let derived = row(&v, DERIVED).unwrap_or_else(|| {
        panic!("`{DERIVED}` is declared by a protocol but absent from the listing: {v:#}")
    });
    assert_eq!(derived["kind"], "constraint");
    // The role reaches the claim gate, not just the title — a station
    // that shows a constraint without enforcing it is decoration.
    assert_eq!(
        derived["capability"]["roles"],
        serde_json::json!(["bookkeeper"])
    );
    assert_eq!(derived["predicate"]["step"]["kind"], "sign-off");

    // The unconstrained step declares no queue: `task` steps are not
    // waiting on anybody in particular, so projecting one would invent
    // a queue with no owner.
    assert!(
        row(&v, "q.bookkeeper.task").is_none(),
        "an unconstrained step must not project a station: {v:#}"
    );
    // And the authored row is still there — the projection ADDS.
    assert!(row(&v, "loading-dock").is_some(), "authored row lost");
}

/// Asking for the derived station's queue returns packets.
///
/// This is the half a unit test cannot reach. `GET
/// /api/stations/{name}/queue` routes on ONE path segment, so a name
/// containing `/` would list correctly here and 404 below.
#[tokio::test]
async fn a_derived_station_has_a_queue_that_answers() {
    let app = app(None);
    post_bill(&app, "bill-1").await;
    post_bill(&app, "bill-2").await;

    let (status, v) = get_json(&app, &format!("/api/stations/{DERIVED}/queue")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the listing advertises `{DERIVED}`; its queue must answer: {v:#}"
    );
    assert_eq!(v["station"], DERIVED);
    assert_eq!(v["kind"], "constraint");
    assert_eq!(v["total"], 2, "both open bills wait on the approval step");
}

/// An authored row of the same name wins, and appears once.
#[tokio::test]
async fn an_authored_row_of_the_same_name_wins_and_appears_once() {
    let mut authored = StationSpec::draft(
        DERIVED,
        "Bookkeeping — hand-tuned",
        StationKind::Constraint,
        StationPredicate {
            kind: Some("vendor-bill".into()),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    authored.status = boss_jobs::registry::WorkflowStatus::Active;
    authored.wip_limit = Some(5);

    let app = app(Some(authored));
    let (status, v) = get_json(&app, "/api/stations").await;
    assert_eq!(status, StatusCode::OK);

    let hits = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["name"] == DERIVED)
        .count();
    assert_eq!(hits, 1, "two sources must not yield two rows: {v:#}");

    let only = row(&v, DERIVED).expect("the authored row");
    assert_eq!(only["title"], "Bookkeeping — hand-tuned");
    assert_eq!(
        only["wip_limit"], 5,
        "the authored row's own settings must survive the merge"
    );
}
