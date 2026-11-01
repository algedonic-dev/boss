//! `POST /api/jobs/{id}/release`, end to end through the real router,
//! as a RULE and as a PERSON, on a car reserved for a person's own
//! release and on one that is not (backlog 60993cda, from builder run
//! e2fec1ad on item 0cfc57e6).
//!
//! WHY THIS EXISTS. The release rule (`release-a-held-car-on-its-
//! reviewer-verdict`) made an automation a caller of the release door,
//! and two facts it rests on were pinned only below HTTP: the record's
//! `by` (`release_actor`, a pure function in `http/release.rs`) and the
//! judge's refusal of an automation on a car carrying `releaser`
//! (`car_release::may_release_by`, unit tests). Neither pin could see
//! the door itself: that the header a rule sends is the actor the record
//! credits, that the merge event the release writes names the same
//! actor, that a reserved car's refusal leaves the hold on, and that a
//! body cannot name its own `by`. The release door is a trust boundary
//! (area policy): every answer here is the door's, read back from the
//! store, never a function's return.
//!
//! THE SHAPE TESTED is the live one: a `ship-a-change` car whose review
//! step is held at H1 with its dispatched reviewer named as `agent_run`,
//! and that reviewer's run recording a RELEASE verdict at H1 — the shape
//! of car a_held_cars_row_carries_what_its_release_rests_on in
//! yard_status_http.rs, which the yard reads releasable. The rule's
//! caller is `dispatcher_actor_header`'s exact shape (`id:
//! "rule:<name>"`, platform-admin), the person is `emp-david`, and both
//! hold the same policy grants, so every difference below is the
//! judge's.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

#[path = "the_release_door_credits_its_caller_and_keeps_a_reserved_car_http/raw_hold_readback.rs"]
mod raw_hold_readback;

#[tokio::test]
async fn gate_submission_poison_cannot_start_a_repoint_or_bare_release_sequence() {
    for bare in [false, true] {
        let (app, jobs) = app();
        let (car, run) = held_car(&jobs, 1300, json!({"releaser":"emp-david"})).await;
        let original = review_md(&jobs, car).await;
        let events = jobs.recorded_events().len();
        let (status, text) = metadata_patch(
            &app,
            &jobs,
            "agent-codex",
            car,
            json!({"gate_submission":{}}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "poison must be refused: {text}"
        );
        assert_eq!(review_md(&jobs, car).await, original);
        assert_eq!(jobs.recorded_events().len(), events);
        let status = if bare {
            bare_release(&app, &jobs, "agent-codex", car).await.0
        } else {
            metadata_patch(
                &app,
                &jobs,
                "agent-codex",
                car,
                json!({"releaser":"agent-codex"}),
            )
            .await
            .0
        };
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            metadata_patch(
                &app,
                &jobs,
                "agent-codex",
                car,
                json!({"gate_submission":null})
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            release(&app, &caller("agent-codex"), car, ask(run)).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(review_md(&jobs, car).await, original);
        assert_eq!(jobs.recorded_events().len(), events);
    }
}

#[tokio::test]
async fn preexisting_gate_submission_poison_does_not_erase_raw_reservation_authority() {
    for patch in [json!({"releaser":"agent-codex"}), json!({"hold":null})] {
        let (app, jobs) = app();
        let (car, _) = held_car(
            &jobs,
            1301,
            json!({"releaser":"emp-david", "gate_submission":{}}),
        )
        .await;
        let original = review_md(&jobs, car).await;
        assert_eq!(
            boss_jobs::stranded::marked(&original, "hold").as_deref(),
            Some(HOLD)
        );
        assert_eq!(boss_jobs::stranded::hold_reason(&original), None);
        let events = jobs.recorded_events().len();
        let (status, text) = metadata_patch(&app, &jobs, "agent-codex", car, patch).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "raw held reservation must govern: {text}"
        );
        assert!(text.contains("emp-david"), "{text}");
        assert_eq!(review_md(&jobs, car).await, original);
        assert_eq!(jobs.recorded_events().len(), events);
    }
}

const NOW: &str = "2026-10-18T12:00:00Z";
#[tokio::test]
async fn conditional_evidence_cannot_clear_or_repoint_a_reserved_hold() {
    for who in ["agent-codex", "automation:rule:release-a-held-car"] {
        for poison in [json!({}), json!(null)] {
            for evidence in [
                json!({"hold":null,"releaser":null,"pr_url":"https://example.invalid/pr/1"}),
                json!({"releaser":"agent-codex","pr_url":"https://example.invalid/pr/1"}),
            ] {
                let (app, jobs) = app();
                let (car, _) = held_car(
                    &jobs,
                    1400,
                    json!({"releaser":"emp-david","gate_submission":poison}),
                )
                .await;
                let steps = jobs.list_steps(&JobId::from_uuid(car)).await.unwrap();
                let step = &steps[0];
                let original = step.clone();
                let events = jobs.recorded_events().len();
                let response = app.oneshot(Request::builder()
                .method("POST")
                .uri(format!("/api/jobs/{car}/steps/{}/complete-if", step.id))
                .header("content-type", "application/json")
                .header("x-boss-user", caller(who))
                .body(Body::from(json!({"operation_id":Uuid::new_v4(),"expected":{"status":"active","assignee_id":null},"evidence":evidence}).to_string())).unwrap()).await.unwrap();
                let status = response.status();
                let body = response.into_body().collect().await.unwrap().to_bytes();
                assert_eq!(
                    status,
                    StatusCode::CONFLICT,
                    "reservation evidence must be refused: {}",
                    String::from_utf8_lossy(&body)
                );
                let after = jobs.get_step(&step.id).await.unwrap().unwrap();
                assert_eq!(
                    serde_json::to_value(after).unwrap(),
                    serde_json::to_value(original).unwrap()
                );
                assert_eq!(jobs.recorded_events().len(), events);
            }
        }
    }
}

#[tokio::test]
async fn conditional_evidence_owner_can_clear_the_reservation() {
    let (app, jobs) = app();
    let (car, _) = held_car(&jobs, 1401, json!({"releaser":"emp-david"})).await;
    let step = jobs
        .list_steps(&JobId::from_uuid(car))
        .await
        .unwrap()
        .remove(0);
    let response = app.oneshot(Request::builder().method("POST")
        .uri(format!("/api/jobs/{car}/steps/{}/complete-if", step.id))
        .header("content-type", "application/json").header("x-boss-user", caller("emp-david"))
        .body(Body::from(json!({"operation_id":Uuid::new_v4(),"expected":{"status":"active","assignee_id":null},"evidence":{"hold":null,"releaser":null,"pr_url":"https://example.invalid/pr/1"}}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(after.status, StepStatus::Completed);
    assert!(after.metadata.get("hold").is_none());
    assert!(after.metadata.get("releaser").is_none());
}

#[tokio::test]
async fn malformed_gate_envelope_cannot_hide_review_hold_from_yard_or_release_ask() {
    for poison in [json!({}), json!(null)] {
        let (_, jobs) = app();
        let (id, _) = held_car(
            &jobs,
            1402,
            json!({"releaser":"emp-david","gate_submission":poison}),
        )
        .await;
        let job = jobs.get_job(&JobId::from_uuid(id)).await.unwrap().unwrap();
        let steps = jobs.list_steps(&job.id).await.unwrap();
        assert_eq!(
            boss_jobs::yard::car_hold_reason(&steps).as_deref(),
            Some(HOLD)
        );
        let (parked, held) = boss_jobs::yard::dock_lanes(&[(job.clone(), steps.clone())]);
        assert!(parked.is_empty());
        assert_eq!(held.len(), 1);
        let car = json!({"id":id,"metadata":job.metadata,"steps":steps});
        assert_eq!(
            boss_jobs::car_release::JudgedHold::of(&car)
                .reason
                .as_deref(),
            Some(HOLD)
        );
    }
}

#[tokio::test]
async fn malformed_gate_envelope_cannot_hide_the_hold_sent_to_release() {
    for poison in [json!({}), json!(null)] {
        let (_, jobs) = app();
        let (id, _) = held_car(
            &jobs,
            1403,
            json!({"releaser":"emp-david","gate_submission":poison}),
        )
        .await;
        let steps = jobs.list_steps(&JobId::from_uuid(id)).await.unwrap();
        let car = json!({"id":id,"steps":steps});
        assert_eq!(
            boss_jobs::car_release::JudgedHold::of(&car)
                .reason
                .as_deref(),
            Some(HOLD)
        );
    }
}

#[tokio::test]
async fn conditional_evidence_alias_judgment_is_fenced_by_the_original_row_version() {
    let race = Arc::new(std::sync::Mutex::new(None));
    let actors = Arc::new(NativeAliases {
        inner: boss_jobs::agents::InMemoryAgents::new()
            .with_agent("agent-owner", ["owner@login.example"]),
        race: race.clone(),
        unavailable: false,
    });
    let (app, jobs) = app_with_actors(Some(actors));
    let (car, _) = held_car(&jobs, 1404, json!({"releaser":"agent-owner"})).await;
    let step = jobs
        .list_steps(&JobId::from_uuid(car))
        .await
        .unwrap()
        .remove(0);
    *race.lock().unwrap() = Some((jobs.clone(), step.id));
    let before = jobs.recorded_events().len();
    let response = app.oneshot(Request::builder().method("POST")
        .uri(format!("/api/jobs/{car}/steps/{}/complete-if", step.id))
        .header("content-type", "application/json").header("x-boss-user", caller("owner@login.example"))
        .body(Body::from(json!({"operation_id":Uuid::new_v4(),"expected":{"status":"active","assignee_id":null},"evidence":{"hold":null,"releaser":null,"pr_url":"https://example.invalid/pr/1"}}).to_string())).unwrap()).await.unwrap();
    assert!(
        race.lock().unwrap().is_none(),
        "completion must judge the native alias snapshot"
    );
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(after.status, StepStatus::Active);
    assert_eq!(after.metadata["releaser"], "agent-other");
    assert_eq!(after.metadata["hold"], HOLD);
    assert!(after.metadata.get("pr_url").is_none());
    assert_eq!(
        jobs.recorded_events().len(),
        before + 1,
        "only controlled reservation change writes"
    );
}

const H1: &str = "1111111111111111111111111111111111111111";
const RULE: &str = "release-a-held-car-on-its-reviewer-verdict";
const HOLD: &str = "trust-boundary car (area policy): waits for its adversarial review";

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

/// The caller as the door's `x-boss-user` header carries it — the shape
/// `boss_dispatcher::rules::actor::dispatcher_actor_header` sends.
fn caller(id: &str) -> String {
    json!({
        "id": id,
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn the_rule() -> String {
    caller(&format!("rule:{RULE}"))
}

fn app() -> (Router, Arc<InMemoryJobs>) {
    app_with_actors(None)
}

fn app_with_actors(
    actors: Option<Arc<dyn boss_jobs::agents::AgentsRegistry>>,
) -> (Router, Arc<InMemoryJobs>) {
    app_with_race(actors, Arc::new(std::sync::Mutex::new(None)))
}

type ReservationRace = Arc<std::sync::Mutex<Option<StepId>>>;

struct RacePolicy {
    inner: Arc<dyn PolicyClient>,
    jobs: Arc<InMemoryJobs>,
    race: ReservationRace,
}

#[async_trait::async_trait]
impl PolicyClient for RacePolicy {
    async fn check(
        &self,
        user: &boss_policy_client::User,
        action: Action,
        resource: Resource,
    ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
        if action == Action::Update {
            let step = self.race.lock().unwrap().take();
            if let Some(step) = step {
                let patch = json!({boss_jobs::car::RELEASER:"emp-another"});
                self.jobs
                    .merge_step_metadata_at(
                        &step,
                        patch.as_object().unwrap(),
                        &boss_core::publisher::EventStamp::new(
                            "jobs",
                            boss_core::actor::ActorId::Automation("test-race".into()),
                        ),
                    )
                    .await
                    .unwrap();
            }
        }
        self.inner.check(user, action, resource).await
    }
    async fn scope_predicate(
        &self,
        user: &boss_policy_client::User,
        resource: Resource,
    ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
        self.inner.scope_predicate(user, resource).await
    }
}

fn app_with_race(
    actors: Option<Arc<dyn boss_jobs::agents::AgentsRegistry>>,
    race: ReservationRace,
) -> (Router, Arc<InMemoryJobs>) {
    app_with_race_and_roster(actors, race, None)
}

fn app_with_race_and_roster(
    actors: Option<Arc<dyn boss_jobs::agents::AgentsRegistry>>,
    race: ReservationRace,
    roster: Option<Arc<dyn boss_jobs::owner_resolution::RosterLookup>>,
) -> (Router, Arc<InMemoryJobs>) {
    app_with_readback_rehold(actors, race, roster, None)
}

fn app_with_readback_rehold(
    actors: Option<Arc<dyn boss_jobs::agents::AgentsRegistry>>,
    race: ReservationRace,
    roster: Option<Arc<dyn boss_jobs::owner_resolution::RosterLookup>>,
    rehold: Option<ReservationRace>,
) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let stations = Arc::new(boss_jobs::InMemoryStations::new());
    let mut dock = boss_jobs::StationSpec::draft(
        "loading-dock",
        "Loading dock",
        boss_jobs::StationKind::Batch,
        boss_jobs::station_queue::StationPredicate {
            kind: Some("ship-a-change".into()),
            status: Some(JobStatus::Open),
            metadata_present: vec!["branch".into()],
            step: Some(boss_jobs::station_queue::StepMatch {
                slug: Some("review".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                metadata_unmarked: vec!["hold".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        t(NOW),
    );
    dock.status = boss_jobs::registry::WorkflowStatus::Active;
    stations.seed(dock).unwrap();
    let kinds = Arc::new(boss_jobs::InMemoryWorkflows::for_fixture());
    let mut spec = boss_jobs::WorkflowSpec::platform_seed(
        "ship-a-change",
        "Ship a change",
        "test",
        vec!["custom".into()],
        vec![boss_jobs::StepSpec {
            title: "review".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: boss_jobs::car::REVIEW.into(),
            ..Default::default()
        }],
    );
    spec.version = 16;
    kinds.seed(spec).unwrap();
    // The SAME grants for both callers: a refusal below is the judge's,
    // never the policy's.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(raw_hold_readback::registry(kinds, jobs.clone(), rehold)),
        stations: Some(stations),
        release_actors: actors,
        roster,
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            Arc::new(RacePolicy {
                inner: policy,
                jobs: jobs.clone(),
                race,
            }),
            Arc::new(boss_clock_client::FixedClockClient::new(
                boss_clock_client::ClockNow {
                    now: t(NOW),
                    simulated: false,
                    epoch_start: None,
                    epoch_end: None,
                    paused: false,
                    restart_in_progress: false,
                    warp_factor: None,
                },
            )),
        )
    };
    (router(state), jobs)
}

fn job(kind: &str, id: Uuid, status: JobStatus, metadata: Value) -> Job {
    Job {
        id: JobId::from_uuid(id),
        workflow_version: 16,
        status,
        metadata,
        ..Job::new(
            kind,
            Subject::new("custom", "s"),
            "a car",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 18).unwrap(),
        )
    }
}

/// A held car at H1 and its dispatched reviewer's RELEASE at H1. `mark`
/// is merged onto the review step (the `releaser` reservation, or
/// nothing). Returns (car, run).
async fn held_car(jobs: &InMemoryJobs, n: u128, mark: Value) -> (Uuid, Uuid) {
    let car = Uuid::from_u128(0xca00 + n);
    let run = Uuid::from_u128(0x5ee0 + n);
    let builder = Uuid::from_u128(0xb000 + n);
    jobs.create_job_at(
        &job(
            "ship-a-change",
            car,
            JobStatus::Open,
            json!({"branch": format!("feat/car-{n}"), "agent_run": builder.to_string()}),
        ),
        t(NOW),
        &[],
    )
    .await
    .unwrap();
    let mut md = json!({"hold": HOLD, "hold_sha": H1, "agent_run": run.to_string()});
    for (k, v) in mark.as_object().cloned().unwrap_or_default() {
        md[k] = v;
    }
    let mut step = Step::new(JobId::from_uuid(car), "task", boss_jobs::car::REVIEW, 0);
    step.id = StepId::new();
    step.spec_slug = Some("review".into());
    step.status = StepStatus::Active;
    step.metadata = md;
    jobs.add_step_at(&step, t(NOW), &[]).await.unwrap();
    jobs.create_job_at(
        &job(
            "agent-run",
            run,
            JobStatus::Closed,
            json!({"review": {
                "car": car.to_string(), "branch": format!("feat/car-{n}"),
                "reviewed_sha": H1, "verdict": "release", "at": "2026-10-18T11:00:00Z",
                "findings": "### VERDICT: release\nSAFE TO RELEASE WITHOUT DAVID: yes.",
            }}),
        ),
        t(NOW),
        &[],
    )
    .await
    .unwrap();
    let mut building = Step::new(JobId::from_uuid(run), "task", "Building", 0);
    building.spec_slug = Some("building".into());
    building.status = StepStatus::Active;
    jobs.add_step_at(&building, t(NOW), &[]).await.unwrap();
    let source_result = jobs.record_step_metadata_at(&building.id, boss_jobs::release_window::SOURCE_KEY,
        &json!({"car":car.to_string(),"head":H1,"base":H1,"main":H1,"path":boss_jobs::release_window::MANIFEST_PATH,"mode":"100644","base_mode":"100644","base_manifest":"kind: ConfigMap\nmetadata:\n  name: boss-machine-gate\ndata:\n  mode: report\n","manifest":"kind: ConfigMap\nmetadata:\n  name: boss-machine-gate\ndata:\n  mode: report\n","diff":""}),
        &boss_core::publisher::EventStamp::new("test",boss_core::actor::ActorId::automation("review"))).await.unwrap();
    let boss_jobs::first_record::FirstRecordResult::Recorded(source) = source_result else {
        panic!("fixture source was not recorded")
    };
    let mut source_run = jobs.get_job(&JobId::from_uuid(run)).await.unwrap().unwrap();
    source_run.metadata["review"]["source_binding"] =
        json!({"digest":source.digest,"event_id":source.event_id});
    jobs.update_job_at(&source_run, source_run.status, t(NOW), &[])
        .await
        .unwrap();
    (car, run)
}

/// The ask every caller sends for `held_car`'s car: the review, and the
/// hold it judged as it read the car — REQUIRED since review 792b871a
/// N1, so the door lifts that hold or nothing.
fn ask(run: Uuid) -> Value {
    json!({"review": run.to_string(), "hold": {"reason": HOLD, "sha": H1}})
}

async fn release(app: &Router, who: &str, car: Uuid, body: Value) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/jobs/{car}/release"))
                .header("x-boss-user", who)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The car's review-step metadata as the STORE holds it after the call.
async fn review_md(jobs: &InMemoryJobs, car: Uuid) -> Value {
    let steps = jobs.list_steps(&JobId::from_uuid(car)).await.unwrap();
    steps
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some("review"))
        .expect("the review step")
        .metadata
}

/// The actors on every event the store recorded for `car`'s steps.
fn step_event_actors(jobs: &InMemoryJobs, car: Uuid) -> Vec<String> {
    jobs.recorded_events()
        .iter()
        .filter(|e| e.payload["job_id"] == json!(car.to_string()))
        .filter_map(|e| e.payload["_actor"].as_str().map(str::to_string))
        .collect()
}

fn assert_still_held(md: &Value) {
    assert_eq!(md["hold"], json!(HOLD), "the hold stays on: {md}");
    assert!(
        md.get(boss_jobs::car::RELEASE).is_none(),
        "no release record: {md}"
    );
}

/// UNRESERVED, AS THE RULE: granted, and the record and the merge event
/// credit the RULE in the same spelling — `automation:rule:<name>` —
/// never a person, an agent, or the wire id `rule:<name>`.
#[tokio::test]
async fn the_rule_releases_an_unreserved_car_and_is_credited_as_the_rule() {
    let (app, jobs) = app();
    let (car, run) = held_car(&jobs, 1, json!({})).await;
    let before = step_event_actors(&jobs, car).len();
    let (status, text) = release(&app, &the_rule(), car, ask(run)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let md = review_md(&jobs, car).await;
    assert!(md.get("hold").is_none(), "the hold is lifted: {md}");
    let rec = &md[boss_jobs::car::RELEASE];
    let credited = format!("automation:rule:{RULE}");
    assert_eq!(rec["by"], json!(credited), "{md}");
    assert_eq!(rec["review"], json!(run.to_string()), "{md}");
    assert_eq!(rec["sha"], json!(H1), "{md}");
    let actors: Vec<String> = step_event_actors(&jobs, car)
        .into_iter()
        .skip(before)
        .collect();
    assert!(!actors.is_empty(), "the release wrote an event");
    assert!(
        actors.iter().all(|a| a == &credited),
        "the event credits the same actor the record does: {actors:?}"
    );
}

/// RESERVED, AS THE RULE: refused 409 in the judge's words, and nothing
/// is written — the hold stays, no release record, no event.
#[tokio::test]
async fn the_rule_is_refused_a_car_reserved_for_a_person() {
    let (app, jobs) = app();
    let (car, run) = held_car(&jobs, 2, json!({boss_jobs::car::RELEASER: "emp-david"})).await;
    let before = jobs.recorded_events().len();
    let (status, text) = release(&app, &the_rule(), car, ask(run)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(text.starts_with("boss release: REFUSED — "), "{text}");
    assert!(text.contains("reserved for emp-david"), "{text}");
    assert_still_held(&review_md(&jobs, car).await);
    assert_eq!(jobs.recorded_events().len(), before, "nothing was written");
}

#[tokio::test]
async fn every_other_actor_is_refused_a_reserved_car_without_writes() {
    for (n, actor) in [
        "agent-codex",
        "emp-another",
        "automation:train-conductor",
        "rule:x",
    ]
    .into_iter()
    .enumerate()
    {
        let (app, jobs) = app();
        let (car, run) = held_car(
            &jobs,
            100 + n as u128,
            json!({boss_jobs::car::RELEASER: "emp-david"}),
        )
        .await;
        let before = jobs.recorded_events().len();
        let steps_before = review_md(&jobs, car).await;
        let (status, text) = release(&app, &caller(actor), car, ask(run)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{actor}: {text}");
        assert!(text.contains("emp-david"), "{actor}: {text}");
        assert_eq!(
            review_md(&jobs, car).await,
            steps_before,
            "{actor}: no metadata write"
        );
        assert_eq!(
            jobs.recorded_events().len(),
            before,
            "{actor}: no event write"
        );
    }
}

#[tokio::test]
async fn a_registered_alias_releases_only_its_own_reservation_and_is_credited() {
    let agents = Arc::new(
        boss_jobs::agents::InMemoryAgents::new()
            .with_agent("agent-owner", ["owner@login.example"])
            .with_agent("agent-other", ["other@login.example"]),
    );
    let (app, jobs) = app_with_actors(Some(agents));
    let (car, run) = held_car(&jobs, 200, json!({boss_jobs::car::RELEASER:"agent-owner"})).await;
    let before = jobs.recorded_events().len();
    let md_before = review_md(&jobs, car).await;
    let (status, text) = release(&app, &caller("other@login.example"), car, ask(run)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(review_md(&jobs, car).await, md_before);
    assert_eq!(jobs.recorded_events().len(), before);
    let (status, text) = release(&app, &caller("owner@login.example"), car, ask(run)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        review_md(&jobs, car).await[boss_jobs::car::RELEASE]["by"],
        "owner@login.example"
    );
    let (app, jobs) = app_with_actors(None);
    let (car, run) = held_car(&jobs, 201, json!({boss_jobs::car::RELEASER:"agent-owner"})).await;
    let before = jobs.recorded_events().len();
    let md_before = review_md(&jobs, car).await;
    let (status, text) = release(&app, &caller("owner@login.example"), car, ask(run)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(text.contains("registry"), "{text}");
    assert_eq!(review_md(&jobs, car).await, md_before);
    assert_eq!(jobs.recorded_events().len(), before);
}

#[tokio::test]
async fn the_yard_judges_the_reserved_release_for_its_actual_viewer_without_writes() {
    let agents = Arc::new(
        boss_jobs::agents::InMemoryAgents::new().with_agent("agent-owner", ["owner@login.example"]),
    );
    let (app, jobs) = app_with_actors(Some(agents));
    let (car, _) = held_car(&jobs, 300, json!({boss_jobs::car::RELEASER:"agent-owner"})).await;
    let before = jobs.recorded_events().len();
    let md_before = review_md(&jobs, car).await;
    for (actor, expected) in [
        ("agent-owner", true),
        ("owner@login.example", true),
        ("agent-codex", false),
        ("automation:train-conductor", false),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/yard/status")
                    .header("x-boss-user", caller(actor))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let raw = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            status,
            StatusCode::OK,
            "{actor}: {}",
            String::from_utf8_lossy(&raw)
        );
        let body: Value = serde_json::from_slice(&raw).unwrap();
        let held = body["held_cars"].as_array().unwrap();
        let row = held
            .iter()
            .find(|r| r["branch"] == "feat/car-300")
            .unwrap_or_else(|| panic!("the actual held car in {body}"));
        assert_eq!(row["release"]["releasable"], expected, "{actor}: {row}");
    }
    assert_eq!(review_md(&jobs, car).await, md_before);
    assert_eq!(jobs.recorded_events().len(), before);
}

#[tokio::test]
async fn a_reservation_changed_after_judgement_refuses_the_write_and_current_owner_can_release() {
    for (n, mark) in [
        (400, json!({boss_jobs::car::RELEASER:"emp-david"})),
        (401, json!({})),
    ] {
        let race = Arc::new(std::sync::Mutex::new(None));
        let (app, jobs) = app_with_race(None, race.clone());
        let (car, run) = held_car(&jobs, n, mark).await;
        let step = jobs
            .list_steps(&JobId::from_uuid(car))
            .await
            .unwrap()
            .remove(0);
        *race.lock().unwrap() = Some(step.id);
        let before = jobs.recorded_events().len();
        let (status, text) = release(&app, &caller("emp-david"), car, ask(run)).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "the changed reservation must refuse: {text}"
        );
        assert!(text.contains("changed"), "{text}");
        let md = review_md(&jobs, car).await;
        assert_still_held(&md);
        assert_eq!(md[boss_jobs::car::RELEASER], "emp-another");
        assert_eq!(
            jobs.recorded_events().len(),
            before + 1,
            "only the controlled reservation change wrote an event"
        );
        let (status, text) = release(&app, &caller("emp-another"), car, ask(run)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the current named owner can release: {text}"
        );
        assert_eq!(
            review_md(&jobs, car).await[boss_jobs::car::RELEASE]["by"],
            "emp-another"
        );
    }
}

async fn bare_release(
    app: &Router,
    jobs: &InMemoryJobs,
    actor: &str,
    car: Uuid,
) -> (StatusCode, String) {
    metadata_patch(
        app,
        jobs,
        actor,
        car,
        json!({"hold":null,"hold_released":{"by":actor,"at":NOW,"hold":HOLD,"agent_shell":true}}),
    )
    .await
}

async fn metadata_patch(
    app: &Router,
    jobs: &InMemoryJobs,
    actor: &str,
    car: Uuid,
    body: Value,
) -> (StatusCode, String) {
    let step = jobs
        .list_steps(&JobId::from_uuid(car))
        .await
        .unwrap()
        .remove(0);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{car}/steps/{}/metadata", step.id))
                .header("x-boss-user", caller(actor))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let raw = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(raw.to_vec()).unwrap())
}

#[tokio::test]
async fn the_bare_metadata_door_cannot_bypass_the_named_releaser() {
    for actor in ["agent-codex", "automation:train-conductor"] {
        let (app, jobs) = app();
        let (car, _) = held_car(&jobs, 500, json!({boss_jobs::car::RELEASER:"emp-david"})).await;
        let before = jobs.recorded_events().len();
        let md_before = review_md(&jobs, car).await;
        let (status, text) = bare_release(&app, &jobs, actor, car).await;
        assert_eq!(status, StatusCode::CONFLICT, "{actor}: {text}");
        assert!(text.contains("emp-david"), "{text}");
        assert_eq!(review_md(&jobs, car).await, md_before);
        assert_eq!(jobs.recorded_events().len(), before);
    }
}

#[tokio::test]
async fn an_existing_reservation_cannot_be_deleted_or_repointed_before_either_release() {
    for patch in [
        json!({"releaser":null}),
        json!({"releaser":"agent-codex"}),
        json!({"hold":HOLD,"releaser":"agent-codex"}),
    ] {
        let (app, jobs) = app();
        let (car, run) = held_car(&jobs, 950, json!({"releaser":"emp-david"})).await;
        let before = review_md(&jobs, car).await;
        let events = jobs.recorded_events().len();
        let (status, text) = metadata_patch(&app, &jobs, "agent-codex", car, patch.clone()).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "reservation patch {patch}: {text}"
        );
        assert_eq!(review_md(&jobs, car).await, before);
        assert_eq!(jobs.recorded_events().len(), events);
        assert_eq!(
            release(&app, &caller("agent-codex"), car, ask(run)).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            bare_release(&app, &jobs, "agent-codex", car).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(review_md(&jobs, car).await, before);
        // Unrelated metadata and writing the identical reservation do not transfer authority.
        assert_eq!(
            metadata_patch(
                &app,
                &jobs,
                "agent-codex",
                car,
                json!({"releaser":"emp-david","review_note":"unchanged reservation"})
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
    }
}

#[tokio::test]
async fn an_agent_aliasing_a_person_cannot_capture_that_persons_reservation() {
    let actors =
        Arc::new(boss_jobs::agents::InMemoryAgents::new().with_agent("agent-codex", ["emp-david"]));
    let (app, jobs) = app_with_actors(Some(actors));
    let (car, run) = held_car(&jobs, 951, json!({"releaser":"emp-david"})).await;
    let before = review_md(&jobs, car).await;
    let events = jobs.recorded_events().len();
    let (status, text) = release(&app, &caller("agent-codex"), car, ask(run)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(
        bare_release(&app, &jobs, "agent-codex", car).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(review_md(&jobs, car).await, before);
    assert_eq!(jobs.recorded_events().len(), events);
}

#[tokio::test]
async fn an_existing_null_reservation_cannot_be_erased_as_an_identical_value() {
    let (app, jobs) = app();
    let (car, _) = held_car(&jobs, 955, json!({"releaser":null})).await;
    let before = review_md(&jobs, car).await;
    let events = jobs.recorded_events().len();
    let (status, text) =
        metadata_patch(&app, &jobs, "emp-david", car, json!({"releaser":null})).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "PATCH null deletes the existing malformed mark: {text}"
    );
    assert_eq!(review_md(&jobs, car).await, before);
    assert_eq!(jobs.recorded_events().len(), events);
}

type AliasRace = Arc<std::sync::Mutex<Option<(Arc<InMemoryJobs>, StepId)>>>;

struct LateReservationRoster {
    placement: AliasRace,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl boss_jobs::owner_resolution::RosterLookup for LateReservationRoster {
    async fn active_holders(&self, _role: &str) -> Result<Vec<String>, String> {
        Ok(vec![])
    }

    async fn is_active_employee(&self, _id: &str) -> Result<bool, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let target = self.placement.lock().unwrap().take();
        if let Some((jobs, step)) = target {
            jobs.merge_step_metadata_at(
                &step,
                json!({"releaser":"emp-competing"}).as_object().unwrap(),
                &boss_core::publisher::EventStamp::new(
                    "jobs",
                    boss_core::actor::ActorId::Automation("reservation-placement-race".into()),
                ),
            )
            .await
            .unwrap();
        }
        Ok(true)
    }
}

/// The human-only roster port is called after reservation judgment and
/// before the actual merge. It supplies a deterministic competing write,
/// not a sleep or an assumption about task scheduling.
#[tokio::test]
async fn a_reservation_patch_freezes_absence_and_identical_marks_until_its_write() {
    for (old, requested) in [
        (None, json!(null)),
        (None, json!("emp-caller")),
        (Some(json!("emp-initial")), json!("emp-initial")),
    ] {
        for competing in [false, true] {
            let placement = Arc::new(std::sync::Mutex::new(None));
            let roster = Arc::new(LateReservationRoster {
                placement: placement.clone(),
                calls: std::sync::atomic::AtomicUsize::new(0),
            });
            let (app, jobs) = app_with_race_and_roster(
                None,
                Arc::new(std::sync::Mutex::new(None)),
                Some(roster.clone()),
            );
            let mut mark = json!({"human_only":true});
            if let Some(value) = &old {
                mark["releaser"] = value.clone();
            }
            let (car, _) = held_car(&jobs, 954, mark).await;
            let step = jobs
                .list_steps(&JobId::from_uuid(car))
                .await
                .unwrap()
                .remove(0);
            if competing {
                *placement.lock().unwrap() = Some((jobs.clone(), step.id));
            }
            let before = jobs.recorded_events().len();
            let (status, text) = metadata_patch(
                &app,
                &jobs,
                "emp-caller",
                car,
                json!({"releaser":requested,"note":"ordinary metadata at the race boundary"}),
            )
            .await;
            assert!(
                roster.calls.load(std::sync::atomic::Ordering::SeqCst) > 0,
                "the actual post-judgment roster boundary must be reached"
            );
            assert!(
                placement.lock().unwrap().is_none(),
                "the competing placement happened"
            );
            let md = review_md(&jobs, car).await;
            assert_eq!(md["hold"], HOLD);
            if competing {
                assert_eq!(
                    status,
                    StatusCode::CONFLICT,
                    "old={old:?}, requested={requested}: {text}"
                );
                assert!(text.contains("step_changed"), "{text}");
                assert_eq!(md["releaser"], "emp-competing");
                assert!(md.get("note").is_none(), "no part of the raced patch lands");
            } else {
                assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
                assert_eq!(
                    md.get("releaser"),
                    if requested.is_null() {
                        None
                    } else {
                        Some(&requested)
                    }
                );
                assert!(
                    md.get("note").is_some(),
                    "unchanged marks must not overblock ordinary metadata"
                );
            }
            assert_eq!(
                jobs.recorded_events().len(),
                before + 1,
                "exactly the competing reservation or the uncontended requested patch writes"
            );
        }
    }
}

struct NativeAliases {
    inner: boss_jobs::agents::InMemoryAgents,
    race: AliasRace,
    unavailable: bool,
}

#[async_trait::async_trait]
impl boss_jobs::agents::AgentsRegistry for NativeAliases {
    async fn resolve_login(
        &self,
        login: &str,
    ) -> Result<Option<String>, boss_jobs::agents::AgentsError> {
        self.inner.resolve_login(login).await
    }
    async fn list(
        &self,
    ) -> Result<Vec<boss_jobs::agents::AgentRow>, boss_jobs::agents::AgentsError> {
        if self.unavailable {
            return Err(boss_jobs::agents::AgentsError::Storage(
                "native registry unavailable".into(),
            ));
        }
        let rows = self.inner.list().await?;
        let target = self.race.lock().unwrap().take();
        if let Some((jobs, step)) = target {
            jobs.merge_step_metadata_at(
                &step,
                json!({boss_jobs::car::RELEASER:"agent-other"})
                    .as_object()
                    .unwrap(),
                &boss_core::publisher::EventStamp::new(
                    "jobs",
                    boss_core::actor::ActorId::Automation("alias-read-race".into()),
                ),
            )
            .await
            .unwrap();
        }
        Ok(rows)
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
async fn a_bare_alias_release_is_fenced_against_a_reservation_change_after_judgment() {
    let race = Arc::new(std::sync::Mutex::new(None));
    let agents = Arc::new(NativeAliases {
        inner: boss_jobs::agents::InMemoryAgents::new()
            .with_agent("agent-owner", ["owner@login.example"]),
        race: race.clone(),
        unavailable: false,
    });
    let (app, jobs) = app_with_actors(Some(agents));
    let (car, _) = held_car(&jobs, 501, json!({boss_jobs::car::RELEASER:"agent-owner"})).await;
    let step = jobs
        .list_steps(&JobId::from_uuid(car))
        .await
        .unwrap()
        .remove(0);
    *race.lock().unwrap() = Some((jobs.clone(), step.id));
    let before = jobs.recorded_events().len();
    let (status, text) = bare_release(&app, &jobs, "owner@login.example", car).await;
    assert!(
        race.lock().unwrap().is_none(),
        "the controlled native alias snapshot boundary was reached"
    );
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(text.contains("step_changed"), "{text}");
    let md = review_md(&jobs, car).await;
    assert_still_held(&md);
    assert!(md.get("hold_released").is_none());
    assert_eq!(md[boss_jobs::car::RELEASER], "agent-other");
    assert_eq!(
        jobs.recorded_events().len(),
        before + 1,
        "only controlled reservation change wrote"
    );
    let (status, text) = bare_release(&app, &jobs, "agent-other", car).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    assert!(review_md(&jobs, car).await.get("hold").is_none());
}

#[tokio::test]
async fn transferring_a_reservation_is_fenced_against_a_change_during_alias_judgment() {
    let race = Arc::new(std::sync::Mutex::new(None));
    let actors = Arc::new(NativeAliases {
        inner: boss_jobs::agents::InMemoryAgents::new()
            .with_agent("agent-owner", ["owner@login.example"]),
        race: race.clone(),
        unavailable: false,
    });
    let (app, jobs) = app_with_actors(Some(actors));
    let (car, _) = held_car(&jobs, 952, json!({"releaser":"agent-owner"})).await;
    let step = jobs
        .list_steps(&JobId::from_uuid(car))
        .await
        .unwrap()
        .remove(0);
    *race.lock().unwrap() = Some((jobs.clone(), step.id));
    let before = jobs.recorded_events().len();
    let (status, text) = metadata_patch(
        &app,
        &jobs,
        "owner@login.example",
        car,
        json!({"releaser":"agent-next"}),
    )
    .await;
    assert!(
        race.lock().unwrap().is_none(),
        "reservation transfer must reach native alias judgment"
    );
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(text.contains("step_changed"), "{text}");
    assert_eq!(review_md(&jobs, car).await["releaser"], "agent-other");
    assert_eq!(
        jobs.recorded_events().len(),
        before + 1,
        "only controlled race writes"
    );
}

#[tokio::test]
async fn the_current_owner_can_transfer_or_delete_its_own_reservation() {
    for replacement in [json!(null), json!("agent-next")] {
        let (app, jobs) = app();
        let (car, _) = held_car(&jobs, 953, json!({"releaser":"emp-david"})).await;
        let (status, text) = metadata_patch(
            &app,
            &jobs,
            "emp-david",
            car,
            json!({"releaser":replacement}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
        assert_eq!(
            review_md(&jobs, car).await.get("releaser"),
            if replacement.is_null() {
                None
            } else {
                Some(&replacement)
            }
        );
        assert_eq!(review_md(&jobs, car).await["hold"], HOLD);
    }
}

#[tokio::test]
async fn bare_release_identity_and_registry_failures_preserve_the_same_contract() {
    for (actor, unavailable, allowed) in [
        ("agent-owner", true, true),
        ("owner@login.example", false, true),
        ("owner@login.example", true, false),
        ("other@login.example", false, false),
    ] {
        let agents = Arc::new(NativeAliases {
            inner: boss_jobs::agents::InMemoryAgents::new()
                .with_agent("agent-owner", ["owner@login.example"]),
            race: Arc::new(std::sync::Mutex::new(None)),
            unavailable,
        });
        let (app, jobs) = app_with_actors(Some(agents));
        let (car, _) = held_car(&jobs, 502, json!({boss_jobs::car::RELEASER:"agent-owner"})).await;
        let before = jobs.recorded_events().len();
        let md_before = review_md(&jobs, car).await;
        let (status, text) = bare_release(&app, &jobs, actor, car).await;
        if allowed {
            assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
        } else {
            assert_eq!(status, StatusCode::CONFLICT, "{text}");
            assert_eq!(review_md(&jobs, car).await, md_before);
            assert_eq!(jobs.recorded_events().len(), before);
        }
    }
    let (app, jobs) = app();
    let (car, _) = held_car(&jobs, 503, json!({})).await;
    assert_eq!(
        bare_release(&app, &jobs, "automation:train-conductor", car)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn the_bare_guard_rejects_malformed_marks_and_all_marker_clearing_shapes_only() {
    for mark in [json!(null), json!(""), json!([]), json!({"id":"emp-david"})] {
        for actor in ["agent-codex", "automation:train-conductor"] {
            for clear in [json!(null), json!(false), json!(" "), json!({})] {
                let (app, jobs) = app();
                let (car, _) =
                    held_car(&jobs, 504, json!({boss_jobs::car::RELEASER:mark.clone()})).await;
                let before = jobs.recorded_events().len();
                let md_before = review_md(&jobs, car).await;
                let (status, text) = metadata_patch(
                    &app,
                    &jobs,
                    actor,
                    car,
                    json!({"hold":clear,"hold_released":{"by":"emp-david"}}),
                )
                .await;
                assert_eq!(status, StatusCode::CONFLICT, "{actor}: {text}");
                assert_eq!(review_md(&jobs, car).await, md_before);
                assert_eq!(jobs.recorded_events().len(), before);
            }
        }
    }
    let (app, jobs) = app();
    let (car, _) = held_car(&jobs, 505, json!({boss_jobs::car::RELEASER:"emp-david"})).await;
    let (status, text) = metadata_patch(
        &app,
        &jobs,
        "agent-codex",
        car,
        json!({"note":"ordinary metadata"}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    let md = review_md(&jobs, car).await;
    assert_eq!(md["note"], "ordinary metadata");
    assert_eq!(md["hold"], HOLD);
}

/// A reservation in a shape that names no one (a hand-written blank)
/// fails CLOSED for the rule at the door too (review 3af8f7e2 N1).
#[tokio::test]
async fn a_reservation_that_names_no_one_still_refuses_the_rule() {
    for mark in [
        json!(null),
        json!(false),
        json!(true),
        json!(""),
        json!("  "),
        json!([]),
        json!({"id":"emp-david"}),
        json!(7),
    ] {
        for actor in [
            "emp-david",
            "agent-codex",
            "rule:x",
            "automation:train-conductor",
        ] {
            let (app, jobs) = app();
            let (car, run) =
                held_car(&jobs, 3, json!({boss_jobs::car::RELEASER: mark.clone()})).await;
            let before = jobs.recorded_events().len();
            let md_before = review_md(&jobs, car).await;
            let (status, text) = release(&app, &caller(actor), car, ask(run)).await;
            assert_eq!(status, StatusCode::CONFLICT, "{actor}, {mark}: {text}");
            assert!(text.contains("names no actor"), "{text}");
            assert_eq!(review_md(&jobs, car).await, md_before);
            assert_eq!(jobs.recorded_events().len(), before);
        }
    }
}

/// AS A PERSON, on both cars: granted, and credited to that person. The
/// reserved person's exact identity needs no registry lookup, and the
/// unreserved road keeps the same review contract.
#[tokio::test]
async fn a_person_releases_a_reserved_and_an_unreserved_car_as_themself() {
    let (app, jobs) = app();
    for (n, mark) in [
        (4, json!({})),
        (5, json!({boss_jobs::car::RELEASER: "emp-david"})),
    ] {
        let (car, run) = held_car(&jobs, n, mark.clone()).await;
        let (status, text) = release(&app, &caller("emp-david"), car, ask(run)).await;
        assert_eq!(status, StatusCode::OK, "{mark}: {text}");
        let md = review_md(&jobs, car).await;
        assert!(md.get("hold").is_none(), "{mark}: {md}");
        assert_eq!(
            md[boss_jobs::car::RELEASE]["by"],
            json!("emp-david"),
            "{mark}: {md}"
        );
    }
}

/// NO BODY NAMES ITS OWN `by`. The rule cannot pass as a person by
/// asking to be recorded as one: the body is refused before anything is
/// read, and the reserved car stays held.
#[tokio::test]
async fn a_body_naming_its_own_releaser_is_refused() {
    let (app, jobs) = app();
    let (car, run) = held_car(&jobs, 6, json!({boss_jobs::car::RELEASER: "emp-david"})).await;
    let (status, text) = release(
        &app,
        &the_rule(),
        car,
        json!({"review": run.to_string(), "hold": {"reason": HOLD, "sha": H1}, "by": "emp-david"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
    assert_still_held(&review_md(&jobs, car).await);
}
