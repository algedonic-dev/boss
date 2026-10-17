//! Station read surfaces + the claim-CAS capability gate
//! (docs/design/stations.md, Q1–Q4 ratified).
//!
//! - `GET /api/stations` lists the active registry rows; a caller
//!   whose job-read scope is None is REFUSED (403) on every station
//!   read, never answered an empty collection (backlog 8dcd28ce).
//! - `GET /api/stations/{name}/queue` evaluates the predicate over
//!   the caller's policy-scoped open Jobs and orders by the
//!   discipline; the envelope names the discipline and carries the
//!   advisory `over_limit`.
//! - `POST .../claim?station=<name>` enforces the station's
//!   capability (Class-registry role vocabulary) and membership
//!   BEFORE the CAS; a claim without a station keeps today's
//!   behavior.

use boss_policy_client::types::{AccessTier, User};
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobStatus};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::station_queue::{DisciplineKey, SELF, StationPredicate, StepMatch};
use boss_jobs::{
    InMemoryJobs, InMemoryStations, InMemoryWorkflows, StationCapability, StationKind,
    StationRegistry, StationSpec, WorkflowRegistry,
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

// SERIALISE THE REAL TYPE, never a copy of its wire shape. A test
// that hand-builds the header is testing a copy: if a field loses its
// serde default or a new required one lands, a hand-built payload keeps
// passing here while production rejects it — the failure surfaces as a
// live 4xx instead of a red test, which is the wrong way round
// (7c3649e2).
fn user_header(id: &str, role: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
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
        chrono::Utc::now(),
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
            step: Some(StepMatch {
                slug: Some("review".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s.capability = Some(StationCapability {
        roles: vec!["head-brewer".into()],
        ..Default::default()
    });
    s
}

/// The filer's watchlist: the platform's one per-actor station row
/// (118-watchlist-station.sql). One row, every actor — `@me` binds to
/// whoever is asking — and a terminal window so a packet's outcome is
/// still there when the filer comes looking for it.
fn watchlist_station() -> StationSpec {
    let mut s = StationSpec::draft(
        "my-watchlist",
        "My watchlist — packets I filed",
        StationKind::Actor,
        StationPredicate {
            metadata_equals: BTreeMap::from([("submitted_by".into(), SELF.to_string())]),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s.discipline = vec![DisciplineKey::Recency];
    s.terminal_window_days = Some(14);
    s
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    app_with_agents(Some(Arc::new(boss_jobs::agents::InMemoryAgents::new())))
}

fn app_with_agents(
    agents: Option<Arc<dyn boss_jobs::agents::AgentsRegistry>>,
) -> (axum::Router, Arc<InMemoryJobs>) {
    app_with_workflow_and_agents(car_kind(), agents)
}

fn app_with_workflow(workflow: WorkflowSpec) -> (axum::Router, Arc<InMemoryJobs>) {
    app_with_workflow_and_agents(
        workflow,
        Some(Arc::new(boss_jobs::agents::InMemoryAgents::new())),
    )
}

fn app_with_workflow_and_agents(
    workflow: WorkflowSpec,
    agents: Option<Arc<dyn boss_jobs::agents::AgentsRegistry>>,
) -> (axum::Router, Arc<InMemoryJobs>) {
    app_with_policy_and_agents(workflow, agents, None)
}

fn app_with_policy_and_agents(
    workflow: WorkflowSpec,
    agents: Option<Arc<dyn boss_jobs::agents::AgentsRegistry>>,
    policy_override: Option<Arc<dyn PolicyClient>>,
) -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(workflow).unwrap();
    let stations = Arc::new(InMemoryStations::new());
    stations.seed(dock_station()).unwrap();
    stations.seed(gated_station()).unwrap();
    stations.seed(watchlist_station()).unwrap();
    let jobs = Arc::new(InMemoryJobs::for_selection_fixture());
    let policy: Arc<dyn PolicyClient> = policy_override.unwrap_or_else(|| {
        Arc::new(
            FakePolicyClient::builder()
                .allow("ceo", Action::Create, Resource::job(), Scope::All)
                .allow("ceo", Action::Read, Resource::job(), Scope::All)
                .allow("ceo", Action::Update, Resource::step(), Scope::All)
                .allow("head-brewer", Action::Read, Resource::job(), Scope::All)
                .allow("head-brewer", Action::Update, Resource::step(), Scope::All)
                .allow("reporter", Action::Read, Resource::job(), Scope::All)
                // A guest that policy would let read everything: the only
                // thing standing between them and somebody's watchlist is
                // the placeholder failing to bind, which is what the guest
                // test below is actually asserting.
                .allow("guest", Action::Read, Resource::job(), Scope::All)
                .build(),
        )
    });
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        agent_budget: agents.map(|agents| {
            Arc::new(boss_jobs::agent_budget::BudgetDoor {
                agents,
                runs: Arc::new(boss_jobs::agent_runs::InMemoryAgentRuns::new(vec![])),
            })
        }),
        stations: Some(stations as Arc<dyn StationRegistry>),
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
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
    assert_eq!(v["total"], 3);
    let names: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["brewer-gate", "my-watchlist", "test-dock"],
        "name-ordered"
    );
    // The rows carry the queue metadata a lens needs.
    assert_eq!(v["data"][2]["kind"], "batch");
    assert_eq!(
        v["data"][2]["discipline"],
        serde_json::json!(["priority", "age"])
    );
    assert_eq!(v["data"][2]["wip_limit"], 2);
    // The per-actor row is a registry row like any other — the
    // placeholder is visible in the listing, unbound.
    assert_eq!(v["data"][1]["kind"], "actor");
    assert_eq!(v["data"][1]["terminal_window_days"], 14);
    assert_eq!(
        v["data"][1]["predicate"]["metadata_equals"]["submitted_by"],
        "@me"
    );
}

/// A refused scope refuses — on every station read surface.
///
/// Until 2026-09-23 each of these answered `{"data": [], "total": 0}`
/// with a 200, which is word for word what an idle network says. A
/// caller who may read nothing was told there was nothing, and a
/// surface rendering it drew an empty board with no hint that the read
/// had been denied (backlog 8dcd28ce; CLAUDE.md, "a wrong target
/// answers instead of erroring").
#[tokio::test]
async fn a_denied_caller_is_refused_by_every_station_read() {
    let (app, _jobs) = app();
    post_car(&app, "feat/a", "standard", "2026-08-01", false).await;
    // "intern" holds no job-read grant: scope predicate is None.
    for path in [
        "/api/stations",
        "/api/stations/load",
        "/api/stations/flow",
        "/api/stations/test-dock/queue",
    ] {
        let (status, v) = get_json(&app, path, "emp-x", "intern").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} answered {v}");
        // Control: the same read by a caller who may see packets is a
        // 200, so the 403 is the scope and not a broken route.
        let (status, _) = get_json(&app, path, "emp-ceo", "ceo").await;
        assert_eq!(status, StatusCode::OK, "{path} as ceo");
    }
}

#[tokio::test]
async fn membership_flow_is_explicit_about_missing_original_history_and_keeps_step_basis_separate()
{
    let (app, _) = app();
    let (status, value) = get_json(&app, "/api/stations/flow", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["basis"], "packet-membership-v1");
    assert!(
        value["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["arrived"].is_null()
                && r["departed"].is_null()
                && r["unknown"].as_array().is_some_and(|u| !u.is_empty()))
    );
    assert!(
        value["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["covered"] == serde_json::json!([]))
    );
    let (status, steps) = get_json(
        &app,
        "/api/stations/flow?basis=step-events",
        "emp-ceo",
        "ceo",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(steps["basis"], "step-events");
    assert!(
        steps["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["basis"] == "step-events" || r["basis"] == "unavailable")
    );
    let (status, _) = get_json(
        &app,
        "/api/stations/flow?basis=an-unmatched-basis",
        "emp-ceo",
        "ceo",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn legacy_distinct_step_cube_cannot_be_a_global_aggregate_for_a_narrow_reader() {
    let policy = Arc::new(
        FakePolicyClient::builder()
            .allow("owner", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let mut workflow = car_kind();
    workflow.steps[0].authority_role = Some("operator".into());
    let (app, _) = app_with_policy_and_agents(workflow, None, Some(policy));
    let (status, value) = get_json(
        &app,
        "/api/stations/flow?basis=step-events",
        "alice",
        "owner",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        value["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["station"] == "q.operator.task")
    );
    assert!(
        value["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["arrived"].is_null() && r["served"].is_null()),
        "{value}"
    );
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
async fn queue_of_unknown_station_is_404() {
    let (app, _jobs) = app();
    let (status, _) = get_json(&app, "/api/stations/no-such/queue", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A packet whose steps cannot be read is not a packet with no steps.
///
/// The dock's predicate reads step state, so the handler reads each
/// candidate's steps. It answered a failed read with
/// `unwrap_or_default()` — an empty list — and the step clause then
/// could not match, so the packet fell out of the queue and the 200
/// said the queue was one shorter than it was (backlog c11e9d3c). The
/// read that could not be made must fail the whole answer, loudly,
/// naming the packet it could not read.
#[tokio::test]
async fn a_failed_steps_read_fails_the_queue_rather_than_dropping_the_packet() {
    let (app, jobs) = app();
    let id = post_car(&app, "feat/a", "standard", "2026-08-01", false).await;

    // Control: readable, the packet is a member.
    let (status, v) = get_json(&app, "/api/stations/test-dock/queue", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 1);

    jobs.fail_steps_read(&boss_core::job::JobId::from_uuid(id.parse().unwrap()));
    let resp = get_text(&app, "/api/stations/test-dock/queue").await;
    assert_eq!(resp.0, StatusCode::INTERNAL_SERVER_ERROR, "{}", resp.1);
    assert!(resp.1.contains(&id), "names the packet: {}", resp.1);
}

/// The same defect on the one-call congestion read: a depth computed
/// over a packet whose steps were silently empty is a smaller number,
/// not an error (backlog c11e9d3c). Written with 59c34790 and left out
/// of that car because it needs 6c06ef65's `resolved_open_packets`,
/// which answers a failed read with a 500; restored once both were on
/// main, since nothing else pinned the load's answer (f6c97006).
#[tokio::test]
async fn a_failed_steps_read_fails_the_load_rather_than_shrinking_a_depth() {
    let (app, jobs) = app();
    let id = post_car(&app, "feat/a", "standard", "2026-08-01", false).await;

    let (status, v) = get_json(&app, "/api/stations/load", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    let dock = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["station"] == "test-dock")
        .cloned()
        .unwrap();
    assert_eq!(dock["depth"], 1, "control: readable, the dock holds it");

    jobs.fail_steps_read(&boss_core::job::JobId::from_uuid(id.parse().unwrap()));
    let resp = get_text(&app, "/api/stations/load").await;
    assert_eq!(resp.0, StatusCode::INTERNAL_SERVER_ERROR, "{}", resp.1);
    assert!(resp.1.contains(&id), "names the packet: {}", resp.1);
}

/// Stations overlap, and the load says by how much.
///
/// A packet stands at every station whose predicate it matches, so the
/// depths do not add up to a packet count. On 2026-09-23 the
/// Marshalling Yard's sidings summed to 517 over 303 distinct packets —
/// all 213 in the agent station also stood in q.platform-admin.task —
/// and nothing on the board said so; a reader adding the depth column
/// overstated the work by 214 (backlog 140a2222). The load now carries
/// the distinct count over the rows it returns, and each row how many
/// of its members also stand at another of them, so the overlap is a
/// figure the server counted, not one the reader has to guess at.
#[tokio::test]
async fn the_load_states_how_many_distinct_packets_its_depths_hold() {
    let (app, _jobs) = app();
    // Two parked cars stand at BOTH the dock and the brewer gate (which
    // holds every car-kind packet); the boarded one only at the gate.
    post_car(&app, "feat/a", "standard", "2026-08-01", false).await;
    post_car(&app, "feat/b", "standard", "2026-08-02", false).await;
    post_car(&app, "feat/c", "standard", "2026-08-03", true).await;

    let (status, v) = get_json(&app, "/api/stations/load", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let row = |name: &str| {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["station"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no {name} row in {v}"))
    };
    assert_eq!(row("test-dock")["depth"], 2);
    assert_eq!(row("brewer-gate")["depth"], 3);
    assert_eq!(row("my-watchlist")["depth"], 0);

    // 2 + 3 + 0 = 5 standings over 3 packets.
    assert_eq!(v["distinct_packets"], 3, "{v}");
    assert_eq!(
        row("test-dock")["also_elsewhere"],
        2,
        "both dock cars are also at the gate"
    );
    assert_eq!(
        row("brewer-gate")["also_elsewhere"],
        2,
        "the boarded car stands only here"
    );
    assert_eq!(row("my-watchlist")["also_elsewhere"], 0);
}

/// Each load row names the Workflow kind its station's predicate holds,
/// apart from the station's own kind — so the Department Map can link a
/// siding to the per-kind drill-down at /it/operate/bottlenecks
/// (backlog c7c5c1de). The row's `kind` is the STATION kind (batch,
/// constraint, actor): the page audit read it as the Workflow kind, and
/// a link built on it would have opened the Bottlenecks page on
/// `?kind=batch`, which is no kind at all. A station whose predicate
/// spans kinds names none — `null`, not a guess.
#[tokio::test]
async fn a_load_row_names_the_workflow_kind_its_station_holds() {
    let (app, _jobs) = app();
    let (status, v) = get_json(&app, "/api/stations/load", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let row = |name: &str| {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["station"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no {name} row in {v}"))
    };
    assert_eq!(row("test-dock")["kind"], "batch", "the station's own kind");
    assert_eq!(row("test-dock")["workflow_kind"], "car-kind", "{v}");
    assert_eq!(row("brewer-gate")["workflow_kind"], "car-kind");
    assert!(
        row("my-watchlist")["workflow_kind"].is_null(),
        "a predicate naming no kind names none: {v}"
    );
}

/// Past one page, the load and the queue still count every packet.
///
/// Both read the open packets through one `list_jobs` call of
/// `MAX_LIMIT` (1000, `http/mod.rs`) rows and bound the total as
/// `_total`, so past 1000 open packets a station silently undercounted
/// — a limit is not a filter (backlog f71d1e81; 465 open on
/// 2026-09-27, so latent). 1001 parked cars stand at the dock: the
/// load's depth, the queue's total and the rows the queue returns must
/// all say 1001.
#[tokio::test]
async fn the_load_and_the_queue_count_every_open_packet_past_one_page() {
    let (app, _jobs) = app();
    const PAST_ONE_PAGE: usize = 1001;
    for i in 0..PAST_ONE_PAGE {
        post_car(&app, &format!("feat/{i}"), "standard", "2026-08-01", false).await;
    }

    let (status, v) = get_json(&app, "/api/stations/load", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    let dock = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["station"] == "test-dock")
        .cloned()
        .unwrap();
    assert_eq!(dock["depth"], PAST_ONE_PAGE, "the load's depth");
    assert_eq!(
        v["distinct_packets"], PAST_ONE_PAGE,
        "the load's distinct count"
    );

    let (status, v) = get_json(&app, "/api/stations/test-dock/queue", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], PAST_ONE_PAGE, "the queue's total");
    assert_eq!(
        v["data"].as_array().map(Vec::len),
        Some(PAST_ONE_PAGE),
        "the queue's rows"
    );
}

/// The raw body as text, read as the ceo — an error body is prose,
/// not JSON, and the test needs the words.
async fn get_text(app: &axum::Router, path: &str) -> (StatusCode, String) {
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

// ---------------------------------------------------------------------------
// The filer's watchlist — the per-actor station read
// ---------------------------------------------------------------------------

/// A filed packet, seeded straight into the repository so the test can
/// place it at an exact point in its life (open, or closed with an
/// outcome N days ago) without driving a Workflow to get there.
fn filed_packet(who: &str, title: &str, opened_days_ago: i64, closed: Option<(i64, &str)>) -> Job {
    let today = chrono::Utc::now().date_naive();
    let mut j = Job::new(
        "user-feedback",
        boss_core::job::Subject::new("custom", "/ux/jobs"),
        title,
        who,
        boss_core::job::Priority::Standard,
        today - chrono::Duration::days(opened_days_ago),
    );
    j.status = JobStatus::Open;
    j.metadata = serde_json::json!({ "submitted_by": who, "message": "…" });
    if let Some((days_ago, outcome)) = closed {
        // What `close_job_on_terminal` leaves behind.
        j.status = JobStatus::Closed;
        j.closed_on = Some(today - chrono::Duration::days(days_ago));
        j.metadata["outcome"] = serde_json::json!(outcome);
    }
    j
}

async fn get_json_anonymous(app: &axum::Router, path: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn a_watchlist_holds_my_open_and_recently_closed_packets() {
    let (app, jobs) = app();
    let mine_open = filed_packet("emp-r", "Column picker forgets my choice", 9, None);
    let mine_closed = filed_packet("emp-r", "Dark mode contrast", 20, Some((1, "completed")));
    let mine_stale = filed_packet("emp-r", "Ancient gripe", 90, Some((30, "declined")));
    let theirs = filed_packet("emp-other", "Not mine", 1, None);
    for j in [&mine_open, &mine_closed, &mine_stale, &theirs] {
        jobs.create_job(j).await.unwrap();
    }

    let (status, v) = get_json(
        &app,
        "/api/stations/my-watchlist/queue",
        "emp-r",
        "reporter",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["station"], "my-watchlist");
    assert_eq!(v["discipline"], serde_json::json!(["recency"]));
    assert_eq!(
        v["terminal_window_days"], 14,
        "the envelope names the window, so a reader can see why a \
         closed packet is in a queue"
    );
    assert_eq!(
        v["total"], 2,
        "mine, open and recently closed — not theirs, not the one that aged out"
    );

    // Newest activity first: the packet that closed yesterday leads the
    // one opened nine days ago, even though both are standard priority
    // and the closed one is the older Job.
    let titles: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec!["Dark mode contrast", "Column picker forgets my choice"]
    );

    // The terminal state travels WITH the packet — it is the whole
    // reason a closed entry is still on the list.
    assert_eq!(v["data"][0]["status"], "closed");
    assert_eq!(v["data"][0]["metadata"]["outcome"], "completed");
}

#[tokio::test]
async fn one_row_serves_every_actor_and_no_actor_sees_another_s() {
    let (app, jobs) = app();
    jobs.create_job(&filed_packet("emp-r", "Mine", 1, None))
        .await
        .unwrap();
    jobs.create_job(&filed_packet("emp-s", "Theirs", 1, None))
        .await
        .unwrap();

    for (who, expected) in [("emp-r", "Mine"), ("emp-s", "Theirs")] {
        let (status, v) = get_json(&app, "/api/stations/my-watchlist/queue", who, "reporter").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["total"], 1, "{who} sees exactly their own filing");
        assert_eq!(v["data"][0]["title"], expected);
    }
}

/// A per-actor station asked by nobody is refused, not answered empty.
///
/// `@me` has nothing to bind to, so there is no queue to evaluate. The
/// handler used to answer the station's envelope with an empty `data`
/// and a 200 — the same bytes as "you have filed nothing", which is a
/// claim about the caller the server cannot make (backlog c11e9d3c).
/// The binding still never falls back to the literal placeholder: the
/// refusal carries no packet at all.
#[tokio::test]
async fn a_guest_is_refused_the_watchlist_rather_than_shown_an_empty_one() {
    let (app, jobs) = app();
    jobs.create_job(&filed_packet("emp-r", "Mine", 1, None))
        .await
        .unwrap();
    // A packet whose metadata literally holds the placeholder. If the
    // self clause ever went unbound, this is what would leak.
    jobs.create_job(&filed_packet("@me", "Literal placeholder", 1, None))
        .await
        .unwrap();

    let (status, v) = get_json_anonymous(&app, "/api/stations/my-watchlist/queue").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "no actor to bind @me to: refused, answered {v}"
    );
    assert!(
        !v.to_string().contains("Literal placeholder"),
        "the refusal carries no packet: {v}"
    );
}

#[tokio::test]
async fn a_station_without_a_window_still_holds_only_in_flight_packets() {
    // The regression guard on the widened universe: the dock is
    // unchanged by any of this — closed packets have departed the
    // network and a routing station has nothing to say about them.
    let (app, jobs) = app();
    let id = post_car(&app, "feat/a", "standard", "2026-08-01", false).await;
    let (status, v) = get_json(&app, "/api/stations/test-dock/queue", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 1);
    assert_eq!(v["terminal_window_days"], serde_json::Value::Null);

    let (all, _) = jobs
        .list_jobs(&boss_jobs::port::JobFilter::default(), 100, 0)
        .await
        .unwrap();
    let mut job = all
        .into_iter()
        .find(|j| j.id.to_string() == id)
        .expect("job exists");
    job.status = JobStatus::Closed;
    job.closed_on = Some(chrono::Utc::now().date_naive());
    jobs.update_job(&job).await.unwrap();

    let (_, v) = get_json(&app, "/api/stations/test-dock/queue", "emp-ceo", "ceo").await;
    assert_eq!(v["total"], 0, "the packet left the dock when it closed");
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

/// The native selection must conserve its actual choice on retry; it cannot
/// be a station GET followed by a specific-step claim chosen by the client.
#[tokio::test]
async fn station_claim_next_selects_priority_and_conserves_the_owned_receipt() {
    let (app, jobs) = app_with_agents(Some(Arc::new(boss_jobs::agents::InMemoryAgents::new())));
    let older = post_car(&app, "older-standard", "standard", "2026-10-01", false).await;
    let urgent = post_car(&app, "newer-urgent", "urgent", "2026-10-04", false).await;
    let request = serde_json::json!({"request_id":"a1d3b6c8-0456-4789-8123-000000000001"});
    let mut receipts = Vec::new();
    let mut response_bytes = Vec::new();
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/stations/test-dock/claim-next")
                    .header("content-type", "application/json")
                    .header("x-boss-user", user_header("emp-ceo", "ceo"))
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "selection and owned claim must leave one copied receipt"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        response_bytes.push(body.to_vec());
        receipts.push(serde_json::from_slice::<serde_json::Value>(&body).unwrap());
    }
    assert_eq!(
        receipts[0], receipts[1],
        "retry must not acquire another packet"
    );
    assert_eq!(
        response_bytes[0], response_bytes[1],
        "the HTTP receipt is copied byte for byte"
    );
    assert_eq!(receipts[0]["selected"]["job_id"], urgent);
    assert_eq!(receipts[0]["station"]["name"], "test-dock");
    let old_steps = jobs
        .list_steps(&boss_core::job::JobId::from_uuid(older.parse().unwrap()))
        .await
        .unwrap();
    let urgent_steps = jobs
        .list_steps(&boss_core::job::JobId::from_uuid(urgent.parse().unwrap()))
        .await
        .unwrap();
    assert_eq!(old_steps[0].status, boss_core::job::StepStatus::Ready);
    assert_eq!(urgent_steps[0].status, boss_core::job::StepStatus::Active);
    assert_eq!(urgent_steps[0].assignee_id.as_deref(), Some("emp-ceo"));
}

#[tokio::test]
async fn station_claim_next_uses_the_same_derived_station_as_the_claim_door() {
    let mut workflow = car_kind();
    workflow.steps[0].authority_role = Some("head-brewer".into());
    let (app, jobs) = app_with_workflow_and_agents(
        workflow,
        Some(Arc::new(boss_jobs::agents::InMemoryAgents::new())),
    );
    let id = post_car(&app, "derived-claim", "standard", "2026-10-04", false).await;
    let response = app
        .oneshot(
            Request::post("/api/stations/q.head-brewer.task/claim-next")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header("emp-brewer", "head-brewer"))
                .body(Body::from(
                    r#"{"request_id":"a1d3b6c8-0456-4789-8123-000000000002"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let steps = jobs
        .list_steps(&boss_core::job::JobId::from_uuid(id.parse().unwrap()))
        .await
        .unwrap();
    assert_eq!(steps[0].assignee_id.as_deref(), Some("emp-brewer"));
}

#[tokio::test]
async fn derived_station_selection_refuses_two_eligible_steps_without_claiming_either() {
    let mut workflow = car_kind();
    workflow.steps[0].authority_role = Some("head-brewer".into());
    let mut second = workflow.steps[0].clone();
    second.title = "second".into();
    workflow.steps.push(second);
    let (app, jobs) = app_with_workflow(workflow);
    let id = post_car(&app, "ambiguous", "standard", "2026-10-04", false).await;
    let response = app
        .oneshot(
            Request::post("/api/stations/q.head-brewer.task/claim-next")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header("emp-brewer", "head-brewer"))
                .body(Body::from(
                    r#"{"request_id":"a1d3b6c8-0456-4789-8123-000000000003"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let steps = jobs
        .list_steps(&boss_core::job::JobId::from_uuid(id.parse().unwrap()))
        .await
        .unwrap();
    assert_eq!(steps.len(), 2);
    assert!(
        steps
            .iter()
            .all(|step| step.status == boss_core::job::StepStatus::Ready
                && step.assignee_id.is_none())
    );
}

#[tokio::test]
async fn station_selection_preserves_policy_and_station_capability_refusals() {
    let (app, jobs) = app();
    let id = post_car(&app, "refuse-owned-claim", "standard", "2026-10-04", false).await;
    for (station, actor, role, request_id) in [
        (
            "test-dock",
            "emp-guest",
            "guest",
            "a1d3b6c8-0456-4789-8123-000000000004",
        ),
        (
            "brewer-gate",
            "emp-ceo",
            "ceo",
            "a1d3b6c8-0456-4789-8123-000000000005",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/api/stations/{station}/claim-next"))
                    .header("content-type", "application/json")
                    .header("x-boss-user", user_header(actor, role))
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"request_id":request_id})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            jobs.station_claim_receipt(uuid::Uuid::parse_str(request_id).unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }
    let steps = jobs
        .list_steps(&boss_core::job::JobId::from_uuid(id.parse().unwrap()))
        .await
        .unwrap();
    assert_eq!(steps[0].status, boss_core::job::StepStatus::Ready);
    assert!(steps[0].assignee_id.is_none());
}

#[tokio::test]
async fn an_empty_station_or_an_active_only_station_returns_no_owned_receipt() {
    let (app, jobs) = app_with_agents(Some(Arc::new(boss_jobs::agents::InMemoryAgents::new())));
    for (request_id, expected) in [
        (
            "a1d3b6c8-0456-4789-8123-000000000006",
            StatusCode::NO_CONTENT,
        ),
        ("a1d3b6c8-0456-4789-8123-000000000007", StatusCode::CREATED),
        (
            "a1d3b6c8-0456-4789-8123-000000000008",
            StatusCode::NO_CONTENT,
        ),
    ] {
        if expected == StatusCode::CREATED {
            post_car(&app, "one-ready-packet", "standard", "2026-10-04", false).await;
        }
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/stations/test-dock/claim-next")
                    .header("content-type", "application/json")
                    .header("x-boss-user", user_header("emp-ceo", "ceo"))
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"request_id":request_id})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(
            jobs.station_claim_receipt(uuid::Uuid::parse_str(request_id).unwrap())
                .await
                .unwrap()
                .is_some(),
            expected == StatusCode::CREATED
        );
    }
}

/// The actual registry writer lands a revocation after the read copied its
/// prior row. This wrapper controls only the interleaving; both states and
/// the update event are produced by the real in-memory registry adapter.
struct RevokeAfterAgentRead {
    inner: boss_jobs::agents::InMemoryAgents,
    replacement: boss_jobs::agents::AgentInput,
    revoked: std::sync::atomic::AtomicBool,
    fail_after_consume: bool,
}

#[async_trait::async_trait]
impl boss_jobs::agents::AgentsRegistry for RevokeAfterAgentRead {
    fn lock_selection_rows(
        &self,
    ) -> Result<
        Box<dyn boss_jobs::agents::port::AgentSelectionRows + '_>,
        boss_jobs::agents::AgentsError,
    > {
        if self.fail_after_consume {
            Err(boss_jobs::agents::AgentsError::Storage(
                "owner could not complete its fence".into(),
            ))
        } else {
            self.inner.lock_selection_rows()
        }
    }
    async fn resolve_login(
        &self,
        login: &str,
    ) -> Result<Option<String>, boss_jobs::agents::AgentsError> {
        self.inner.resolve_login(login).await
    }
    async fn list(
        &self,
    ) -> Result<Vec<boss_jobs::agents::AgentRow>, boss_jobs::agents::AgentsError> {
        let copied = self.inner.list().await?;
        if !self.revoked.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let stamp = boss_core::publisher::EventStamp::new(
                "test",
                boss_core::actor::ActorId::automation("registry-writer"),
            );
            self.inner
                .publish(
                    std::slice::from_ref(&self.replacement),
                    boss_core::publish::PublishMode::Take,
                    &stamp,
                )
                .await?;
        }
        Ok(copied)
    }
    async fn publish(
        &self,
        rows: &[boss_jobs::agents::AgentInput],
        mode: boss_core::publish::PublishMode,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<boss_jobs::agents::AgentsBatchOutcome, boss_jobs::agents::AgentsError> {
        self.inner.publish(rows, mode, stamp).await
    }
    async fn list_automations(
        &self,
    ) -> Result<Vec<boss_jobs::agents::AutomationActor>, boss_jobs::agents::AgentsError> {
        self.inner.list_automations().await
    }
    async fn declare_automations(
        &self,
        rows: &[boss_jobs::agents::AutomationActor],
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<boss_jobs::agents::AutomationsSeedOutcome, boss_jobs::agents::AgentsError> {
        self.inner.declare_automations(rows, stamp).await
    }
}

#[tokio::test]
async fn an_actor_role_revoked_after_preflight_cannot_leave_a_selection_claim_or_receipt() {
    use boss_jobs::agents::AgentsRegistry;
    let registry = boss_jobs::agents::InMemoryAgents::new().with_rate_card(["gpt-6.1-sol"]);
    let original = boss_jobs::agents::AgentInput {
        id: "agent-selector".into(),
        display_name: "Selector".into(),
        default_model: "gpt-6.1-sol".into(),
        role: Some("head-brewer".into()),
        department: None,
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
        aliases: vec![],
    };
    let stamp = boss_core::publisher::EventStamp::new(
        "test",
        boss_core::actor::ActorId::automation("registry-writer"),
    );
    registry
        .publish(
            std::slice::from_ref(&original),
            boss_core::publish::PublishMode::Take,
            &stamp,
        )
        .await
        .unwrap();
    let mut replacement = original;
    replacement.role = None;
    let registry = Arc::new(RevokeAfterAgentRead {
        inner: registry,
        replacement,
        revoked: std::sync::atomic::AtomicBool::new(false),
        fail_after_consume: false,
    });
    let (app, jobs) = app_with_agents(Some(registry.clone()));
    let job_id = post_car(&app, "revoked-selector", "standard", "2026-10-04", false).await;
    let step_id = first_step_id(&app, &job_id).await;
    let step_id: boss_core::job::StepId =
        serde_json::from_value(serde_json::json!(step_id)).unwrap();
    let before = jobs.get_step(&step_id).await.unwrap().unwrap();
    let request_id = uuid::Uuid::new_v4();
    let response = app
        .oneshot(
            Request::post("/api/stations/brewer-gate/claim-next")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header("agent-selector", "ceo"))
                .body(Body::from(
                    serde_json::json!({"request_id": request_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        registry.inner.list().await.unwrap()[0].role,
        None,
        "the actual writer must have removed the role before the claim verdict"
    );
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "revoked capability must be rechecked under the commit fence: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(jobs.get_step(&step_id).await.unwrap().unwrap(), before);
    assert!(
        jobs.station_claim_receipt(request_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn unchanged_actor_owner_conserves_its_observation_and_original_replay() {
    use boss_jobs::agents::AgentsRegistry;
    let registry =
        Arc::new(boss_jobs::agents::InMemoryAgents::new().with_rate_card(["gpt-6.1-sol"]));
    let declared = boss_jobs::agents::AgentInput {
        id: "agent-selector".into(),
        display_name: "Selector".into(),
        default_model: "gpt-6.1-sol".into(),
        role: Some("head-brewer".into()),
        department: None,
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
        aliases: vec!["selector-login".into()],
    };
    let stamp = boss_core::publisher::EventStamp::new(
        "test",
        boss_core::actor::ActorId::automation("registry-writer"),
    );
    registry
        .publish(&[declared], boss_core::publish::PublishMode::Take, &stamp)
        .await
        .unwrap();
    let actual_row = registry.list().await.unwrap().remove(0);
    let (app, jobs) = app_with_agents(Some(registry));
    post_car(&app, "unchanged-selector", "standard", "2026-10-04", false).await;
    let request_id = uuid::Uuid::new_v4();
    let request = || {
        Request::post("/api/stations/brewer-gate/claim-next")
            .header("content-type", "application/json")
            .header("x-boss-user", user_header("agent-selector", "ceo"))
            .body(Body::from(
                serde_json::json!({"request_id": request_id}).to_string(),
            ))
            .unwrap()
    };
    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let first = first.into_body().collect().await.unwrap().to_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(
        parsed["validated"]["actor_observation"]["row"],
        serde_json::json!(actual_row)
    );
    assert_eq!(
        parsed["validated"]["actor_observation"]["requested_user"]["id"],
        "agent-selector"
    );
    let prepared = &parsed["owner_consumption"]["policy"]["prepared_inputs"];
    assert_eq!(prepared["user"]["id"], "agent-selector");
    assert_eq!(
        prepared["rules"],
        parsed["owner_consumption"]["policy"]["rules"]
    );
    assert_eq!(
        prepared["overrides"],
        parsed["owner_consumption"]["policy"]["overrides"]
    );
    assert_eq!(prepared["request_id"], parsed["request_id"]);
    assert!(prepared["observed_at"].as_str().is_some());
    assert_eq!(
        prepared["source_binding"],
        "same native PolicyClient owner instance; actual writer guard"
    );
    assert_eq!(
        parsed["owner_consumption"]["actor"]["row"],
        serde_json::json!(actual_row)
    );
    assert_eq!(
        parsed["owner_consumption"]["policy"]["user"]["id"],
        "agent-selector"
    );
    assert_eq!(
        parsed["owner_consumption"]["policy"]["prepared_update"],
        serde_json::json!(boss_policy_client::Decision::Allow { scope: Scope::All })
    );
    assert_eq!(
        parsed["owner_consumption"]["policy"]["prepared_read_scope"],
        serde_json::json!(boss_jobs::port::JobScope::All)
    );
    assert!(
        parsed["owner_consumption"]["policy"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| rule["resource"] == "step" && rule["action"] == "update")
    );
    assert_eq!(
        parsed["owner_consumption"]["policy"]["overrides"],
        serde_json::json!([])
    );
    assert!(
        parsed["owner_consumption"]["policy"]["linearized_at"]
            .as_str()
            .is_some()
    );
    assert_eq!(
        parsed["owner_consumption"]["definitions"]["workflows"],
        serde_json::json!(
            parsed["validated"]["workflows"]
                .as_object()
                .unwrap()
                .values()
                .cloned()
                .collect::<Vec<_>>()
        )
    );
    assert_eq!(
        parsed["owner_consumption"]["run_population"]["applicable"],
        false
    );
    let copied = boss_jobs::station_claim::SelectionReceipt::from_json(
        String::from_utf8(first.to_vec()).unwrap(),
    )
    .unwrap();
    assert_eq!(copied.json().as_bytes(), first.as_ref());
    let mut changed = parsed.clone();
    changed["owner_consumption"]["actor"]["row"]["default_model"] =
        serde_json::json!("changed-after-consumption");
    assert!(
        boss_jobs::station_claim::SelectionReceipt::from_json(changed.to_string()).is_err(),
        "copied owner inputs cannot be changed under the conserved digest"
    );
    let mut missing = parsed.clone();
    missing.as_object_mut().unwrap().remove("owner_consumption");
    assert!(
        boss_jobs::station_claim::SelectionReceipt::from_json(missing.to_string()).is_err(),
        "missing owner artifact cannot keep its digest"
    );
    let before = serde_json::to_value(jobs.recorded_events()).unwrap();
    let replay = app.oneshot(request()).await.unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(
        replay.into_body().collect().await.unwrap().to_bytes(),
        first
    );
    assert_eq!(
        serde_json::to_value(jobs.recorded_events()).unwrap(),
        before
    );
}

#[tokio::test]
async fn fresh_selection_without_an_actor_owner_refuses_without_writing() {
    let (app, jobs) = app_with_agents(None);
    let job_id = post_car(&app, "unowned-selector", "standard", "2026-10-04", false).await;
    let step_id = first_step_id(&app, &job_id).await;
    let step_id: boss_core::job::StepId =
        serde_json::from_value(serde_json::json!(step_id)).unwrap();
    let before = jobs.get_step(&step_id).await.unwrap().unwrap();
    let before_events = serde_json::to_value(jobs.recorded_events()).unwrap();
    let request_id = uuid::Uuid::new_v4();
    let response = app
        .oneshot(
            Request::post("/api/stations/test-dock/claim-next")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header("emp-ceo", "ceo"))
                .body(Body::from(
                    serde_json::json!({"request_id": request_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "no native actor owner can fence this fresh selection: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(jobs.get_step(&step_id).await.unwrap().unwrap(), before);
    assert_eq!(
        serde_json::to_value(jobs.recorded_events()).unwrap(),
        before_events
    );
    assert!(
        jobs.station_claim_receipt(request_id)
            .await
            .unwrap()
            .is_none()
    );
}

/// Both the copied allow and its revocation come from the real policy
/// repository/evaluator; this wrapper controls only their interleaving.
struct RevokeAfterPolicyRead {
    repo: Arc<boss_policy_client::InMemoryPolicy>,
    engine: boss_policy_client::PolicyEngine<boss_policy_client::InMemoryPolicy>,
    revoke_rule: String,
    revoked: std::sync::atomic::AtomicBool,
}

#[tokio::test]
async fn an_agent_owner_failure_cannot_escape_as_a_durable_selection_effect() {
    let registry = Arc::new(RevokeAfterAgentRead {
        inner: boss_jobs::agents::InMemoryAgents::new(),
        replacement: boss_jobs::agents::AgentInput {
            id: "agent-unused".into(),
            display_name: "Unused".into(),
            default_model: "gpt-6.1-sol".into(),
            role: None,
            department: None,
            hourly_budget_usd_micros: None,
            max_concurrent_runs: None,
            aliases: vec![],
        },
        revoked: std::sync::atomic::AtomicBool::new(true),
        fail_after_consume: true,
    });
    let (app, jobs) = app_with_agents(Some(registry));
    let job_id = post_car(
        &app,
        "failed-owner-selector",
        "standard",
        "2026-10-04",
        false,
    )
    .await;
    let step_id = first_step_id(&app, &job_id).await;
    let step_id: boss_core::job::StepId =
        serde_json::from_value(serde_json::json!(step_id)).unwrap();
    let before = jobs.get_step(&step_id).await.unwrap().unwrap();
    let before_events = serde_json::to_value(jobs.recorded_events()).unwrap();
    let request_id = uuid::Uuid::new_v4();
    let response = app
        .oneshot(
            Request::post("/api/stations/test-dock/claim-next")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header("emp-ceo", "ceo"))
                .body(Body::from(
                    serde_json::json!({"request_id": request_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        jobs.get_step(&step_id).await.unwrap().unwrap(),
        before,
        "an owner error must precede every durable effect"
    );
    assert_eq!(
        serde_json::to_value(jobs.recorded_events()).unwrap(),
        before_events
    );
    assert!(
        jobs.station_claim_receipt(request_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[async_trait::async_trait]
impl PolicyClient for RevokeAfterPolicyRead {
    fn lock_selection_rows(
        &self,
    ) -> Result<
        Box<dyn boss_policy_client::port::PolicySelectionRows + '_>,
        boss_policy_client::port::PolicyError,
    > {
        use boss_policy_client::PolicyRepository;
        self.repo.lock_selection_rows()
    }

    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
        use boss_policy_client::PolicyRepository;
        let copied = self
            .engine
            .check(user, action, resource.clone())
            .await
            .map_err(|e| boss_policy_client::PolicyClientError::Transport(e.to_string()))?;
        if action == Action::Update
            && resource == Resource::step()
            && !self.revoked.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.repo
                .deactivate_rule(&self.revoke_rule, "native-policy-writer")
                .await
                .map_err(|e| boss_policy_client::PolicyClientError::Transport(e.to_string()))?;
        }
        Ok(copied)
    }
    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
        self.engine
            .scope_predicate(user, resource)
            .await
            .map_err(|e| boss_policy_client::PolicyClientError::Transport(e.to_string()))
    }
}

#[tokio::test]
async fn an_update_grant_revoked_after_preflight_cannot_leave_a_selection_claim_or_receipt() {
    use boss_policy_client::PolicyRepository;
    let update =
        boss_policy_client::PolicyRule::new("ceo", Resource::step(), Action::Update, Scope::All);
    let repo = Arc::new(boss_policy_client::InMemoryPolicy::with_rules([
        boss_policy_client::PolicyRule::new("ceo", Resource::job(), Action::Create, Scope::All),
        boss_policy_client::PolicyRule::new("ceo", Resource::job(), Action::Read, Scope::All),
        update.clone(),
    ]));
    let policy = Arc::new(RevokeAfterPolicyRead {
        engine: boss_policy_client::PolicyEngine::new(repo.clone()),
        repo: repo.clone(),
        revoke_rule: update.id.clone(),
        revoked: std::sync::atomic::AtomicBool::new(false),
    });
    let agents = Arc::new(boss_jobs::agents::InMemoryAgents::new());
    let (app, jobs) = app_with_policy_and_agents(car_kind(), Some(agents), Some(policy));
    let job_id = post_car(
        &app,
        "revoked-policy-selector",
        "standard",
        "2026-10-04",
        false,
    )
    .await;
    let step_id = first_step_id(&app, &job_id).await;
    let step_id: boss_core::job::StepId =
        serde_json::from_value(serde_json::json!(step_id)).unwrap();
    let before = jobs.get_step(&step_id).await.unwrap().unwrap();
    let before_events = serde_json::to_value(jobs.recorded_events()).unwrap();
    let request_id = uuid::Uuid::new_v4();
    let response = app
        .oneshot(
            Request::post("/api/stations/test-dock/claim-next")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header("emp-ceo", "ceo"))
                .body(Body::from(
                    serde_json::json!({"request_id": request_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!repo.rule_for(&update.id).await.unwrap().unwrap().active);
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the native writer revoked the actual grant before commit: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(jobs.get_step(&step_id).await.unwrap().unwrap(), before);
    assert_eq!(
        serde_json::to_value(jobs.recorded_events()).unwrap(),
        before_events
    );
    assert!(
        jobs.station_claim_receipt(request_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_read_grant_revoked_after_selection_read_cannot_leave_a_selection_claim_or_receipt() {
    use boss_policy_client::PolicyRepository;
    let update =
        boss_policy_client::PolicyRule::new("ceo", Resource::step(), Action::Update, Scope::All);
    let read =
        boss_policy_client::PolicyRule::new("ceo", Resource::job(), Action::Read, Scope::All);
    let repo = Arc::new(boss_policy_client::InMemoryPolicy::with_rules([
        boss_policy_client::PolicyRule::new("ceo", Resource::job(), Action::Create, Scope::All),
        read.clone(),
        update.clone(),
    ]));
    let policy = Arc::new(RevokeAfterPolicyRead {
        engine: boss_policy_client::PolicyEngine::new(repo.clone()),
        repo: repo.clone(),
        revoke_rule: read.id.clone(),
        revoked: std::sync::atomic::AtomicBool::new(false),
    });
    let agents = Arc::new(boss_jobs::agents::InMemoryAgents::new());
    let (app, jobs) = app_with_policy_and_agents(car_kind(), Some(agents), Some(policy));
    let job_id = post_car(
        &app,
        "revoked-policy-selector",
        "standard",
        "2026-10-04",
        false,
    )
    .await;
    let step_id = first_step_id(&app, &job_id).await;
    let step_id: boss_core::job::StepId =
        serde_json::from_value(serde_json::json!(step_id)).unwrap();
    let before = jobs.get_step(&step_id).await.unwrap().unwrap();
    let before_events = serde_json::to_value(jobs.recorded_events()).unwrap();
    let request_id = uuid::Uuid::new_v4();
    let response = app
        .oneshot(
            Request::post("/api/stations/test-dock/claim-next")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header("emp-ceo", "ceo"))
                .body(Body::from(
                    serde_json::json!({"request_id": request_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!repo.rule_for(&read.id).await.unwrap().unwrap().active);
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the native writer revoked the actual grant before commit: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(jobs.get_step(&step_id).await.unwrap().unwrap(), before);
    assert_eq!(
        serde_json::to_value(jobs.recorded_events()).unwrap(),
        before_events
    );
    assert!(
        jobs.station_claim_receipt(request_id)
            .await
            .unwrap()
            .is_none()
    );
}
