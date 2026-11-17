//! THE PER-STEP CLAIM DOOR RECORDS THE STATION ITS STEP IS QUEUED AT
//! (backlog 2682c418).
//!
//! Measured 2026-11-06 07:04Z on `GET /api/claims`: 32 of 96 open claims
//! read `station_unavailable: not-recorded`. The claims console takes a
//! claim's station only from a station receipt, and only the station
//! door `POST /api/stations/{name}/claim-next` wrote one. Every robot
//! claim arrives through the per-step door
//! `POST /api/jobs/{id}/steps/{sid}/claim` — the one `boss dispatch`
//! uses — which wrote none.
//!
//! The door records, on the claim's own fact, THE TREE STATION THE CLAIM
//! STANDS AT (D1 of design 305cf4ce, backlog 5662d4b6;
//! `station_tree::claim_station`): the step's `at`, else the deepest tree
//! station whose queue holds the packet on that step's account — never a
//! generated `a.<role>.<model>` or `q.<role>.<kind>` name, which is what
//! 2682c418 first recorded (measured 2026-11-12 18:01Z: 0 of 97 claims at
//! a tree station, 76 at `a.platform-admin.opus-5-5`, 11 at
//! `q.platform-admin.task`). A claim that NAMES a station is gated there
//! and still stands where the tree says. `claim-next` is unchanged: its
//! receipt names its station and it writes no second one (car 2 of the
//! design moves `--next` onto it).
//!
//! These tests go through the real router and the in-memory jobs
//! adapter. The first three fold into the in-memory claims history with
//! each claim's events grouped as one original transaction; the last
//! pair (backlog 971a6190) read the claims exactly as the per-step door
//! leaves them, ungrouped and unbilled, once into the in-memory history
//! and once through the audit log and the native source into the
//! Postgres history, which is the path production reads. The fold rules
//! themselves are held by `claims_console_history.rs` on both adapters.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::event::Event;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_events::original_group::{GroupReference, OriginalGroup};
use boss_jobs::agent_spec::{AgentSpec, Effort};
use boss_jobs::audience::Audience;
use boss_jobs::claims_console::{
    ClaimsHistory, HistoryBatch, InMemoryClaimsHistory, SequencedFact,
};
use boss_jobs::events::{CLAIM_STATION_KEY, STATION_CLAIMED, STEP_UPDATED};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::station_queue::StepMatch;
use boss_jobs::{
    InMemoryJobs, InMemoryStations, InMemoryWorkflows, StationCapability, StationKind,
    StationPredicate, StationRegistry, StationSpec, WorkflowRegistry,
};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

const KIND: &str = "door-kind";
const ROLE: &str = "platform-admin";
const MODEL: &str = "opus-5-5";
const ROBOT: &str = "agent-builder";
/// The `(role, model)` station the `build` step's agent block projects.
const AGENT_STATION: &str = "a.platform-admin.opus-5-5";
/// The `(kind, role)` station the role-only steps project — authored
/// INTO the tree here, under engineering, as the live row is under
/// executive, so it holds every one of this packet's role steps.
const ROLE_STATION: &str = "q.platform-admin.task";

// The tree, in miniature: the company; engineering, which owns
// `door-kind`; `build`, which holds the build step by its predicate; and
// `review`, where the `file` step stands by its `at`.
const COMPANY: &str = "0b5e0000-0000-4000-8000-000000000001";
const ENGINEERING: &str = "0b5e0000-0000-4000-8000-000000000002";
const BUILD: &str = "0b5e0000-0000-4000-8000-000000000003";
const REVIEW: &str = "0b5e0000-0000-4000-8000-000000000004";

fn tree_row(
    name: &str,
    id: &str,
    parent: Option<&str>,
    predicate: StationPredicate,
) -> StationSpec {
    let mut s = StationSpec::draft(
        name,
        name,
        StationKind::Group,
        predicate,
        chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s.id = Some(id.into());
    s.parent = parent.map(str::to_owned);
    s
}

fn tagged(name: &str) -> StationPredicate {
    StationPredicate {
        tags_any: vec![format!("station:{name}")],
        ..Default::default()
    }
}

fn tree() -> Vec<StationSpec> {
    let mut role = tree_row(
        ROLE_STATION,
        "0b5e0000-0000-4000-8000-000000000005",
        Some(ENGINEERING),
        StationPredicate {
            step: Some(StepMatch {
                kind: Some("task".into()),
                metadata_equals: [("authority_role".to_string(), ROLE.to_string())].into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    role.kind = StationKind::Constraint;
    role.capability = Some(StationCapability {
        roles: vec![ROLE.into()],
        ..Default::default()
    });
    vec![
        tree_row("company", COMPANY, None, tagged("company")),
        tree_row(
            "engineering",
            ENGINEERING,
            Some(COMPANY),
            tagged("engineering"),
        ),
        tree_row(
            "build",
            BUILD,
            Some(ENGINEERING),
            StationPredicate {
                kind: Some(KIND.into()),
                step: Some(StepMatch {
                    slug: Some("build".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        tree_row("review", REVIEW, Some(ENGINEERING), tagged("review")),
        role,
    ]
}

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

/// Three steps, one per answer, owned by engineering: `build` (role and
/// model, so the agent and role stations hold it too) stands at the
/// `build` station, its deepest holder; `file` (role only) stands `at`
/// review; `note` declares nothing and stands at its owner.
fn protocol() -> WorkflowSpec {
    let step = |title: &str, audience: Option<Audience>, agent: Option<AgentSpec>| StepSpec {
        title: title.into(),
        kind: "task".into(),
        ready_when: "true".into(),
        title_template: title.into(),
        audience,
        agent,
        metadata_defaults: if title == "file" {
            serde_json::json!({ "at": REVIEW })
        } else {
            serde_json::json!({})
        },
        ..Default::default()
    };
    let mut wf = WorkflowSpec::platform_seed(
        KIND,
        "Door kind",
        "test",
        vec!["custom".into()],
        vec![
            step(
                "build",
                Some(Audience::Role(ROLE.into())),
                Some(AgentSpec {
                    profile: "builder".into(),
                    model: MODEL.into(),
                    budget_usd: 5.0,
                    effort: Effort::Medium,
                    executor_provenance: Default::default(),
                }),
            ),
            step("file", Some(Audience::Role(ROLE.into())), None),
            step("note", None, None),
        ],
    );
    wf.metadata = serde_json::json!({ "owner": ENGINEERING });
    wf
}

struct Fixture {
    app: axum::Router,
    jobs: Arc<InMemoryJobs>,
    history: Arc<dyn ClaimsHistory>,
    /// The audit-index ranges of the events each claim recorded: one
    /// original transaction apiece, the grouping the outbox trigger
    /// gives a claim's events in production.
    claims: Vec<std::ops::Range<usize>>,
}

fn fixture() -> Fixture {
    fixture_with(Arc::new(InMemoryClaimsHistory::default()))
}

/// The same router over a claims history of the caller's choosing.
fn fixture_with(history: Arc<dyn ClaimsHistory>) -> Fixture {
    fixture_with_protocol(history, protocol())
}

fn fixture_with_protocol(
    history: Arc<dyn ClaimsHistory>,
    protocol: boss_jobs::registry::WorkflowSpec,
) -> Fixture {
    fixture_with_protocol_and_pools(history, protocol, None)
}

fn fixture_with_protocol_and_pools(
    history: Arc<dyn ClaimsHistory>,
    protocol: boss_jobs::registry::WorkflowSpec,
    pools: Option<Arc<boss_jobs::pools::InMemoryPools>>,
) -> Fixture {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(protocol).unwrap();
    let jobs = InMemoryJobs::for_selection_fixture();
    let jobs = Arc::new(match &pools {
        Some(pools) => jobs
            .with_workflows(kinds.clone())
            .with_resource_pools(pools.clone()),
        None => jobs,
    });
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(ROLE, Action::Create, Resource::job(), Scope::All)
            .allow(ROLE, Action::Read, Resource::job(), Scope::All)
            .allow(ROLE, Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let stations = InMemoryStations::new();
    for row in tree() {
        stations.seed(row).unwrap();
    }
    let state = JobsApiState {
        agent_budget: Some(Arc::new(boss_jobs::agent_budget::BudgetDoor {
            agents: Arc::new(boss_jobs::agents::InMemoryAgents::new()),
            runs: Arc::new(boss_jobs::agent_runs::InMemoryAgentRuns::new(vec![])),
        })),
        stations: Some(Arc::new(stations) as Arc<dyn StationRegistry>),
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        claims_history: Some(history.clone()),
        pools: pools.map(|p| p as Arc<dyn boss_jobs::pools::PoolRegistry>),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    Fixture {
        app: router(state),
        jobs,
        history,
        claims: Vec::new(),
    }
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("x-boss-user", user_header(ROBOT, ROLE));
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let body = body.map_or_else(Body::empty, |b| Body::from(serde_json::to_vec(&b).unwrap()));
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// Open one packet of [`KIND`]; answer its id and its steps by title.
async fn packet(
    f: &Fixture,
    subject: &str,
) -> (String, std::collections::BTreeMap<String, String>) {
    let (status, created) = send(
        &f.app,
        "POST",
        "/api/jobs",
        Some(serde_json::json!({
            "kind": KIND,
            "subject": { "subject_kind": "custom", "id": subject },
            "title": format!("packet {subject}"),
            "owner_id": ROBOT,
            "status": "open",
            "priority": "standard",
            "opened_on": "2026-11-06",
            "metadata": {},
            "tags": [],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let (_, detail) = send(&f.app, "GET", &format!("/api/jobs/{id}"), None).await;
    let steps = detail["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["title"].as_str().unwrap().to_string(),
                s["id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    (id, steps)
}

/// Claim one step through the per-step door; answer the claim's own
/// `jobs.step.updated` fact.
async fn claim(f: &mut Fixture, job: &str, step: &str, query: &str) -> serde_json::Value {
    let before = f.jobs.recorded_events().len();
    let (status, body) = send(
        &f.app,
        "POST",
        &format!("/api/jobs/{job}/steps/{step}/claim{query}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let recorded = f.jobs.recorded_events();
    f.claims.push(before..recorded.len());
    recorded[before..]
        .iter()
        .find(|e| e.kind == STEP_UPDATED)
        .expect("a claim records jobs.step.updated")
        .payload
        .clone()
}

/// Fold every recorded event into the claims history, each claim's
/// events as one original transaction, and read `GET /api/claims`.
async fn claims_console(f: &Fixture) -> serde_json::Value {
    let recorded = f.jobs.recorded_events();
    let at = recorded.iter().map(|e| e.timestamp).max().unwrap();
    // A selection receipt is conserved as evidence chunks; the native
    // source reassembles each before the fold reads it, and so does this.
    let replay: Vec<_> = recorded
        .iter()
        .enumerate()
        .map(|(i, e)| boss_events::replay::ReplayEvent {
            raw_payload: Some(serde_json::to_string(&e.payload).unwrap()),
            audit_id: i as i64 + 1,
            event_id: e.id,
            kind: e.kind.clone(),
            ts: e.timestamp,
            payload: e.payload.clone(),
        })
        .collect();
    let receipts: std::collections::HashMap<_, _> =
        boss_jobs::selection_evidence::reconstruct(&replay)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == STATION_CLAIMED)
            .map(|e| (e.event_id, e.payload))
            .collect();
    let mut facts: Vec<SequencedFact> = recorded
        .iter()
        .enumerate()
        .map(|(i, e): (usize, &Event)| SequencedFact {
            seq: i as i64 + 1,
            recorded_at: e.timestamp,
            event_id: e.id,
            event_at: e.timestamp,
            source: e.source.clone(),
            kind: e.kind.clone(),
            payload: serde_json::to_string(receipts.get(&e.id).unwrap_or(&e.payload)).unwrap(),
            original_group: None,
        })
        .collect();
    for (n, range) in f.claims.iter().enumerate() {
        let ids: Vec<_> = facts[range.clone()].iter().map(|f| f.event_id).collect();
        let group = OriginalGroup {
            reference: GroupReference {
                id: uuid::Uuid::from_u128(0x2682_0000 + n as u128),
                schema_version: 1,
                event_count: ids.len() as i64,
                member_hash: OriginalGroup::roster_hash(&ids),
            },
            audit_sequences: facts[range.clone()].iter().map(|f| Some(f.seq)).collect(),
            event_ids: ids,
            complete: true,
        };
        for fact in &mut facts[range.clone()] {
            fact.original_group = Some(group.clone());
        }
    }
    f.history
        .materialize(&HistoryBatch {
            through: facts.len() as i64,
            through_at: at,
            facts,
        })
        .await
        .unwrap();
    let (status, body) = send(&f.app, "GET", "/api/claims", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn claim_of(console: &serde_json::Value, step: &str) -> serde_json::Value {
    console["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["step_id"] == step)
        .cloned()
        .unwrap_or_else(|| panic!("no open claim for {step}: {console:#}"))
}

/// The station a recorded claim names must be one `/api/stations`
/// serves and whose queue holds the packet — the floor's own answer.
async fn the_floor_agrees(f: &Fixture, station: &str, job: &str) {
    let (status, listed) = send(&f.app, "GET", "/api/stations", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert!(
        listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == station),
        "{station} is not a station /api/stations serves: {listed:#}"
    );
    let (status, queue) = send(
        &f.app,
        "GET",
        &format!("/api/stations/{station}/queue"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{queue}");
    assert!(
        queue.to_string().contains(job),
        "{station}'s queue does not hold packet {job}: {queue:#}"
    );
}

/// The claim names no generated `q.`/`a.` station: not a projected one
/// (outside the tree) and not one authored into it.
fn tree_named(station: &serde_json::Value) {
    let name = station.as_str().unwrap_or_default();
    assert!(
        !name.is_empty() && !name.starts_with("q.") && !name.starts_with("a."),
        "a claim stands at a tree station, never a generated name: {station}"
    );
}

/// A per-step claim stands at the tree station the door resolves (D1 of
/// design 305cf4ce) and the claims console reads it there: `file` at the
/// station its `at` names; `build`, which declares none, at its deepest
/// holder — `build`, though the generated agent and role stations hold it
/// too and the role station is a tree row as deep; `note`, which matches
/// no predicate, at its protocol's owner. None of them at a generated
/// name, which is what all three recorded before (or nothing, for `note`).
#[tokio::test]
async fn a_per_step_claim_reads_the_tree_station_it_stands_at_on_the_claims_console() {
    let mut f = fixture();
    let (job, steps) = packet(&f, "queued").await;
    // Read the floor BEFORE the claims: a queue holds ready steps. The
    // generated stations hold this packet too — they are simply not
    // where a claim stands.
    for station in [
        AGENT_STATION,
        ROLE_STATION,
        "build",
        "review",
        "engineering",
    ] {
        the_floor_agrees(&f, station, &job).await;
    }

    let build = claim(&mut f, &job, &steps["build"], "").await;
    assert_eq!(build[CLAIM_STATION_KEY], "build", "{build}");
    let file = claim(&mut f, &job, &steps["file"], "").await;
    assert_eq!(file[CLAIM_STATION_KEY], "review", "{file}");
    let note = claim(&mut f, &job, &steps["note"], "").await;
    assert_eq!(note[CLAIM_STATION_KEY], "engineering", "{note}");
    for fact in [&build, &file, &note] {
        tree_named(&fact[CLAIM_STATION_KEY]);
    }
    // A fact about the claim, never written onto the row.
    for (title, id) in &steps {
        let row = boss_jobs::JobsRepository::get_step(
            f.jobs.as_ref(),
            &boss_core::job::StepId::from_uuid(id.parse().unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            row.metadata.get(CLAIM_STATION_KEY).is_none(),
            "{title}: {}",
            row.metadata
        );
    }

    let console = claims_console(&f).await;
    for (title, station) in [
        ("build", "build"),
        ("file", "review"),
        ("note", "engineering"),
    ] {
        let read = claim_of(&console, &steps[title]);
        assert_eq!(read["station"], station, "{title}: {read}");
        assert_eq!(read["station_unavailable"], serde_json::Value::Null);
    }
}

#[tokio::test]
async fn an_unaddressed_legacy_claim_records_its_root_fallback_for_counting() {
    let mut protocol = protocol();
    protocol.metadata = serde_json::json!({});
    let mut f = fixture_with_protocol(Arc::new(InMemoryClaimsHistory::default()), protocol);
    let (job, steps) = packet(&f, "legacy-root").await;
    let fact = claim(&mut f, &job, &steps["note"], "").await;
    assert_eq!(fact[CLAIM_STATION_KEY], "company");
    assert_eq!(fact["_station_id"], COMPANY);
    assert_eq!(fact["station_resolution"], "root-fallback");
    assert_eq!(
        fact["station_resolution_reason"],
        "unaddressed legacy packet"
    );
    let console = claims_console(&f).await;
    let recorded_claim = claim_of(&console, &steps["note"]);
    assert_eq!(recorded_claim["station_id"], COMPANY);
    assert_eq!(recorded_claim["station_resolution"], "root-fallback");
    assert_eq!(console["root_fallback_claims"], 1);
    let (next, next_steps) = packet(&f, "another-legacy-root").await;
    claim(&mut f, &next, &next_steps["note"], "").await;
    let console = claims_console(&f).await;
    assert_eq!(console["root_fallback_claims"], 2);
    let (status, limited) = send(&f.app, "GET", "/api/claims?limit=1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(limited["claims"].as_array().unwrap().len(), 1);
    assert_eq!(
        limited["root_fallback_claims"], 2,
        "the count precedes the display limit"
    );
}

/// B1 of review a480ed50: acquiring a resource rewrites STEP_UPDATED
/// after the door adds its claim evidence. Count the committed fact,
/// including the lease-bearing step, rather than an unbilled stand-in.
#[tokio::test]
async fn a_resource_backed_legacy_claim_keeps_its_root_fallback_for_counting() {
    use boss_jobs::pools::PoolRegistry;

    let mut protocol = protocol();
    protocol.metadata = serde_json::json!({});
    let note = protocol
        .steps
        .iter_mut()
        .find(|s| s.title == "note")
        .unwrap();
    note.resources = Some(serde_json::from_value(serde_json::json!({"vcpu":"1"})).unwrap());
    note.duration_hours = Some(1.0);
    let pools = Arc::new(boss_jobs::pools::InMemoryPools::default());
    let now = chrono::Utc::now();
    pools
        .publish_declared(
            serde_json::from_value(serde_json::json!({
                "name":"vcpu", "version":1, "status":"active", "unit":"vCPU",
                "kind":"flow", "capacity":{"state":"configured", "amount":"4"},
                "created_at":now
            }))
            .unwrap(),
            &boss_core::actor::ActorId::automation("claim-evidence-test"),
            now,
        )
        .await
        .unwrap();
    let mut f = fixture_with_protocol_and_pools(
        Arc::new(InMemoryClaimsHistory::default()),
        protocol,
        Some(pools),
    );
    let (job, steps) = packet(&f, "resource-backed-legacy-root").await;
    let fact = claim(&mut f, &job, &steps["note"], "").await;
    assert!(fact["metadata"]["resource_claim"].is_object(), "{fact}");
    assert!(
        f.jobs
            .recorded_events()
            .iter()
            .any(|event| event.kind == boss_jobs::flow_resources::RESERVED)
    );
    assert_eq!(fact[CLAIM_STATION_KEY], "company");
    assert_eq!(fact["_station_id"], COMPANY);
    assert_eq!(fact["station_resolution"], "root-fallback", "{fact}");
    assert_eq!(
        fact["station_resolution_reason"],
        "unaddressed legacy packet"
    );
    let console = claims_console(&f).await;
    let recorded_claim = claim_of(&console, &steps["note"]);
    assert_eq!(recorded_claim["station_id"], COMPANY);
    assert_eq!(recorded_claim["station_resolution"], "root-fallback");
    assert_eq!(console["root_fallback_claims"], 1);
    assert_eq!(console["station_resolution_unrecorded_claims"], 0);
}

#[tokio::test]
async fn the_root_queue_holds_unaddressed_legacy_packets_before_their_claim() {
    let mut protocol = protocol();
    protocol.metadata = serde_json::json!({});
    for step in &mut protocol.steps {
        if let Some(defaults) = step.metadata_defaults.as_object_mut() {
            defaults.remove("at");
        }
    }
    let f = fixture_with_protocol(Arc::new(InMemoryClaimsHistory::default()), protocol);
    let (job, _) = packet(&f, "legacy-root-queue").await;
    the_floor_agrees(&f, "company", &job).await;
}

/// A claim that NAMES a station is gated there — membership and
/// capability, as before — and still stands where the tree says: the
/// station a claim stands at is a function of the tree (D7), not of the
/// door the claimant came in by, so naming a generated station records
/// the tree station, never the generated name.
#[tokio::test]
async fn a_claim_naming_a_generated_station_stands_at_the_tree_station() {
    let mut f = fixture();
    let (job, steps) = packet(&f, "named").await;
    for (title, named, stands) in [
        ("build", ROLE_STATION, "build"),
        ("file", AGENT_STATION, "review"),
    ] {
        let fact = claim(&mut f, &job, &steps[title], &format!("?station={named}")).await;
        assert_eq!(
            fact[CLAIM_STATION_KEY], stands,
            "{title} named {named}: {fact}"
        );
    }
    let console = claims_console(&f).await;
    assert_eq!(claim_of(&console, &steps["build"])["station"], "build");
    assert_eq!(claim_of(&console, &steps["file"])["station"], "review");
}

/// `claim-next` is unchanged: its selection receipt names its station,
/// it writes no per-step station key beside it, and the console reads
/// the receipt's station as before.
#[tokio::test]
async fn claim_next_keeps_its_own_receipt_and_writes_no_second_station() {
    let mut f = fixture();
    let (job, steps) = packet(&f, "selected").await;
    let before = f.jobs.recorded_events().len();
    let (status, receipt) = send(
        &f.app,
        "POST",
        &format!("/api/stations/{AGENT_STATION}/claim-next"),
        Some(serde_json::json!({"request_id":"2682c418-0000-4000-8000-000000000001"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    assert_eq!(receipt["station"]["name"], AGENT_STATION);
    assert_eq!(receipt["selected"]["job_id"], job);
    assert_eq!(receipt["selected"]["step_id"], steps["build"]);
    let recorded = f.jobs.recorded_events();
    f.claims.push(before..recorded.len());
    let claimed: Vec<_> = recorded[before..]
        .iter()
        .filter(|e| e.kind == STATION_CLAIMED)
        .collect();
    assert_eq!(claimed.len(), 1, "one selection receipt");
    assert!(
        recorded[before..]
            .iter()
            .all(|e| e.payload.get(CLAIM_STATION_KEY).is_none()),
        "claim-next writes no per-step station beside its receipt"
    );
    let console = claims_console(&f).await;
    assert_eq!(
        claim_of(&console, &steps["build"])["station"],
        AGENT_STATION
    );
}

/// Where the claims console of an unbilled-claim test reads its history.
enum Ledger {
    /// The recorded events folded straight into the in-memory history.
    Memory,
    /// The recorded events written to the audit log and read through the
    /// native source into the Postgres history — production's path.
    Native(boss_testing::TestDb),
}

/// Fold every recorded event AS RECORDED — no original transaction, as
/// the per-step door's claims carry none — and read `GET /api/claims`.
async fn claims_console_as_recorded(f: &Fixture, ledger: &Ledger) -> serde_json::Value {
    let recorded = f.jobs.recorded_events();
    match ledger {
        Ledger::Memory => {
            let at = recorded.iter().map(|e| e.timestamp).max().unwrap();
            let facts: Vec<SequencedFact> = recorded
                .iter()
                .enumerate()
                .map(|(i, e)| SequencedFact {
                    seq: i as i64 + 1,
                    recorded_at: e.timestamp,
                    event_id: e.id,
                    event_at: e.timestamp,
                    source: e.source.clone(),
                    kind: e.kind.clone(),
                    payload: serde_json::to_string(&e.payload).unwrap(),
                    original_group: None,
                })
                .collect();
            f.history
                .materialize(&HistoryBatch {
                    through: facts.len() as i64,
                    through_at: at,
                    facts,
                })
                .await
                .unwrap();
        }
        Ledger::Native(db) => {
            use boss_core::audit::AuditWriter;
            boss_events::PgAuditWriter::new(db.pool.clone())
                .write_batch(&recorded)
                .await
                .unwrap();
            while !boss_jobs::claims_console::source::advance_native_once(
                f.history.as_ref(),
                &db.pool,
                1024,
            )
            .await
            .unwrap()
            {}
        }
    }
    let (status, body) = send(&f.app, "GET", "/api/claims", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// AN UNBILLED PER-STEP CLAIM READS ITS STATION (backlog 971a6190).
/// After 2682c418 converged, both robot claims made through this door
/// carried `_station` on their own fact and still read `station: null,
/// station_unavailable: not-recorded` (measured 2026-11-06 13:47Z):
/// the native source's payload allowlist dropped the key before the
/// fold saw it, and the fold matched a receipt to a claim only by an
/// activation group or a resource claim, neither of which an unbilled
/// per-step claim carries. The tests above fold grouped claims straight
/// into memory, so they passed. These read the claims as the door
/// leaves them, on both histories.
async fn an_unbilled_per_step_claim_reads_its_station(ledger: Ledger) {
    let history: Arc<dyn ClaimsHistory> = match &ledger {
        Ledger::Memory => Arc::new(InMemoryClaimsHistory::default()),
        // A fold fixture, not production's 2 s budget (backlog ebbf38c6).
        Ledger::Native(db) => {
            Arc::new(boss_jobs::claims_console::PgClaimsHistory::for_fold_fixture(db.pool.clone()))
        }
    };
    let mut f = fixture_with(history);
    let (job, steps) = packet(&f, "unbilled").await;
    let build = claim(&mut f, &job, &steps["build"], "").await;
    assert_eq!(build[CLAIM_STATION_KEY], "build", "{build}");
    assert!(
        build["metadata"]["resource_claim"].is_null(),
        "the claim is unbilled: {build}"
    );
    let file = claim(&mut f, &job, &steps["file"], "").await;
    assert_eq!(file[CLAIM_STATION_KEY], "review", "{file}");
    let note = claim(&mut f, &job, &steps["note"], "").await;
    assert_eq!(note[CLAIM_STATION_KEY], "engineering", "{note}");

    let console = claims_console_as_recorded(&f, &ledger).await;
    let read = claim_of(&console, &steps["build"]);
    assert_eq!(read["resource_acquisition"], serde_json::Value::Null);
    assert_eq!(read["station"], "build", "{read}");
    assert_eq!(read["station_unavailable"], serde_json::Value::Null);
    let read = claim_of(&console, &steps["file"]);
    assert_eq!(read["station"], "review", "{read}");
    let read = claim_of(&console, &steps["note"]);
    assert_eq!(read["station"], "engineering", "{read}");
    assert_eq!(read["station_unavailable"], serde_json::Value::Null);
}

#[tokio::test]
async fn an_unbilled_per_step_claim_reads_its_station_from_the_in_memory_history() {
    an_unbilled_per_step_claim_reads_its_station(Ledger::Memory).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unbilled_per_step_claim_reads_its_station_through_the_native_source() {
    an_unbilled_per_step_claim_reads_its_station(Ledger::Native(boss_testing::TestDb::new().await))
        .await;
}
