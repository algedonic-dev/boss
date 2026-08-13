//! Station read surfaces + the claim-CAS capability gate
//! (docs/design/stations.md, Q1–Q4 ratified).
//!
//! - `GET /api/stations` lists the active registry rows; a caller
//!   whose job-read scope is None sees an empty collection (one
//!   policy path with /api/jobs).
//! - `GET /api/stations/{name}/queue` evaluates the predicate over
//!   the caller's policy-scoped open Jobs and orders by the
//!   discipline; the envelope names the discipline and carries the
//!   advisory `over_limit`.
//! - `POST .../claim?station=<name>` enforces the station's
//!   capability (Class-registry role vocabulary) and membership
//!   BEFORE the CAS; a claim without a station keeps today's
//!   behavior.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::station_queue::{StationPredicate, StepMatch};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{
    InMemoryJobs, InMemoryStations, InMemoryWorkflows, StationCapability, StationKind,
    StationRegistry, StationSpec, WorkflowRegistry,
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn user_header(id: &str, role: &str) -> String {
    serde_json::json!({
        "id": id,
        "role": role,
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "it",
    })
    .to_string()
}

fn car_kind() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "car-kind",
        "Car kind",
        "test",
        vec!["custom".into()],
        vec![StepSpec {
            title: "review".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Open for review".into(),
            ..Default::default()
        }],
    )
}

/// The dock, in miniature: open car-kind packets with a branch and no
/// train, whose review step is open.
fn dock_station() -> StationSpec {
    let mut s = StationSpec::draft(
        "test-dock",
        "Test dock",
        StationKind::Batch,
        StationPredicate {
            kind: Some("car-kind".into()),
            metadata_present: vec!["branch".into()],
            metadata_absent: vec!["train".into()],
            step: Some(StepMatch {
                slug: Some("review".into()),
                status_in: vec![
                    boss_core::job::StepStatus::Ready,
                    boss_core::job::StepStatus::Active,
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s.wip_limit = Some(2);
    s
}

fn gated_station() -> StationSpec {
    let mut s = StationSpec::draft(
        "brewer-gate",
        "Brewer-gated station",
        StationKind::Constraint,
        StationPredicate {
            kind: Some("car-kind".into()),
            ..Default::default()
        },
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s.capability = Some(StationCapability {
        roles: vec!["head-brewer".into()],
    });
    s
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(car_kind()).unwrap();
    let stations = Arc::new(InMemoryStations::new());
    stations.seed(dock_station()).unwrap();
    stations.seed(gated_station()).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::step(), Scope::All)
            .allow("head-brewer", Action::Read, Resource::job(), Scope::All)
            .allow("head-brewer", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: Some(stations as Arc<dyn StationRegistry>),
        jobs: jobs.clone(),
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
    (router(state), jobs)
}

async fn post_car(
    app: &axum::Router,
    branch: &str,
    priority: &str,
    opened_on: &str,
    boarded: bool,
) -> String {
    let mut metadata = serde_json::json!({ "branch": branch });
    if boarded {
        metadata["train"] = serde_json::json!("t1");
    }
    let body = serde_json::json!({
        "kind": "car-kind",
        "subject": { "subject_kind": "custom", "id": branch },
        "title": format!("car {branch}"),
        "owner_id": "emp-ceo",
        "status": "open",
        "priority": priority,
        "opened_on": opened_on,
        "metadata": metadata,
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
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["id"].as_str().unwrap().to_string()
}

async fn get_json(
    app: &axum::Router,
    path: &str,
    user: &str,
    role: &str,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::get(path)
                .header("x-boss-user", user_header(user, role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test]
async fn list_stations_returns_active_rows() {
    let (app, _jobs) = app();
    let (status, v) = get_json(&app, "/api/stations", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 2);
    let names: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["brewer-gate", "test-dock"], "name-ordered");
    // The rows carry the queue metadata a lens needs.
    assert_eq!(v["data"][1]["kind"], "batch");
    assert_eq!(
        v["data"][1]["discipline"],
        serde_json::json!(["priority", "age"])
    );
    assert_eq!(v["data"][1]["wip_limit"], 2);
}

#[tokio::test]
async fn a_denied_caller_sees_no_stations() {
    let (app, _jobs) = app();
    // "intern" holds no job-read grant: scope predicate is None.
    let (status, v) = get_json(&app, "/api/stations", "emp-x", "intern").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 0);
    assert_eq!(v["data"], serde_json::json!([]));
}

#[tokio::test]
async fn queue_is_evaluated_ordered_and_advisory_flagged() {
    let (app, _jobs) = app();
    // Three parked cars + one boarded (not a member): the queue
    // orders by priority then age, reports its discipline, and flags
    // the advisory wip_limit breach (limit 2, 3 members).
    let standard_old = post_car(&app, "feat/a", "standard", "2026-08-01", false).await;
    let urgent_new = post_car(&app, "feat/b", "urgent", "2026-08-10", false).await;
    let urgent_old = post_car(&app, "feat/c", "urgent", "2026-08-03", false).await;
    let _boarded = post_car(&app, "feat/d", "emergency", "2026-08-01", true).await;

    let (status, v) = get_json(&app, "/api/stations/test-dock/queue", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["station"], "test-dock");
    assert_eq!(v["discipline"], serde_json::json!(["priority", "age"]));
    assert_eq!(v["total"], 3, "the boarded car is not a member");
    let order: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        vec![
            urgent_old.as_str(),
            urgent_new.as_str(),
            standard_old.as_str()
        ],
        "priority first, then age"
    );
    assert_eq!(v["wip_limit"], 2);
    assert_eq!(v["over_limit"], true, "advisory: reported, nothing dropped");
}

#[tokio::test]
async fn queue_of_unknown_station_is_404_and_denied_caller_sees_empty() {
    let (app, _jobs) = app();
    let (status, _) = get_json(&app, "/api/stations/no-such/queue", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    post_car(&app, "feat/a", "standard", "2026-08-01", false).await;
    let (status, v) = get_json(&app, "/api/stations/test-dock/queue", "emp-x", "intern").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 0, "policy-scoped universe: None sees nothing");
}

async fn claim(
    app: &axum::Router,
    job_id: &str,
    step_id: &str,
    query: &str,
    user: &str,
    role: &str,
) -> StatusCode {
    app.clone()
        .oneshot(
            Request::post(format!("/api/jobs/{job_id}/steps/{step_id}/claim{query}"))
                .header("x-boss-user", user_header(user, role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn first_step_id(app: &axum::Router, job_id: &str) -> String {
    let (_, detail) = get_json(app, &format!("/api/jobs/{job_id}"), "emp-ceo", "ceo").await;
    detail["steps"][0]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn claim_gate_enforces_the_station_capability() {
    let (app, _jobs) = app();
    let job_id = post_car(&app, "feat/a", "standard", "2026-08-01", false).await;
    let step_id = first_step_id(&app, &job_id).await;

    // The ceo is not in the station's capability roles: 403, and the
    // step stays unclaimed (the gate runs before the CAS).
    let denied = claim(
        &app,
        &job_id,
        &step_id,
        "?station=brewer-gate",
        "emp-ceo",
        "ceo",
    )
    .await;
    assert_eq!(denied, StatusCode::FORBIDDEN);

    // A head-brewer is admitted; the CAS then decides as usual.
    let won = claim(
        &app,
        &job_id,
        &step_id,
        "?station=brewer-gate",
        "emp-hb",
        "head-brewer",
    )
    .await;
    assert_eq!(won, StatusCode::OK);
}

#[tokio::test]
async fn claim_from_a_station_the_packet_is_not_at_conflicts() {
    let (app, _jobs) = app();
    // Boarded car: has a train marker, so it is NOT at the dock.
    let job_id = post_car(&app, "feat/x", "standard", "2026-08-01", true).await;
    let step_id = first_step_id(&app, &job_id).await;

    let status = claim(
        &app,
        &job_id,
        &step_id,
        "?station=test-dock",
        "emp-ceo",
        "ceo",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Unknown station names 404 rather than silently skipping the gate.
    let status = claim(
        &app,
        &job_id,
        &step_id,
        "?station=nowhere",
        "emp-ceo",
        "ceo",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A stationless claim keeps today's behavior: CAS only.
    let status = claim(&app, &job_id, &step_id, "", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
}
