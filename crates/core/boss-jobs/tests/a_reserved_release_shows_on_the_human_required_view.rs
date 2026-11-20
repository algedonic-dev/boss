//! A reserved release that the door would grant shows on the
//! human-required view, as a line on the car's own review step (backlog
//! 8ad93d25, design 58c72d95).
//!
//! WHY THIS EXISTS. `boss hold --releaser emp-david` reserves a car's
//! release for one person, and the release door refuses everyone else.
//! But the reservation is metadata on a review step assigned to the
//! reviewer, which declares no `human_only`, so the human-required view
//! never named it: the machine-token car 10f1b77a sat admissible for 19 h
//! on 2026-10-25, and on 2026-11-20 the drain grant 06bca3f0 sat RELEASE-
//! reviewed for seven hours, with nothing on David's boards saying it was
//! his (David, 2026-11-20 ~15:35Z: "I don't see the jobs as being visible
//! when I can act on them").
//!
//! THE RULE HELD HERE (design 58c72d95 D1-D4, D7). The line is DERIVED on
//! read, never written: it exists exactly while
//! `car_release::decision_for`, asked as the reserved actor, grants the
//! release — the same judge the yard row and `POST /release` run, so the
//! board cannot offer what the door refuses. It is the review step itself
//! (its own id and status) marked `act: release` and `reserved_for`, and
//! it is the same whoever reads the view. It is absent for a CHANGES
//! verdict, a review of another head, a car already released through the
//! door, a car reserved for no one, and a completed review step; and the
//! envelope's `total` keeps its paging meaning.
//!
//! THE SHAPE TESTED is the live one, copied from
//! the_release_door_credits_its_caller_and_keeps_a_reserved_car_http.rs:
//! a ship-a-change car held at H1 whose review step names its dispatched
//! reviewer, and that reviewer's run recording its verdict and the
//! immutable review source the door reads.

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

const NOW: &str = "2026-11-20T12:00:00Z";
const H1: &str = "1111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222";
const HOLD: &str = "drain grant: David releases it himself";
const MANIFEST: &str =
    "kind: ConfigMap\nmetadata:\n  name: boss-machine-gate\ndata:\n  mode: report\n";

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

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
    // The same grants for every caller: a difference below is the
    // judge's, never the policy's.
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
            NaiveDate::from_ymd_opt(2026, 11, 20).unwrap(),
        )
    }
}

/// One held car and its reviewer's run, as the record carries them.
struct Car {
    /// Merged onto the review step: the reservation, or nothing.
    mark: Value,
    verdict: &'static str,
    reviewed_sha: &'static str,
    review_status: StepStatus,
}

impl Car {
    fn reserved() -> Self {
        Self {
            mark: json!({boss_jobs::car::RELEASER: "emp-david"}),
            verdict: "release",
            reviewed_sha: H1,
            review_status: StepStatus::Active,
        }
    }
}

/// Seeds car `n` held at H1 and its dispatched reviewer's run. Returns
/// (car id, run id, review step id).
async fn held_car(jobs: &InMemoryJobs, n: u128, c: Car) -> (Uuid, Uuid, StepId) {
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
    for (k, v) in c.mark.as_object().cloned().unwrap_or_default() {
        md[k] = v;
    }
    let mut step = Step::new(JobId::from_uuid(car), "task", boss_jobs::car::REVIEW, 0);
    step.id = StepId::new();
    step.spec_slug = Some("review".into());
    step.status = c.review_status;
    step.metadata = md;
    jobs.add_step_at(&step, t(NOW), &[]).await.unwrap();
    jobs.create_job_at(
        &job(
            "agent-run",
            run,
            JobStatus::Closed,
            json!({"review": {
                "car": car.to_string(), "branch": format!("feat/car-{n}"),
                "reviewed_sha": c.reviewed_sha, "verdict": c.verdict,
                "at": "2026-11-20T11:00:00Z",
                "findings": "### VERDICT: release\nSAFE TO RELEASE WITHOUT DAVID: no.",
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
    let recorded = jobs
        .record_step_metadata_at(
            &building.id,
            boss_jobs::release_window::SOURCE_KEY,
            &json!({"car": car.to_string(), "head": c.reviewed_sha, "base": c.reviewed_sha,
                    "main": c.reviewed_sha, "path": boss_jobs::release_window::MANIFEST_PATH,
                    "mode": "100644", "base_mode": "100644", "base_manifest": MANIFEST,
                    "manifest": MANIFEST, "diff": ""}),
            &boss_core::publisher::EventStamp::new(
                "test",
                boss_core::actor::ActorId::automation("review"),
            ),
        )
        .await
        .unwrap();
    let boss_jobs::first_record::FirstRecordResult::Recorded(source) = recorded else {
        panic!("fixture source was not recorded")
    };
    let mut source_run = jobs.get_job(&JobId::from_uuid(run)).await.unwrap().unwrap();
    source_run.metadata["review"]["source_binding"] =
        json!({"digest": source.digest, "event_id": source.event_id});
    jobs.update_job_at(&source_run, source_run.status, t(NOW), &[])
        .await
        .unwrap();
    (car, run, step.id)
}

async fn call(app: &Router, who: &str, method: &str, uri: &str, body: Option<Value>) -> Value {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-boss-user", caller(who));
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let resp = app
        .clone()
        .oneshot(
            req.body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert_eq!(status, StatusCode::OK, "{method} {uri} as {who}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

/// The view's row for `car`.
fn row(view: &Value, car: Uuid) -> &Value {
    view["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(car.to_string()))
        .unwrap_or_else(|| panic!("car {car} is on the view: {view}"))
}

#[tokio::test]
async fn a_reserved_release_shows_on_the_human_required_view() {
    let (app, jobs) = app();
    let (shown, _, review) = held_car(&jobs, 1, Car::reserved()).await;
    let (changes, _, _) = held_car(
        &jobs,
        2,
        Car {
            verdict: "changes",
            ..Car::reserved()
        },
    )
    .await;
    let (other_head, _, _) = held_car(
        &jobs,
        3,
        Car {
            reviewed_sha: H2,
            ..Car::reserved()
        },
    )
    .await;
    let (released, released_run, _) = held_car(&jobs, 4, Car::reserved()).await;
    let (unreserved, _, _) = held_car(
        &jobs,
        5,
        Car {
            mark: json!({}),
            ..Car::reserved()
        },
    )
    .await;
    let (completed, _, _) = held_car(
        &jobs,
        6,
        Car {
            review_status: StepStatus::Completed,
            ..Car::reserved()
        },
    )
    .await;

    // Released through the ONE door, by the person it was reserved for:
    // the line leaves on the next read, with no step to withdraw.
    call(
        &app,
        "emp-david",
        "POST",
        &format!("/api/jobs/{released}/release"),
        Some(json!({"review": released_run.to_string(), "hold": {"reason": HOLD, "sha": H1}})),
    )
    .await;

    let view_uri = "/api/jobs?status=open&human_required_view=true&limit=50";
    let as_agent = call(&app, "agent-codex", "GET", view_uri, None).await;
    let as_david = call(&app, "emp-david", "GET", view_uri, None).await;
    assert_eq!(
        as_agent, as_david,
        "the view is the same whoever reads it (D3)"
    );

    let lines = row(&as_agent, shown)["steps"].as_array().unwrap().clone();
    assert_eq!(
        lines,
        vec![json!({
            "id": review.to_string(),
            "kind": "release",
            "title": "Release at 11111111, reserved for emp-david",
            "status": "active",
            "human_required": true,
            "act": "release",
            "reserved_for": "emp-david",
        })],
        "the admissible reserved release is the review step, marked with its act (D4)"
    );
    for (why, car) in [
        ("a CHANGES verdict", changes),
        ("a review of another head", other_head),
        ("a car already released", released),
        ("a car reserved for no one", unreserved),
        ("a completed review step", completed),
    ] {
        assert_eq!(
            row(&as_agent, car)["steps"],
            json!([]),
            "{why} puts no line on the view"
        );
    }

    let plain = call(
        &app,
        "agent-codex",
        "GET",
        "/api/jobs?status=open&limit=50",
        None,
    )
    .await;
    assert_eq!(
        as_agent["total"], plain["total"],
        "the line leaves total as it was"
    );
    assert_eq!(as_agent["total"], 6, "every open car stays in the window");
}
