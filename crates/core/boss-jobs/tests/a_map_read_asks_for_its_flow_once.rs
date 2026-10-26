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
//! 1. **State reads never start or await history.** The independent flow
//!    door refreshes membership in its owned task; regions and borders
//!    use only its last successful reading, preserving its instant.
//!    Refused history names its cause rather than becoming fresh counts.
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
    let (state, jobs, _) = state_with_store().await;
    (state, jobs)
}

async fn state_with_store() -> (
    Arc<JobsApiState<InMemoryJobs, RecordingEventBus>>,
    Arc<InMemoryJobs>,
    Arc<boss_jobs::membership_projection::InMemoryMembershipProjection>,
) {
    let jobs = Arc::new(InMemoryJobs::new());
    let store = Arc::new(boss_jobs::membership_projection::InMemoryMembershipProjection::new());
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
            .allow("own", Action::Read, Resource::job(), Scope::Self_)
            .allow(
                "outside",
                Action::Read,
                Resource::job(),
                Scope::Department("ops".into()),
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        membership_projection: Some(Arc::new(
            boss_jobs::membership_projection_service::MembershipProjectionService::new(
                jobs.clone(),
                store.clone(),
                Arc::new(boss_clock_client::WallClockClient),
            ),
        )),
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
    (Arc::new(state), jobs, store)
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

#[tokio::test]
async fn regions_and_borders_share_one_completed_map_reading() {
    let (state, jobs) = state().await;
    let app = router_shared(state);
    let regions = get(&app, "/api/yard/regions?window=24h").await;
    let car = JobId::from_uuid(Uuid::parse_str(JOB).unwrap());
    let steps_after_regions = jobs.steps_reads(&car);
    jobs.fail_jobs_list("ship-a-change", 0);

    // Judge the actual shared-read behavior before the new wire field:
    // a second independent build hits the injected list refusal (500).
    let borders = get(&app, "/api/yard/borders?window=24h").await;
    assert_eq!(
        jobs.steps_reads(&car),
        steps_after_regions,
        "borders must reuse the completed paired map, not read car steps again"
    );
    assert!(
        regions["map_as_of"].as_str().is_some(),
        "a completed map must retain its actual reading instant: {regions}"
    );
    assert_eq!(borders["map_as_of"], regions["map_as_of"]);
}

#[tokio::test(start_paused = true)]
async fn a_stale_map_keeps_its_reading_and_names_a_failed_refresh_on_both_surfaces() {
    let (state, jobs) = state().await;
    let app = router_shared(state);
    let regions = get(&app, "/api/yard/regions?window=24h").await;
    let borders = get(&app, "/api/yard/borders?window=24h").await;
    assert!(regions["map_as_of"].as_str().is_some(), "{regions}");
    assert_eq!(regions["map_as_of"], borders["map_as_of"]);
    jobs.fail_jobs_list("ship-a-change", 0);
    tokio::time::advance(StdDuration::from_secs(31)).await;
    let stale = get(&app, "/api/yard/regions?window=24h").await;
    assert_eq!(stale["map_as_of"], regions["map_as_of"]);
    assert_eq!(
        stale["regions"], regions["regions"],
        "refresh must not erase the good counts"
    );
    let failed = tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            let reading = get(&app, "/api/yard/regions?window=24h").await;
            if reading["map_refresh_error"].as_str().is_some() {
                break reading;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("stale success must expose the refused refresh, not silently hide it");
    let reason = failed["map_refresh_error"].as_str().unwrap();
    assert!(
        reason.contains("ship-a-change list at offset 0 unreadable (injected by fail_jobs_list)"),
        "{failed}"
    );
    assert_eq!(failed["map_as_of"], regions["map_as_of"]);
    assert_eq!(failed["regions"], regions["regions"]);
    let stale_borders = get(&app, "/api/yard/borders?window=24h").await;
    assert_eq!(stale_borders["map_as_of"], borders["map_as_of"]);
    assert_eq!(stale_borders["borders"], borders["borders"]);
    assert_eq!(
        stale_borders["map_refresh_error"],
        failed["map_refresh_error"]
    );
}

async fn map_as(app: &axum::Router, uri: &str, user: &Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("x-boss-user", user.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn a_completed_map_is_not_shared_across_window_scope_caller_or_full_user() {
    let operator: Value = serde_json::from_str(&user_header("operator")).unwrap();
    let owner: Value = serde_json::from_str(&user_header("own")).unwrap();
    let mut another_caller = operator.clone();
    another_caller["id"] = serde_json::json!("emp-other");
    let mut changed_user = operator.clone();
    changed_user["direct_report_ids"] = serde_json::json!(["emp-report"]);
    for (case, uri, user) in [
        ("window", "/api/yard/borders?window=48h", operator),
        ("translated scope", "/api/yard/borders?window=24h", owner),
        (
            "actual caller",
            "/api/yard/borders?window=24h",
            another_caller,
        ),
        ("full user", "/api/yard/borders?window=24h", changed_user),
    ] {
        let (state, jobs) = state().await;
        let app = router_shared(state);
        let warm = get(&app, "/api/yard/regions?window=24h").await;
        assert!(warm["map_as_of"].as_str().is_some(), "{case}: {warm}");
        jobs.fail_jobs_list("ship-a-change", 0);
        let (status, unavailable) = tokio::time::timeout(
            StdDuration::from_secs(2),
            map_as(&app, uri, &user),
        )
        .await
        .expect(
            "a completed cold refusal must return its cause without waiting through cache backoff",
        );
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{case}: {unavailable}"
        );
        assert!(unavailable["map_as_of"].is_null(), "{case}: {unavailable}");
        assert!(
            unavailable["why"]
                .as_str()
                .unwrap()
                .contains("ship-a-change list at offset 0 unreadable (injected by fail_jobs_list)"),
            "a distinct {case} must expose its own failed read, never the other key's map: {unavailable}"
        );
    }
}

#[tokio::test]
async fn a_denied_map_caller_cannot_reuse_a_privileged_completed_map() {
    let (state, jobs) = state().await;
    let app = router_shared(state);
    let warm = get(&app, "/api/yard/regions?window=24h").await;
    assert!(warm["map_as_of"].as_str().is_some(), "{warm}");
    let car = JobId::from_uuid(Uuid::parse_str(JOB).unwrap());
    let before = jobs.steps_reads(&car);
    jobs.fail_jobs_list("ship-a-change", 0);
    let denied: Value = serde_json::from_str(&user_header("no-grant")).unwrap();
    let (status, map) = map_as(&app, "/api/yard/regions?window=24h", &denied).await;
    assert_eq!(status, StatusCode::OK, "{map}");
    assert!(
        map["regions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["name"] != "sensors")
            .all(|r| r["count"] == 0),
        "denied scope is an empty map, never a privileged cached count: {map}"
    );
    assert_eq!(
        jobs.steps_reads(&car),
        before,
        "denied scope reads no car steps"
    );
    let sensors = map["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "sensors")
        .unwrap();
    assert!(
        sensors["count"].is_null(),
        "unread telemetry must not become an invented zero: {sensors}"
    );
}

struct ChangingPolicy(std::sync::RwLock<Arc<FakePolicyClient>>);

#[async_trait::async_trait]
impl PolicyClient for ChangingPolicy {
    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
        let policy = self.0.read().unwrap().clone();
        policy.check(user, action, resource).await
    }

    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
        let policy = self.0.read().unwrap().clone();
        policy.scope_predicate(user, resource).await
    }
}

#[tokio::test]
async fn a_cached_map_reauthorizes_the_same_user_and_separates_changed_estate_grants() {
    let (state, jobs) = state().await;
    let full = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("operator", Action::Read, Resource::estate(), Scope::All)
            .build(),
    );
    let policy = Arc::new(ChangingPolicy(std::sync::RwLock::new(full.clone())));
    let state = Arc::new(JobsApiState {
        policy: policy.clone(),
        ..Arc::try_unwrap(state).ok().unwrap()
    });
    let app = router_shared(state);
    let warm = get(&app, "/api/yard/regions?window=24h").await;
    assert!(warm["map_as_of"].as_str().is_some(), "{warm}");
    jobs.fail_jobs_list("ship-a-change", 0);

    // Identical User and job scope, changed estate grant: a distinct
    // snapshot must be built, rather than leaking the former estate map.
    *policy.0.write().unwrap() = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let (status, unread) = flow(&app, "/api/yard/borders?window=24h", "operator").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{unread}");
    assert!(unread["map_as_of"].is_null(), "{unread}");
    assert!(
        unread["why"]
            .as_str()
            .unwrap()
            .contains("ship-a-change list at offset 0 unreadable")
    );

    // Revoking read authority on the very same cached identity must
    // take effect before any cache hit is served.
    *policy.0.write().unwrap() = Arc::new(FakePolicyClient::builder().build());
    let denied = get(&app, "/api/yard/regions?window=24h").await;
    assert!(
        denied["regions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["name"] != "sensors")
            .all(|r| r["count"] == 0)
    );
    assert!(
        denied["regions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "sensors")
            .unwrap()["count"]
            .is_null()
    );

    *policy.0.write().unwrap() = full;
    let restored = get(&app, "/api/yard/borders?window=24h").await;
    assert_eq!(restored["map_as_of"], warm["map_as_of"]);
}

#[tokio::test(start_paused = true)]
async fn three_flow_callers_progress_without_another_callers_cooldown() {
    let (state, jobs) = state().await;
    let app = router_shared(state.clone());
    let service = state.membership_projection.as_ref().unwrap().clone();
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let owner = tokio::spawn(service.clone().run(receiver));
    assert_eq!(
        flow(&app, "/api/yard/flow?window=24h", "operator").await.0,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(StdDuration::from_secs(2), async {
        while service
            .peek(
                &boss_jobs::membership_projection::ProjectionIdentity::new(
                    Some("emp-david".into()),
                    boss_jobs::port::JobScope::All,
                ),
                24,
            )
            .await
            .is_ok()
        {
            tokio::time::sleep(StdDuration::from_millis(1)).await;
        }
    })
    .await
    .expect("the first native refusal must finish before measuring cooldown");
    assert_eq!(
        jobs.membership_reads(),
        2,
        "the first refused wider read retries this window"
    );
    let mut other: Value = serde_json::from_str(&user_header("operator")).unwrap();
    other["id"] = serde_json::json!("emp-other");
    let (status, pending) = map_as(&app, "/api/yard/flow?window=24h", &other).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{pending}");
    assert!(pending["as_of"].is_null());
    let mut third = other.clone();
    third["id"] = serde_json::json!("emp-third");
    map_as(&app, "/api/yard/flow?window=24h", &third).await;
    tokio::time::timeout(StdDuration::from_secs(2), async {
        while jobs.membership_reads() < 6 {
            tokio::time::sleep(StdDuration::from_millis(1)).await;
        }
    })
    .await
    .expect("all three independent callers must finish without a global cooldown");
    assert_eq!(jobs.membership_reads(), 6);
    flow(&app, "/api/yard/flow?window=24h", "operator").await;
    tokio::task::yield_now().await;
    assert_eq!(
        jobs.membership_reads(),
        6,
        "the first key still backs off its own refusal"
    );
    cancel.send(true).unwrap();
    owner.await.unwrap();
}

struct HeldMapPolicy {
    rows: FakePolicyClient,
    job_reads: std::sync::atomic::AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl PolicyClient for HeldMapPolicy {
    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
        self.rows.check(user, action, resource).await
    }

    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
        if user.id == "emp-david"
            && resource.as_str() == "job"
            && self
                .job_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 1
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.rows.scope_predicate(user, resource).await
    }
}

#[tokio::test(start_paused = true)]
async fn a_cold_map_still_building_after_25_seconds_is_not_an_outage() {
    let (state, _) = state().await;
    let policy = Arc::new(HeldMapPolicy {
        rows: FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("operator", Action::Read, Resource::estate(), Scope::All)
            .build(),
        job_reads: std::sync::atomic::AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let state = Arc::new(JobsApiState {
        policy: policy.clone(),
        ..Arc::try_unwrap(state).ok().unwrap()
    });
    let app = router_shared(state);
    let user: Value = serde_json::from_str(&user_header("operator")).unwrap();
    let first_app = app.clone();
    let first_user = user.clone();
    let first = tokio::spawn(async move {
        map_as(&first_app, "/api/yard/regions?window=24h", &first_user).await
    });
    tokio::time::timeout(StdDuration::from_secs(1), policy.entered.notified())
        .await
        .expect("the owned map build must reach its held policy read");
    let mut other = user;
    other["id"] = serde_json::json!("emp-other");
    let (status, other_map) = map_as(&app, "/api/yard/regions?window=24h", &other).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "another caller is not blocked: {other_map}"
    );
    assert!(other_map["map_as_of"].as_str().is_some());
    tokio::time::advance(StdDuration::from_secs(26)).await;
    let (status, pending) = first.await.unwrap();
    assert_eq!(status, StatusCode::ACCEPTED, "{pending}");
    assert_eq!(pending["state"], "building");
    assert!(pending["map_as_of"].is_null());
    assert!(
        pending["why"]
            .as_str()
            .unwrap()
            .contains("map reading pending"),
        "{pending}"
    );
    policy.release.notify_one();
    let ready = get(&app, "/api/yard/regions?window=24h").await;
    assert!(
        ready["map_as_of"].as_str().is_some(),
        "the background task survived waiter timeout: {ready}"
    );
    let borders = get(&app, "/api/yard/borders?window=24h").await;
    assert_eq!(borders["map_as_of"], ready["map_as_of"]);
}

async fn flow(app: &axum::Router, uri: &str, role: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("x-boss-user", user_header(role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::json!(String::from_utf8_lossy(&bytes))),
    )
}

async fn seed_flow(
    store: &boss_jobs::membership_projection::InMemoryMembershipProjection,
    hours: i64,
    as_of: DateTime<Utc>,
) {
    use boss_clock_client::{
        ClockMode,
        mapping_history::{ClockMappingFact, ClockTimeline},
    };
    use boss_jobs::membership_projection::{MembershipProjectionRepository, ProjectionIdentity};
    use boss_jobs::membership_replay::{
        InitialState, ReplayInput, projection::MembershipProjection,
    };
    let since = as_of - Duration::hours(hours);
    let clock = ClockMappingFact {
        schema_version: 1,
        revision: "fixture-original-clock".into(),
        previous_revision: None,
        epoch: Some("fixture-original-epoch".into()),
        mode: Some(ClockMode::Wall),
        params: None,
        captured_at: since - Duration::seconds(2),
        audit_created_at: since - Duration::seconds(1),
        seq: 1,
        complete_group: true,
    };
    let input = ReplayInput {
        groups: vec![],
        since,
        until: as_of,
        audit_cutoff: 1,
        actor: Some("emp-david".into()),
        scope: boss_jobs::port::JobScope::All,
        currently_authorized: None,
        clock: ClockTimeline::build(&[clock], Some("fixture-original-clock"), since, as_of),
        registry_prefix_complete: true,
        packet_prefix_complete: true,
        source_unavailable: vec![],
        initial: InitialState::default(),
    };
    let checkpoint = MembershipProjection::rebuild(&input).unwrap();
    store
        .commit(
            &ProjectionIdentity::new(Some("emp-david".into()), boss_jobs::port::JobScope::All),
            None,
            &checkpoint,
        )
        .await
        .unwrap();
}

fn region<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no region {name} in {v}"))
}

/// A cold history scan must not sit on the page's state endpoints.
#[tokio::test]
async fn regions_and_borders_do_not_start_or_wait_for_a_cold_flow() {
    let (state, jobs) = state().await;
    let app = router_shared(state);
    for uri in ["/api/yard/regions", "/api/yard/borders"] {
        let value = tokio::time::timeout(StdDuration::from_secs(2), get(&app, uri))
            .await
            .expect("state read is independent of the history snapshot");
        assert!(
            value["flow_as_of"].is_null(),
            "cold counts are not a reading: {value}"
        );
        assert_eq!(jobs.membership_reads(), 0, "{uri} starts no history read");
    }
}

#[tokio::test]
async fn the_independent_flow_door_reports_pending_without_waiting() {
    let (state, _) = state().await;
    let app = router_shared(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/yard/flow")
                .header("x-boss-user", user_header("operator"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["state"], "pending");
    assert!(value["as_of"].is_null());
    assert_eq!(value["window_hours"], 24);
    assert!(value["why"].as_str().is_some_and(|why| !why.is_empty()));
}

/// Only the independent door asks the wider membership window and its
/// narrower fallback. Neither state endpoint reads the step-events cube
/// or promotes a refused refresh into a successful reading.
#[tokio::test]
async fn the_independent_door_asks_membership_and_never_the_step_cube() {
    let (state, jobs) = state().await;
    let service = state.membership_projection.as_ref().unwrap().clone();
    let app = router_shared(state);
    assert_eq!(
        flow(&app, "/api/yard/flow", "operator").await.0,
        StatusCode::ACCEPTED
    );
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let owner = tokio::spawn(service.clone().run(receiver));
    tokio::time::timeout(StdDuration::from_secs(2), async {
        while service
            .peek(
                &boss_jobs::membership_projection::ProjectionIdentity::new(
                    Some("emp-david".into()),
                    boss_jobs::port::JobScope::All,
                ),
                24,
            )
            .await
            .is_ok()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let v = tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            let map = get(&app, "/api/yard/regions").await;
            if map
                .to_string()
                .contains("native membership history unavailable")
            {
                break map;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the independent refusal must remain visible without starting another read");
    assert_eq!(
        jobs.membership_reads(),
        2,
        "only the independent door asks history"
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
            .contains("storage failure: native membership history unavailable"),
        "a refused refresh is not published as a completed reading: {station}"
    );

    // Borders also leaves the refused background read alone.
    let b = get(&app, "/api/yard/borders").await;
    assert_eq!(
        jobs.membership_reads(),
        2,
        "the borders read shares the regions read's flow reading"
    );
    assert_eq!(jobs.flow_cube_passes(), 0, "the map never reads the cube");
    // And both say WHEN that reading was taken — one instant, so a
    // served reading is never presented as one taken at this read's now.
    assert!(
        v["flow_as_of"].is_null(),
        "no successful reading was taken: {v}"
    );
    assert_eq!(
        v["flow_as_of"], b["flow_as_of"],
        "the two answers name the one reading"
    );
    cancel.send(true).unwrap();
    owner.await.unwrap();
}

/// Concurrent state reads share the completed reading without running
/// history. Cache single-flight controls separately cover the refresh.
#[tokio::test]
async fn concurrent_map_reads_share_completed_flow_without_history() {
    let (state, jobs, store) = state_with_store().await;
    let as_of = Utc::now();
    seed_flow(&store, 48, as_of).await;
    let app = router_shared(state);
    let reads = (0..4).map(|i| {
        let app = app.clone();
        async move {
            let uri = if i % 2 == 0 {
                "/api/yard/regions"
            } else {
                "/api/yard/borders"
            };
            let value = get(&app, uri).await;
            assert_eq!(value["flow_as_of"], serde_json::json!(as_of));
            value
        }
    });
    futures::future::join_all(reads).await;
    assert_eq!(
        jobs.membership_reads(),
        0,
        "four concurrent map reads only consume the last reading"
    );
}

/// A stale durable generation is named unread, with its original instant
/// retained in the diagnosis. It is never relabelled as a fresh zero.
#[tokio::test]
async fn a_stale_generation_is_unread_and_names_its_original_instant() {
    let (state, jobs, store) = state_with_store().await;
    let as_of = Utc::now() - Duration::minutes(5);
    seed_flow(&store, 24, as_of).await;
    let app = router_shared(state);
    let (status, value) = flow(&app, "/api/yard/flow", "operator").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["state"], "unread");
    assert!(value["as_of"].is_null());
    let why = value["why"].as_str().unwrap();
    assert!(
        why.contains("stale") && why.contains(&as_of.to_string()),
        "{why}"
    );
    for uri in ["/api/yard/regions", "/api/yard/borders"] {
        let value = get(&app, uri).await;
        assert!(
            value["flow_as_of"].is_null(),
            "stale generation is not a fresh reading"
        );
        if uri.ends_with("regions") {
            assert!(value.to_string().contains("stale"), "{value}");
        }
    }
    assert_eq!(
        jobs.membership_reads(),
        0,
        "neither stale flow status nor state views run history in the request"
    );
}

#[tokio::test(start_paused = true)]
async fn flow_keys_separate_windows_and_translated_scopes_and_reject_bad_windows() {
    let (state, jobs, store) = state_with_store().await;
    seed_flow(&store, 24, Utc::now()).await;
    let app = router_shared(state.clone());
    assert_eq!(
        flow(&app, "/api/yard/flow?window=24h", "operator").await.1["state"],
        "ready"
    );
    let (status, value) = flow(&app, "/api/yard/flow?window=48h", "operator").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["state"], "unread");
    assert!(
        value["why"]
            .as_str()
            .is_some_and(|why| why.contains("retained") || why.contains("window")),
        "{value}"
    );
    assert_eq!(value["window_hours"], 48);
    assert!(value["as_of"].is_null());
    let (status, value) = flow(&app, "/api/yard/flow", "own").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(
        value["as_of"].is_null(),
        "a full-scope reading cannot satisfy owner scope"
    );
    assert_eq!(
        jobs.membership_reads(),
        0,
        "scope and retained-window refusals never fall back to request history"
    );
    let before = jobs.membership_reads();
    assert_eq!(
        flow(&app, "/api/yard/flow?window=0", "operator").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        jobs.membership_reads(),
        before,
        "invalid windows start no task"
    );
}

#[tokio::test]
async fn a_denied_translated_scope_is_withheld_without_starting_history() {
    let (state, jobs, store) = state_with_store().await;
    seed_flow(&store, 24, Utc::now()).await;
    let app = router_shared(state);
    for role in ["outside", "no-grant"] {
        let (status, value) = flow(&app, "/api/yard/flow", role).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["state"], "withheld");
        assert!(value["as_of"].is_null());
        assert!(value["why"].as_str().is_some_and(|why| !why.is_empty()));
    }
    assert_eq!(jobs.membership_reads(), 0);
}

#[tokio::test]
async fn flow_requires_the_station_registry_before_starting_history() {
    let (state, jobs) = state().await;
    let state = Arc::new(JobsApiState {
        stations: None,
        ..Arc::try_unwrap(state).ok().unwrap()
    });
    let (status, value) = flow(&router_shared(state), "/api/yard/flow", "operator").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        value["why"]
            .as_str()
            .unwrap()
            .contains("station registry unavailable")
    );
    assert_eq!(jobs.membership_reads(), 0);
}

#[tokio::test]
async fn a_reading_for_another_actual_caller_is_not_shared() {
    let (state, _, store) = state_with_store().await;
    seed_flow(&store, 24, Utc::now()).await;
    let mut user: Value = serde_json::from_str(&user_header("operator")).unwrap();
    user["id"] = serde_json::json!("another-actor");
    let response = router_shared(state)
        .oneshot(
            Request::builder()
                .uri("/api/yard/flow")
                .header("x-boss-user", user.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value["as_of"].is_null());
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
