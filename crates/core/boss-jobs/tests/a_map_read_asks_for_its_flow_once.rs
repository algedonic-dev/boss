//! HOW OFTEN the IT map pays for its flow read (backlog 29f7e897).
//!
//! Measured live 2026-10-19 04:47-05:02Z: in fifteen minutes the regions
//! and borders reads ran 59 times each at 7-99 s, the step-flow cube
//! logged 87 executions over a second (467 s in all, max 53.7 s) AFTER
//! its per-execution repair (17959b76), and the yard's per-map-read
//! warning fired 176 times — 118 page reads and ~58 mover readings. Each
//! map read ran the cube twice, once per window, and the mover's reading
//! ran it twice more to discard both: its placement reads station
//! MEMBERSHIP, never `served`.
//!
//! Since item ab09f1af's car 2 the map counts flow on the packet-
//! membership basis — one snapshot for both windows, replayed twice
//! (`the_map_reads_the_membership_flow_pg` pins that the narrower replay
//! IS the narrower window). The frequency still matters more there: a
//! snapshot is a repeatable-read transaction over a window's history.
//!
//! What this pins, on the in-memory adapter, which holds no native
//! history and so refuses every membership read:
//!
//! 1. **The map never reads the step-events cube**, and asks the
//!    membership reader for the wider window, then — refused — for this
//!    window alone: two asks per map read, never more — and, since
//!    backlog b852ec37, two asks per READING, which every map read
//!    inside its lifetime (`boss_jobs::reading_cache`) is served, and
//!    which concurrent map reads wait on rather than each taking one.
//! 2. **The mover asks for no flow at all.** It places packets by the
//!    same partition, so its reading is the full read's reading.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::event::Event;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router_shared, run_mover};
use boss_jobs::moves::{InMemoryMoves, MovesFeed};
use boss_jobs::station_queue::{StationPredicate, StepMatch};
use boss_jobs::stations::InMemoryStations;
use boss_jobs::{InMemoryJobs, JobsRepository, StationKind, StationSpec};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

const JOB: &str = "aaaaaaaa-0000-4000-8000-0000000029f7";
/// Served an hour ago: this window.
const NOW_ONLY: &str = "11111111-0000-4000-8000-0000000029f7";
/// Served thirty hours ago: the previous window.
const PREVIOUS_ONLY: &str = "22222222-0000-4000-8000-0000000029f7";
/// Served sixty hours ago: neither window.
const TOO_OLD: &str = "33333333-0000-4000-8000-0000000029f7";
/// Served in BOTH windows — re-promoted and completed again. One
/// obligation in the wider window, so the previous window does not
/// count it: the subtraction the two reads did, kept exactly.
const BOTH: &str = "44444444-0000-4000-8000-0000000029f7";
/// Standing at the station now, so the marshalling region has a member.
const STANDING: &str = "55555555-0000-4000-8000-0000000029f7";

fn user_header(role: &str) -> String {
    serde_json::to_string(&User {
        id: "emp-david".to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

/// The live `q.platform-admin.task` in miniature: every ready task.
fn task_station() -> StationSpec {
    let mut s = StationSpec::draft(
        "q.platform-admin.task",
        "Ready tasks",
        StationKind::Constraint,
        StationPredicate {
            status: Some(JobStatus::Open),
            step: Some(StepMatch {
                kind: Some("task".into()),
                status_in: vec![StepStatus::Ready],
                ..Default::default()
            }),
            ..Default::default()
        },
        Utc::now(),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s
}

fn step(id: &str, slug: &str, status: StepStatus) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        spec_slug: Some(slug.into()),
        status,
        ..Step::new(
            JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
            "task",
            slug,
            0,
        )
    }
}

fn done(step_id: &str, at: DateTime<Utc>) -> Event {
    Event {
        id: Uuid::new_v4(),
        timestamp: at,
        source: "jobs".into(),
        kind: "step.done.task".into(),
        payload: serde_json::json!({ "job_id": JOB, "step_id": step_id, "kind": "task" }),
    }
}

/// One open ship-a-change packet whose task steps were served at the
/// instants the constants name — so the step-events cube COULD count
/// them, and a map that still asked it would draw a number — one task
/// still standing, and the platform protocols in the registry so the
/// inbound line can be drawn.
async fn state() -> (
    Arc<JobsApiState<InMemoryJobs, RecordingEventBus>>,
    Arc<InMemoryJobs>,
) {
    let jobs = Arc::new(InMemoryJobs::new());
    let now = Utc::now();
    let packet = Job {
        id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        status: JobStatus::Open,
        ..Job::new(
            "ship-a-change",
            Subject::new("custom", "bosspipeline"),
            "a car whose tasks drain",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        )
    };
    jobs.create_job_at(&packet, now - Duration::hours(72), &[])
        .await
        .unwrap();
    for (id, slug, status) in [
        (NOW_ONLY, "now-only", StepStatus::Completed),
        (PREVIOUS_ONLY, "previous-only", StepStatus::Completed),
        (TOO_OLD, "too-old", StepStatus::Completed),
        (BOTH, "both", StepStatus::Completed),
        (STANDING, "standing", StepStatus::Ready),
    ] {
        jobs.add_step_at(&step(id, slug, status), now - Duration::hours(72), &[])
            .await
            .unwrap();
    }
    jobs.record_events(&[
        done(NOW_ONLY, now - Duration::hours(1)),
        done(PREVIOUS_ONLY, now - Duration::hours(30)),
        done(TOO_OLD, now - Duration::hours(60)),
        done(BOTH, now - Duration::hours(30)),
        done(BOTH, now - Duration::hours(1)),
    ])
    .await
    .unwrap();

    let kinds = Arc::new(boss_jobs::InMemoryWorkflows::for_fixture());
    for spec in boss_jobs::registry::seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let stations = Arc::new(InMemoryStations::new());
    stations
        .seed(task_station())
        .expect("seed the task station");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("operator", Action::Read, Resource::estate(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        stations: Some(stations),
        kind_registry: Some(kinds as Arc<dyn boss_jobs::WorkflowRegistry>),
        yard_moves: Some(Arc::new(MovesFeed::new(Arc::new(InMemoryMoves::new())))),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (Arc::new(state), jobs)
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("x-boss-user", user_header("operator"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn region<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no region {name} in {v}"))
}

/// The regions and the borders reads each ask the membership reader for
/// the wider window and, refused, for this window alone — and neither
/// reads the step-events cube, whose blind clauses the map no longer
/// draws. The refusal is named on the station, never counted.
#[tokio::test]
async fn a_map_read_asks_the_membership_reader_and_never_the_step_cube() {
    let (state, jobs) = state().await;
    let app = router_shared(state);

    let v = get(&app, "/api/yard/regions").await;
    assert_eq!(
        jobs.membership_reads(),
        2,
        "one regions read: the wider window, then this one"
    );
    let station = region(&v, "marshalling")["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "station:q.platform-admin.task")
        .cloned()
        .unwrap_or_else(|| panic!("no task station machine in {v}"));
    assert!(
        station["why"]
            .as_str()
            .unwrap()
            .contains("native membership history unavailable"),
        "the refusal is named: {station}"
    );

    // The borders read inside the reading's lifetime is SERVED the
    // regions read's reading (backlog b852ec37): the map page polls both
    // on one tick, and each used to pay a snapshot of its own.
    let b = get(&app, "/api/yard/borders").await;
    assert_eq!(
        jobs.membership_reads(),
        2,
        "the borders read shares the regions read's flow reading"
    );
    assert_eq!(jobs.flow_cube_passes(), 0, "the map never reads the cube");
    // And both say WHEN that reading was taken — one instant, so a
    // served reading is never presented as one taken at this read's now.
    assert!(v["flow_as_of"].is_string(), "regions states it: {v}");
    assert_eq!(
        v["flow_as_of"], b["flow_as_of"],
        "the two answers name the one reading"
    );
}

/// The map's two reads, polled together and piling up behind a slow
/// snapshot (backlog b852ec37: 120 cancelled regions reads an hour at
/// 127-187 s each), take ONE flow reading between them: the second
/// waits on the first's rather than starting its own.
#[tokio::test]
async fn concurrent_map_reads_take_one_flow_reading() {
    let (state, jobs) = state().await;
    let app = router_shared(state);
    let reads = (0..4).map(|i| {
        let app = app.clone();
        async move {
            let uri = if i % 2 == 0 {
                "/api/yard/regions"
            } else {
                "/api/yard/borders"
            };
            get(&app, uri).await
        }
    });
    futures::future::join_all(reads).await;
    assert_eq!(
        jobs.membership_reads(),
        2,
        "four concurrent map reads, one reading (the wider window, then this one)"
    );
}

/// The sharing is the reading's LIFETIME, not a counter: past it the
/// next map read takes its own. A zero lifetime is the pre-b852ec37
/// behaviour, two asks per map read.
#[tokio::test]
async fn a_reading_past_its_lifetime_is_read_again() {
    let (state, jobs) = state().await;
    let state = Arc::new(JobsApiState {
        map_flows: Arc::new(boss_jobs::reading_cache::ReadingCache::new(
            StdDuration::ZERO,
        )),
        ..Arc::try_unwrap(state).ok().expect("one owner")
    });
    let app = router_shared(state);
    get(&app, "/api/yard/regions").await;
    get(&app, "/api/yard/borders").await;
    assert_eq!(jobs.membership_reads(), 4, "each read takes its own");
}

/// The mover's reading places packets by station membership and never
/// reads `served`, so it never pays for a flow read — and it still reads
/// the marshalling region, so nothing there waits to be judged later.
#[tokio::test]
async fn the_mover_places_packets_without_reading_the_flow() {
    let (state, jobs) = state().await;
    let feed = state.yard_moves.clone().expect("the record is wired");
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    let mover = tokio::spawn(run_mover(state, cancelled));
    let mut status = feed.status();
    for _ in 0..200 {
        if status.state != "starting" {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(25)).await;
        status = feed.status();
    }
    cancel.send(true).unwrap();
    mover.await.unwrap();
    assert_eq!(status.state, "recording", "{status:?}");
    assert!(
        !status.why.contains("marshalling"),
        "the marshalling region was read: {status:?}"
    );
    assert_eq!(
        (jobs.membership_reads(), jobs.flow_cube_passes()),
        (0, 0),
        "the mover reads no flow: {status:?}"
    );
}
