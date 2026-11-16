//! The station authoring path — stations editable at RUN TIME.
//!
//! David, 2026-08-13: "stations need to be editable at run time. They
//! should be data in a registry." The registry and the port already
//! existed; until these routes landed, redrawing a queue meant a SQL
//! seed and a deploy — the exact leak the three-layer reading names,
//! a protocol that cannot be replaced without a deploy having sunk
//! into the substrate.
//!
//! What is asserted here, in the order it matters:
//!
//!  1. A draft can be authored, published, and immediately serves its
//!     queue — no deploy, no restart.
//!  2. **Publish refuses an unviable draft** (422 + the problem list),
//!     and the incumbent keeps serving. This is the whole reason the
//!     lint exists: an always-empty queue and a quiet day render
//!     identically, so nothing downstream can notice the mistake.
//!  3. `_validate` and publish agree — the dry run's problem list is
//!     byte-identical to the refusal's, which is precisely what the
//!     2026-08-13 workflow outage got wrong (`_validate` could name
//!     the problem the whole time; publish never asked it).
//!  4. A draft may be saved unviable. Drafts are work in progress.
//!  5. Authoring is privilege-gated on `workflow`, not `job`: a
//!     caller who may READ every queue still may not redraw one.

use boss_policy_client::types::{AccessTier, User};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::agents::{AgentsRegistry, AutomationActor, InMemoryAgents};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::station_queue::StationPredicate;
use boss_jobs::{
    InMemoryJobs, InMemoryStations, StationKind, StationRegistry, StationSpec, WorkflowRegistry,
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::Value;
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

fn now() -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0).unwrap()
}

/// A viable draft: an ordinary shared batch queue.
fn viable_draft(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        "Authored at run time",
        StationKind::Group,
        StationPredicate {
            kind: Some("car-kind".into()),
            ..Default::default()
        },
        now(),
    )
}

/// An unviable draft: `actor` kind that never binds `@me`, so every
/// executor would see the same queue while the row claims it is
/// personal. Exactly the shape that made the census read a per-actor
/// station as depth 0.
fn unviable_draft(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        "Claims to be personal, is not",
        StationKind::Actor,
        StationPredicate {
            kind: Some("car-kind".into()),
            ..Default::default()
        },
        now(),
    )
}

fn app() -> (axum::Router, Arc<InMemoryStations>) {
    let (app, stations, _, _) = app_with_registries();
    (app, stations)
}

/// The app, with the automations registry the publish door reads an
/// equipment station's automaton against (backlog d0cf2cdc) handed back,
/// so a test can declare the rows it holds. Empty until a test does.
fn app_with_registries() -> (
    axum::Router,
    Arc<InMemoryStations>,
    Arc<InMemoryJobs>,
    Arc<InMemoryAgents>,
) {
    let agents = Arc::new(InMemoryAgents::new());
    let stations = Arc::new(InMemoryStations::new().with_automations(agents.clone()));
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            // The author: may redraw the network.
            .allow("ceo", Action::Create, Resource::workflow(), Scope::All)
            .allow("ceo", Action::Read, Resource::workflow(), Scope::All)
            .allow("ceo", Action::Update, Resource::workflow(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            // The reader: may see every queue, may redraw none.
            .allow("reporter", Action::Read, Resource::job(), Scope::All)
            .allow("reporter", Action::Read, Resource::workflow(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        stations: Some(stations.clone() as Arc<dyn StationRegistry>),
        kind_registry: None::<Arc<dyn WorkflowRegistry>>,
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), stations, jobs, agents)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

fn post(path: &str, role: &str, body: Option<&StationSpec>) -> Request<Body> {
    let b = match body {
        Some(spec) => Body::from(serde_json::to_vec(spec).unwrap()),
        None => Body::empty(),
    };
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("x-boss-user", user_header("emp-1", role))
        .body(b)
        .unwrap()
}

fn get(path: &str, role: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("x-boss-user", user_header("emp-1", role))
        .body(Body::empty())
        .unwrap()
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_retired_department_floor_routes_are_absent() {
    let (app, _) = app();
    let (status, _) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(status, StatusCode::OK, "the station registry still serves");
    for path in ["/api/floor", "/api/floor/finance"] {
        let (status, _) = send(&app, get(path, "ceo")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "retired route: {path}");
    }
}

#[test]
fn a_historical_station_department_label_is_not_written_back() {
    let mut input = serde_json::to_value(viable_draft("old-department")).unwrap();
    input["department"] = serde_json::json!("it");
    let spec: StationSpec = serde_json::from_value(input).unwrap();
    assert!(
        serde_json::to_value(spec)
            .unwrap()
            .get("department")
            .is_none()
    );
}

#[tokio::test]
async fn a_station_authored_at_run_time_serves_its_queue_without_a_deploy() {
    let (app, _) = app();

    let (status, _) = send(
        &app,
        post("/api/stations", "ceo", Some(&viable_draft("new-desk"))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Not live yet — a draft is staged, not serving.
    let (status, body) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0, "a draft must not serve traffic");

    let (status, published) = send(&app, post("/api/stations/new-desk/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(published["status"], "active");
    assert_eq!(published["version"], 1);

    // Live, in the same process, with no restart in between.
    let (status, body) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert_eq!(body["data"][0]["name"], "new-desk");

    let (status, queue) = send(&app, get("/api/stations/new-desk/queue", "ceo")).await;
    assert_eq!(status, StatusCode::OK, "the new station evaluates: {queue}");
}

#[tokio::test]
async fn publish_refuses_an_unviable_draft_and_the_incumbent_keeps_serving() {
    let (app, stations) = app();

    // An incumbent, live.
    stations.seed(viable_draft("desk")).unwrap();
    let (status, _) = send(&app, post("/api/stations/desk/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::OK);

    // A bad v2 staged over it.
    let mut bad = unviable_draft("desk");
    bad.version = 2;
    let (status, _) = send(&app, post("/api/stations", "ceo", Some(&bad))).await;
    assert_eq!(status, StatusCode::CREATED, "a draft may be saved unviable");

    let (status, body) = send(&app, post("/api/stations/desk/publish", "ceo", None)).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unviable draft must not reach the ACTIVE slot: {body}"
    );
    assert_eq!(body["ok"], false);
    let problems = body["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0]["field"], "predicate");
    assert!(
        problems[0]["message"].as_str().unwrap().contains("@me"),
        "the refusal must name the fix: {}",
        problems[0]["message"]
    );

    // The incumbent is untouched — a refused publish is not an outage.
    let (status, body) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert_eq!(body["data"][0]["version"], 1, "v1 still serves");
    assert_eq!(body["data"][0]["status"], "active");
}

#[tokio::test]
async fn the_dry_run_and_the_publish_refusal_report_the_same_problems() {
    // The 2026-08-13 outage in one assertion: an editor that shows
    // "no problems" must publish cleanly, and a refused publish must
    // show what the editor would have shown.
    let (app, stations) = app();

    let (status, dry) = send(
        &app,
        post(
            "/api/stations/_validate",
            "ceo",
            Some(&unviable_draft("desk")),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "lint failures are data, not an error"
    );
    assert_eq!(dry["ok"], false);

    stations.seed(unviable_draft("desk")).unwrap();
    let (status, refused) = send(&app, post("/api/stations/desk/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    assert_eq!(
        dry, refused,
        "the dry run and the refusal must be the same body"
    );
}

/// Codex R1 P2 on car 3eb6ecca (backlog 1bb8c479): the live authoring
/// door. A draft naming one edge twice may be SAVED — drafts are work in
/// progress — but the dry run reports it and publish refuses it 422,
/// naming the repeated target, so the floor never serves it.
#[tokio::test]
async fn publish_refuses_a_draft_naming_one_edge_twice_and_the_dry_run_agrees() {
    let (app, stations) = app();
    // The target is live, so the repeat is the only problem: an edge to
    // no active station is a refusal of its own (backlog 5b80b9f4).
    let mut dock = viable_draft("loading-dock");
    dock.status = boss_jobs::registry::WorkflowStatus::Active;
    stations.seed(dock).unwrap();
    let mut twice = viable_draft("desk");
    twice.edges = vec!["loading-dock".into(), "loading-dock".into()];

    let (status, dry) = send(&app, post("/api/stations/_validate", "ceo", Some(&twice))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dry["ok"], false, "{dry}");

    let (status, _) = send(&app, post("/api/stations", "ceo", Some(&twice))).await;
    assert_eq!(status, StatusCode::CREATED, "a draft may be saved unviable");
    let (status, refused) = send(&app, post("/api/stations/desk/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    let problems = refused["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{refused}");
    assert_eq!(problems[0]["field"], "edges");
    assert!(
        problems[0]["message"]
            .as_str()
            .unwrap()
            .contains("`loading-dock`"),
        "the refusal names the repeated target: {}",
        problems[0]["message"]
    );
    assert_eq!(dry, refused, "the dry run and the refusal are one body");
}

/// Backlog 5b80b9f4: the live door asks the seed's question. An edge
/// must name a station some ACTIVE row declares; the seed refused any
/// other before it published a row, while this door ran only the
/// row-alone lint, so a hand-authored row could route packets to a
/// station that does not exist. Publish now refuses it 422, naming the
/// station and the edge, and writes nothing; the dry run says the same;
/// and once the target is live the same draft publishes as before.
#[tokio::test]
async fn publish_refuses_an_edge_to_no_active_station_naming_both_and_writes_nothing() {
    let (app, stations) = app();
    let mut lost = viable_draft("desk");
    lost.edges = vec!["nowhere".into()];

    let (status, dry) = send(&app, post("/api/stations/_validate", "ceo", Some(&lost))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dry["ok"], false, "the dry run names it too: {dry}");

    let (status, _) = send(&app, post("/api/stations", "ceo", Some(&lost))).await;
    assert_eq!(status, StatusCode::CREATED, "a draft may name any edge");
    let (status, refused) = send(&app, post("/api/stations/desk/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    let problems = refused["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{refused}");
    assert_eq!(problems[0]["field"], "edges");
    let message = problems[0]["message"].as_str().unwrap();
    assert!(
        message.contains("[desk]") && message.contains("`nowhere`"),
        "the refusal names the station and the edge: {message}"
    );
    assert_eq!(dry, refused, "the dry run and the refusal are one body");

    // Nothing written: the draft is still a draft and nothing serves.
    let versions = stations.list_versions("desk").await.unwrap();
    assert_eq!(
        versions.iter().map(|r| r.status).collect::<Vec<_>>(),
        vec![boss_jobs::registry::WorkflowStatus::Draft]
    );
    let (_, listed) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(listed["total"], 0, "{listed}");

    // A valid edge publishes as before.
    let mut target = viable_draft("nowhere");
    target.status = boss_jobs::registry::WorkflowStatus::Active;
    stations.seed(target).unwrap();
    let (status, published) = send(&app, post("/api/stations/desk/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(published["edges"], serde_json::json!(["nowhere"]));
}

/// An equipment station named for `automaton`, with one bay.
fn equipment_draft(name: &str, automaton: &str) -> StationSpec {
    StationSpec {
        automaton: Some(automaton.into()),
        bays: Some(1),
        ..StationSpec::draft(
            name,
            "Operated by an automaton",
            StationKind::Equipment,
            StationPredicate {
                kind: Some("car-kind".into()),
                ..Default::default()
            },
            now(),
        )
    }
}

/// Backlog d0cf2cdc: the automaton an equipment station names decides
/// which actor may operate its bays, so the door asks the question the
/// bundle test asked of the tree alone — is it a REGISTERED automation,
/// a row of the automations registry? The lint checked only that it
/// parsed, so a row published here could name `automation:anything`.
/// Publish now refuses it 422, naming the field and the automaton, and
/// writes nothing; the dry run says the same; and a registered one
/// publishes as before.
#[tokio::test]
async fn publish_refuses_an_equipment_automaton_that_is_not_a_registered_automation() {
    let (app, stations, _, agents) = app_with_registries();
    let actor = boss_core::actor::ActorId::Automation("platform-workflow-seed".into());
    agents
        .declare_automations(
            &[AutomationActor {
                id: "automation:gate-runner".into(),
                role: "platform-admin".into(),
                description: "Runs the gates".into(),
                signs_for: None,
                access_tier: None,
                station: None,
            }],
            &boss_core::publisher::EventStamp::new("jobs", actor),
        )
        .await
        .expect("declare the one registered automation");

    // Spelled as an automation id, so the row-alone lint passes it: only
    // the registry can say nothing answers to it.
    let stray = equipment_draft("kettle", "automation:anything");
    let (status, dry) = send(&app, post("/api/stations/_validate", "ceo", Some(&stray))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dry["ok"], false, "the dry run names it too: {dry}");

    let (status, _) = send(&app, post("/api/stations", "ceo", Some(&stray))).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "a draft may name any automaton"
    );
    let (status, refused) = send(&app, post("/api/stations/kettle/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    let problems = refused["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{refused}");
    assert_eq!(problems[0]["field"], "automaton");
    let message = problems[0]["message"].as_str().unwrap();
    assert!(
        message.contains("[kettle]") && message.contains("`automation:anything`"),
        "the refusal names the station and the automaton: {message}"
    );
    assert_eq!(dry, refused, "the dry run and the refusal are one body");

    // Nothing written: the draft is still a draft and nothing serves.
    let versions = stations.list_versions("kettle").await.unwrap();
    assert_eq!(
        versions.iter().map(|r| r.status).collect::<Vec<_>>(),
        vec![boss_jobs::registry::WorkflowStatus::Draft]
    );
    let (_, listed) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(listed["total"], 0, "{listed}");

    // A registered automaton dry-runs clean and publishes.
    let registered = equipment_draft("kettle", "automation:gate-runner");
    let (status, dry) = send(
        &app,
        post("/api/stations/_validate", "ceo", Some(&registered)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dry["ok"], true, "{dry}");
    let (status, _) = send(&app, post("/api/stations", "ceo", Some(&registered))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, published) = send(&app, post("/api/stations/kettle/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(published["automaton"], "automation:gate-runner");
}

#[tokio::test]
async fn a_viable_spec_dry_runs_clean() {
    let (app, _) = app();
    let (status, dry) = send(
        &app,
        post(
            "/api/stations/_validate",
            "ceo",
            Some(&viable_draft("desk")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dry["ok"], true);
    assert_eq!(dry["problems"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn versions_are_append_only_and_readable() {
    let (app, _) = app();

    send(
        &app,
        post("/api/stations", "ceo", Some(&viable_draft("desk"))),
    )
    .await;
    send(&app, post("/api/stations/desk/publish", "ceo", None)).await;

    let mut v2 = viable_draft("desk");
    v2.version = 2;
    v2.title = "Second cut".into();
    send(&app, post("/api/stations", "ceo", Some(&v2))).await;
    send(&app, post("/api/stations/desk/publish", "ceo", None)).await;

    let (status, versions) = send(&app, get("/api/stations/desk/versions", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    let rows = versions.as_array().unwrap();
    assert_eq!(rows.len(), 2, "history is kept, not overwritten");
    assert_eq!(rows[0]["version"], 1);
    assert_eq!(rows[0]["status"], "retired");
    assert_eq!(rows[1]["version"], 2);
    assert_eq!(rows[1]["status"], "active");

    // The retired row is still readable — what this queue used to be.
    let (status, v1) = send(&app, get("/api/stations/desk/versions/1", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v1["title"], "Authored at run time");
}

#[tokio::test]
async fn retire_closes_the_station_and_is_idempotent() {
    let (app, _) = app();
    send(
        &app,
        post("/api/stations", "ceo", Some(&viable_draft("desk"))),
    )
    .await;
    send(&app, post("/api/stations/desk/publish", "ceo", None)).await;

    let (status, _) = send(&app, post("/api/stations/desk/retire", "ceo", None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(body["total"], 0, "a retired station serves nothing");

    let (status, _) = send(&app, post("/api/stations/desk/retire", "ceo", None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "retire is idempotent");
}

#[tokio::test]
async fn reading_every_queue_does_not_confer_redrawing_one() {
    let (app, stations) = app();
    stations.seed(viable_draft("desk")).unwrap();

    // The reader sees the registry.
    let (status, _) = send(&app, get("/api/stations", "reporter")).await;
    assert_eq!(status, StatusCode::OK);

    // And may not author, publish, or retire.
    for req in [
        post("/api/stations", "reporter", Some(&viable_draft("mine"))),
        post("/api/stations/desk/publish", "reporter", None),
        post("/api/stations/desk/retire", "reporter", None),
    ] {
        let uri = req.uri().to_string();
        let (status, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri} must be gated");
    }
}

/// AN UNPUBLISHED STATION VERSION IS ITS AUTHORS' (backlog 1a4a4d03,
/// second finding). The version reads asked only workflow Read — the
/// basic guest's shipped grant — and served drafts, whose predicates
/// can name an assignee before anyone has decided to route that way. A
/// version that was published is the record of how a queue was drawn,
/// and stays readable; a draft needs an authoring verb on `workflow`.
#[tokio::test]
async fn a_staged_station_is_read_only_by_who_may_author_it() {
    let (app, _) = app();
    send(
        &app,
        post("/api/stations", "ceo", Some(&viable_draft("desk"))),
    )
    .await;
    send(&app, post("/api/stations/desk/publish", "ceo", None)).await;
    let mut v2 = viable_draft("desk");
    v2.version = 2;
    v2.title = "Staged, never published".into();
    let (status, _) = send(&app, post("/api/stations", "ceo", Some(&v2))).await;
    assert_eq!(status, StatusCode::CREATED);

    let statuses = |body: &Value| -> Vec<(i64, String)> {
        body.as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["version"].as_i64().unwrap(),
                    r["status"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let (status, rows) = send(&app, get("/api/stations/desk/versions", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        statuses(&rows),
        vec![(1, "active".to_string()), (2, "draft".to_string())],
        "the author reads every row"
    );
    let (status, rows) = send(&app, get("/api/stations/desk/versions", "reporter")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        statuses(&rows),
        vec![(1, "active".to_string())],
        "a reader reads what was published, and no draft"
    );

    let (status, v1) = send(&app, get("/api/stations/desk/versions/1", "reporter")).await;
    assert_eq!(status, StatusCode::OK, "{v1}");
    let (status, body) = send(&app, get("/api/stations/desk/versions/2", "reporter")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) = send(&app, get("/api/stations/desk/versions/2", "ceo")).await;
    assert_eq!(status, StatusCode::OK, "the author reads its draft");
}
