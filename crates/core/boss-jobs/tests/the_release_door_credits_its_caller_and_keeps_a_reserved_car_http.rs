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

const NOW: &str = "2026-10-18T12:00:00Z";
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
    let jobs = Arc::new(InMemoryJobs::new());
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
        kind_registry: Some(kinds),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
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

/// A reservation in a shape that names no one (a hand-written blank)
/// fails CLOSED for the rule at the door too (review 3af8f7e2 N1).
#[tokio::test]
async fn a_reservation_that_names_no_one_still_refuses_the_rule() {
    let (app, jobs) = app();
    let (car, run) = held_car(&jobs, 3, json!({boss_jobs::car::RELEASER: ""})).await;
    let (status, text) = release(&app, &the_rule(), car, ask(run)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(text.contains("names no actor"), "{text}");
    assert_still_held(&review_md(&jobs, car).await);
}

/// AS A PERSON, on both cars: granted, and credited to that person. The
/// reservation binds automations only; a person's release is the road
/// "as today", resting on the same review.
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
