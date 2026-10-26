#![cfg(feature = "postgres")]
//! THE MARSHALLING REGION READS THE CONSERVED MEMBERSHIP BASIS (item
//! ab09f1af, finish line check 2). Until this car the IT map counted each
//! station's flow from the legacy step-events cube, which cannot see a
//! predicate that binds the caller, names job tags or job metadata, or
//! names no step (`station_flow.rs`, by design): on 2026-10-19 every
//! published station of those shapes drew `?` on the map, TROUBLED or
//! UNKNOWN, while `GET /api/stations/flow` already folded them from the
//! original packet history. Car 1 bounded that fold to a witnessed cut;
//! this car points the map at it.
//!
//! What this pins, against a real schema with originals written through
//! the ordinary producers (the outbox, its group triggers, the relay):
//!
//! 1. **A station whose predicate binds the caller reads a counted flow
//!    on the map**, the same crossings `GET /api/stations/flow` answers —
//!    never the cube's blind clause.
//! 2. **This window and the previous come from ONE snapshot**: the
//!    narrower window replayed from the wider window's snapshot answers
//!    exactly what a snapshot opened at the narrower window answers.

use std::sync::Arc;

use boss_core::event::Event;
use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_events::outbox::{drain_outbox_once, record_event_in_tx};
use boss_jobs::membership_replay::{MembershipReading, replay, replay_windows};
use boss_jobs::port::JobScope;
use boss_jobs::{JobsRepository, PgJobs, StationSpec};
use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn projection_source_advances_only_new_ids_and_catches_up_newly_authorized_packets() {
    use boss_jobs::{
        membership_projection::ProjectionCursor,
        membership_replay::projection::{MembershipProjection, ProjectionBinding},
    };
    let spec = station("q.platform-admin.task");
    let world = World::new(&[spec]).await;
    let since = world.open_window().await;
    let mut job = packet("existing packet");
    job.owner_id = "operator".into();
    world.admit(&job, &[]).await;
    world.relay().await;
    let scope = JobScope::OwnerIs("operator".into());
    let first = world
        .jobs
        .membership_projection_source(since, Some("operator".into()), &scope, None, 10_000)
        .await
        .unwrap();
    let mut projection = MembershipProjection::rebuild(&first.input).unwrap();
    let old_cut = projection.audit_cutoff();
    let cursor = ProjectionCursor {
        until: projection.until(),
        audit_cutoff: old_cut,
        currently_authorized: projection.binding().currently_authorized.clone(),
    };
    let mut added = packet("newly authorized packet");
    added.owner_id = "other".into();
    world.admit(&added, &[]).await;
    world.relay().await;
    added.owner_id = "operator".into();
    world.update(&added).await;
    world.relay().await;
    let next = world
        .jobs
        .membership_projection_source(since, Some("operator".into()), &scope, Some(cursor), 10_000)
        .await
        .unwrap();
    assert!(
        next.input
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .all(|row| row.seq > old_cut),
        "steady delta must not reread retained packet payloads"
    );
    assert!(next.replacement_ids.contains(&added.id.to_string()));
    let donor = next.replacement_history.as_ref().unwrap();
    assert!(
        !donor.groups.iter().flat_map(|g| &g.rows).any(|r| r
            .event
            .payload
            .get("id")
            .and_then(Value::as_str)
            == Some(job.id.to_string().as_str())),
        "targeted history must not reread the unchanged packet"
    );
    projection
        .reconcile(&next.input, Some(donor), &next.replacement_ids)
        .unwrap();
    let mut oracle = world
        .jobs
        .membership_snapshot(since, Some("operator".into()), &scope, 10_000)
        .await
        .unwrap();
    oracle.until = next.input.until;
    oracle.audit_cutoff = next.input.audit_cutoff;
    assert_eq!(
        serde_json::to_value(
            projection
                .read(since, &ProjectionBinding::from_input(&oracle))
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(replay(&oracle)).unwrap()
    );
    assert_eq!(
        world.jobs.membership_census(&scope, 10_000).await.unwrap(),
        oracle.currently_authorized
    );
}

#[tokio::test]
async fn a_producer_committed_after_the_checkpoint_uses_its_new_audit_insertion_cut() {
    use boss_jobs::{
        membership_projection::ProjectionCursor,
        membership_replay::projection::{MembershipProjection, ProjectionBinding},
    };
    let world = World::new(&[station("q.platform-admin.task")]).await;
    let since = world.open_window().await;
    let mut job = packet("late original transaction");
    world.admit(&job, &[]).await;
    world.relay().await;
    let mut held = world.db.pool.begin().await.unwrap();
    job.title = "committed after the checkpoint".into();
    record_event_in_tx(
        &mut held,
        &Event::new(
            "jobs",
            boss_jobs::events::JOB_UPDATED,
            json!(job),
            Utc::now(),
        ),
    )
    .await
    .unwrap();
    let first = world
        .jobs
        .membership_projection_source(since, Some("operator".into()), &JobScope::All, None, 10_000)
        .await
        .unwrap();
    let mut projection = MembershipProjection::rebuild(&first.input).unwrap();
    let cursor = ProjectionCursor {
        until: projection.until(),
        audit_cutoff: projection.audit_cutoff(),
        currently_authorized: None,
    };
    held.commit().await.unwrap();
    world.relay().await;
    let next = world
        .jobs
        .membership_projection_source(
            since,
            Some("operator".into()),
            &JobScope::All,
            Some(cursor.clone()),
            10_000,
        )
        .await
        .unwrap();
    assert!(
        next.input
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .all(|row| row.created_at.is_some_and(|at| at >= cursor.until)),
        "the original producer timestamp is not the audit insertion timestamp"
    );
    assert!(next.replacement_ids.is_empty());
    assert!(next.replacement_history.is_none());
    projection
        .reconcile(
            &next.input,
            next.replacement_history.as_ref(),
            &next.replacement_ids,
        )
        .unwrap();
    let mut oracle = world
        .jobs
        .membership_snapshot(since, Some("operator".into()), &JobScope::All, 10_000)
        .await
        .unwrap();
    oracle.until = next.input.until;
    oracle.audit_cutoff = next.input.audit_cutoff;
    assert_eq!(
        serde_json::to_value(
            projection
                .read(since, &ProjectionBinding::from_input(&oracle))
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(replay(&oracle)).unwrap()
    );
}

/// A station of the live registry as the 2026-10-16 census read it.
fn station(name: &str) -> StationSpec {
    let source: Value = serde_json::from_str(include_str!(
        "fixtures/station-predicate-population-20261016.json"
    ))
    .unwrap();
    serde_json::from_value(
        source["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap()
            .clone(),
    )
    .unwrap()
}

struct World {
    db: TestDb,
    jobs: Arc<PgJobs>,
    bus: Arc<RecordingEventBus>,
    projection_workers: std::sync::Mutex<
        Vec<(
            tokio::sync::watch::Sender<bool>,
            tokio::task::JoinHandle<()>,
        )>,
    >,
}

impl World {
    /// The clock fact first, so every instant of the window maps to a
    /// known business date; then the original registry: each station
    /// and the one workflow the packets are admitted under.
    async fn new(stations: &[StationSpec]) -> Self {
        let db = TestDb::new().await;
        let jobs = Arc::new(PgJobs::new(db.pool.clone()));
        sqlx::query(
            "INSERT INTO clock_mapping_mode(id,mode,epoch) VALUES(1,'wall',gen_random_uuid())",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let now = Utc::now();
        let mut tx = db.pool.begin().await.unwrap();
        for spec in stations {
            record_event_in_tx(
                &mut tx,
                &Event::new(
                    "jobs",
                    boss_jobs::events::STATION_PUBLISHED,
                    json!(spec),
                    now,
                ),
            )
            .await
            .unwrap();
        }
        record_event_in_tx(
            &mut tx,
            &Event::new(
                "jobs",
                boss_jobs::events::WORKFLOW_PUBLISHED,
                json!(workflow()),
                now,
            ),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let world = Self {
            db,
            jobs,
            bus: RecordingEventBus::new(),
            projection_workers: Default::default(),
        };
        world.relay().await;
        world
    }

    async fn relay(&self) {
        let bus: Arc<dyn EventBus> = self.bus.clone();
        drain_outbox_once(&self.db.pool, &bus, 10_000)
            .await
            .unwrap();
    }

    async fn admit(&self, job: &Job, steps: &[Step]) {
        let now = Utc::now();
        self.jobs
            .create_job_at(
                job,
                now,
                &[Event::new(
                    "jobs",
                    boss_jobs::events::JOB_CREATED,
                    json!(job),
                    now,
                )],
            )
            .await
            .unwrap();
        for step in steps {
            self.jobs
                .add_step_at(
                    step,
                    now,
                    &[Event::new(
                        "jobs",
                        boss_jobs::events::STEP_CREATED,
                        json!(step),
                        now,
                    )],
                )
                .await
                .unwrap();
        }
    }

    async fn update(&self, job: &Job) {
        let now = Utc::now();
        self.jobs
            .update_job_at(
                job,
                job.status,
                now,
                &[Event::new(
                    "jobs",
                    boss_jobs::events::JOB_UPDATED,
                    json!(job),
                    now,
                )],
            )
            .await
            .unwrap();
    }

    /// A window opening between two database-clock instants, so every
    /// row relayed before it sits below the cut and every later one above.
    async fn open_window(&self) -> DateTime<Utc> {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let since: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&self.db.pool)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        since
    }

    fn app(&self, stations: &[StationSpec]) -> axum::Router {
        let registry = Arc::new(boss_jobs::InMemoryStations::new());
        for spec in stations {
            registry.seed(spec.clone()).unwrap();
        }
        let workflows = Arc::new(boss_jobs::InMemoryWorkflows::for_fixture());
        workflows.seed(workflow()).unwrap();
        let bus_dyn: Arc<dyn EventBus> = self.bus.clone();
        let projection = Arc::new(
            boss_jobs::membership_projection_service::MembershipProjectionService::new(
                self.jobs.clone(),
                Arc::new(
                    boss_jobs::membership_projection_pg::PgMembershipProjection::new(
                        self.db.pool.clone(),
                    ),
                ),
                Arc::new(boss_clock_client::WallClockClient),
            ),
        );
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        let runner = projection.clone();
        let task = tokio::spawn(runner.run(receiver));
        self.projection_workers.lock().unwrap().push((cancel, task));
        boss_jobs::http::router(boss_jobs::http::JobsApiState {
            membership_projection: Some(projection),
            stations: Some(registry),
            kind_registry: Some(workflows),
            // This fixture observes successive native readings rather than
            // the production map's 30-second retention. Keep SWR publication,
            // but allow the next completed map to include the new flow cut.
            map_readings: Arc::new(boss_jobs::http::MapReadings::with_lifetime(
                std::time::Duration::ZERO,
            )),
            ..boss_jobs::http::JobsApiState::minimal(
                self.jobs.clone(),
                self.bus.clone(),
                boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs"),
                Arc::new(
                    FakePolicyClient::builder()
                        .allow("operator", Action::Read, Resource::job(), Scope::All)
                        .build(),
                ),
                Arc::new(boss_clock_client::WallClockClient),
            )
        })
    }

    async fn stop_projection_workers(&self) {
        let workers = std::mem::take(&mut *self.projection_workers.lock().unwrap());
        for (cancel, task) in workers {
            let _ = cancel.send(true);
            task.await.unwrap();
        }
    }
}

fn workflow() -> boss_jobs::registry::WorkflowSpec {
    boss_jobs::registry::WorkflowSpec::platform_seed(
        "fixture",
        "Original",
        "fixture",
        vec!["custom".into()],
        vec![],
    )
}

fn packet(title: &str) -> Job {
    let mut job = Job::new(
        "fixture",
        Subject::new("custom", "membership"),
        title,
        "emp-david",
        Priority::Standard,
        Utc::now().date_naive(),
    );
    job.status = JobStatus::Open;
    job
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let (status, body) = response(app, uri).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{uri}: {body}");
    body
}

async fn response(app: &axum::Router, uri: &str) -> (axum::http::StatusCode, Value) {
    response_as(app, uri, "emp-david").await
}

async fn response_as(
    app: &axum::Router,
    uri: &str,
    actor: &str,
) -> (axum::http::StatusCode, Value) {
    let user = boss_policy_client::User {
        id: actor.into(),
        role: "operator".into(),
        access_tier: boss_policy_client::types::AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("it".into()),
    };
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .header("x-boss-user", serde_json::to_string(&user).unwrap())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn three_distinct_callers_get_real_readings_without_sharing_caller_bound_counts() {
    let watchlist = station("my-watchlist");
    assert!(watchlist.predicate.binds_self());
    let world = World::new(std::slice::from_ref(&watchlist)).await;
    let mut job = packet("Only David's watchlist");
    job.metadata["submitted_by"] = json!("emp-david");
    world.admit(&job, &[]).await;
    world.relay().await;
    let app = world.app(std::slice::from_ref(&watchlist));
    let actors = ["emp-david", "emp-other", "emp-third"];
    for actor in actors {
        let (status, body) = response_as(&app, "/api/yard/flow?window=24h", actor).await;
        assert!(
            status == axum::http::StatusCode::ACCEPTED || body["state"] == "ready",
            "{body}"
        );
    }
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        for actor in actors {
            loop {
                let (_, body) = response_as(&app, "/api/yard/flow?window=24h", actor).await;
                if body["state"] == "ready" {
                    assert!(body["as_of"].is_string());
                    break;
                }
                assert_eq!(body["state"], "pending", "{actor}: {body}");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let (status, counts) = response_as(&app, "/api/stations/flow", actor).await;
            assert_eq!(status, axum::http::StatusCode::OK, "{counts}");
            let row = counts["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["station"] == "my-watchlist")
                .unwrap();
            assert_eq!(row["caller_binding"], actor);
            assert_eq!(
                row["arrived"],
                if actor == "emp-david" { 1 } else { 0 },
                "{row}"
            );
        }
    })
    .await
    .expect("three callers must become ready without a two-minute inter-key delay");
    world.stop_projection_workers().await;
}

fn machine<'a>(map: &'a Value, id: &str) -> &'a Value {
    map["regions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|region| region["machines"].as_array().unwrap())
        .find(|machine| machine["id"] == id)
        .unwrap_or_else(|| panic!("no machine {id} on the map"))
}

/// `my-watchlist` binds the caller (`submitted_by: @me`), names job
/// metadata and names no step — three of the shapes the cube is blind
/// to. A packet the caller submitted arrives inside the window and is
/// dismissed inside it: the map reads that as a counted, idle station,
/// the same crossing the flow surface counts.
#[tokio::test(flavor = "multi_thread")]
async fn a_station_that_binds_the_caller_reads_a_counted_flow_on_the_map() {
    let watchlist = station("my-watchlist");
    assert!(
        watchlist.predicate.binds_self(),
        "the premise: the fixture station binds the caller"
    );
    let world = World::new(std::slice::from_ref(&watchlist)).await;
    let mut job = packet("Watched");
    job.metadata["submitted_by"] = json!("emp-david");
    world.admit(&job, &[]).await;
    job.metadata["watchlist_dismissed"] = json!(true);
    world.update(&job).await;
    let mut another_callers_job = packet("Another caller's watchlist");
    another_callers_job.metadata["submitted_by"] = json!("emp-other");
    world.admit(&another_callers_job, &[]).await;
    world.relay().await;
    let app = world.app(std::slice::from_ref(&watchlist));

    let flow = get(&app, "/api/stations/flow").await;
    let row = flow["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["station"] == "my-watchlist")
        .unwrap_or_else(|| panic!("no my-watchlist row in {flow}"));
    assert_eq!(row["arrived"], 1, "{row}");
    assert_eq!(row["departed"], 1, "{row}");
    assert_eq!(row["caller_binding"], "emp-david", "{row}");

    let cold = get(&app, "/api/yard/regions?window=24h").await;
    assert!(cold["flow_as_of"].is_null(), "{cold}");
    assert!(
        machine(&cold, "station:my-watchlist")["why"]
            .as_str()
            .unwrap()
            .contains("pending"),
        "regions must not start or await history: {cold}"
    );
    let (status, pending) = response(&app, "/api/yard/flow?window=24h").await;
    assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{pending}");
    assert_eq!(pending["state"], "pending");
    assert!(pending["as_of"].is_null());
    let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (status, flow) = response(&app, "/api/yard/flow?window=24h").await;
            if status == axum::http::StatusCode::OK {
                assert_eq!(flow["state"], "ready", "{flow}");
                assert!(flow["as_of"].as_str().is_some(), "{flow}");
                break flow;
            }
            assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{flow}");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("native PostgreSQL membership refresh must become ready");

    let map = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let map = get(&app, "/api/yard/regions?window=24h").await;
            if map["flow_as_of"] == ready["as_of"] {
                break map;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the paired map refresh must publish the completed native flow reading");
    assert_eq!(map["flow_as_of"], ready["as_of"], "{map}");
    let station = machine(&map, "station:my-watchlist");
    assert_eq!(
        station["state"], "idle",
        "a counted station that holds nothing and served nothing is idle, not unjudged: {station}"
    );
    assert_eq!(
        station["why"], "nothing standing, nothing served in the window",
        "{station}"
    );
    world.stop_projection_workers().await;
}

fn counts(reading: &MembershipReading) -> Vec<(String, Option<u64>, Option<u64>, Option<u64>)> {
    reading
        .stations
        .iter()
        .map(|f| (f.station.clone(), f.arrived, f.departed, f.served))
        .collect()
}

/// An open packet of the `q.platform-admin.task` shape: a member while
/// `proof_probe` is absent.
fn task_packet(title: &str) -> (Job, Step) {
    let job = packet(title);
    let mut step = Step::new(job.id, "task", "Task", 0);
    step.status = StepStatus::Ready;
    step.metadata["authority_role"] = json!("platform-admin");
    (job, step)
}

/// The previous window is the wider window minus this one, and both are
/// replayed from ONE snapshot so a crossing between two reads cannot land
/// in the wider count alone (the reason the cube's pair existed, backlog
/// 29f7e897). That is only sound if the narrower replay of the wider
/// snapshot IS the narrower window: here it answers exactly what a
/// snapshot opened at the narrower window answers, crossings on both
/// sides of the boundary.
#[tokio::test(flavor = "multi_thread")]
async fn this_window_replayed_from_the_wider_snapshot_is_this_window() {
    let task = station("q.platform-admin.task");
    let world = World::new(std::slice::from_ref(&task)).await;
    let (mut first, first_step) = task_packet("First");
    world.admit(&first, std::slice::from_ref(&first_step)).await;
    world.relay().await;
    let wide = world.open_window().await;
    // The previous window: First leaves, Second arrives.
    first.metadata["proof_probe"] = json!("left");
    world.update(&first).await;
    let (mut second, second_step) = task_packet("Second");
    world
        .admit(&second, std::slice::from_ref(&second_step))
        .await;
    world.relay().await;
    let narrow = world.open_window().await;
    // This window: Second leaves, Third arrives.
    second.metadata["proof_probe"] = json!("left");
    world.update(&second).await;
    let (third, third_step) = task_packet("Third");
    world.admit(&third, std::slice::from_ref(&third_step)).await;
    world.relay().await;

    let all = JobScope::All;
    let read = |since| {
        world
            .jobs
            .membership_snapshot(since, Some("emp-david".into()), &all, 250_000)
    };
    let [this, both] = replay_windows(read(wide).await.unwrap(), narrow).unwrap();
    let alone = replay(&read(narrow).await.unwrap());
    assert_eq!(
        counts(&this),
        counts(&alone),
        "the narrower replay of the wider snapshot is the narrower window"
    );
    assert_eq!(
        counts(&this),
        vec![("q.platform-admin.task".into(), Some(1), Some(1), Some(0))],
        "this window: Third arrived, Second left"
    );
    assert_eq!(
        counts(&both),
        vec![("q.platform-admin.task".into(), Some(2), Some(2), Some(0))],
        "both windows: Second and Third arrived, First and Second left"
    );
    assert!(
        replay_windows(read(narrow).await.unwrap(), wide).is_err(),
        "a narrower window that opens before the snapshot's is refused, never answered short"
    );
}
