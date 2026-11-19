//! Layer 4: a `backlog-item`'s `build` cannot be completed by hand while
//! the car that answers it is landed and awaiting its proof.
//!
//! The defect (backlog 81dc0c39, filed 2026-11-06). The item closes when
//! its car closes `merged` — the arrival rule
//! `complete-feedback-branch-on-car-merged` completes `build` — and a
//! car's `merged` terminal waits on `proven`. Nothing on the item said
//! so, so a merged-but-unproven item read as forgotten and was completed
//! by hand: items 2682c418 and bed07fe8 closed as delivered while their
//! cars (a0c7fc14, d37e17b7) stood at `proven` READY, their probes never
//! run, and the proof link was lost.
//!
//! THE MEASUREMENT THAT SCOPED IT (2026-11-10, car 2dd4b8fd's
//! `jobs.job.closed`): the event carried `outcome: merged`, so the emit
//! site is sound and the rule fired as designed; it fired at 12:06:32,
//! 51 minutes after item 21217d8a's build was completed by hand. The
//! missing half is the item side refusing a hand completion ahead of
//! the car, and it is judged by the shed's own predicate
//! (`regions::awaiting_proof`) — no second notion of "unproven".
//!
//! THE FIX IS DATA PLUS ONE CONTRACT. The `build` row declares
//! `completion_contract = "item-awaits-its-car-v1"` and the jobs API
//! judges it on every completion door. These tests drive the REAL
//! router against the REAL platform bundle, for the reason
//! `backlog_item_build_can_refute` gives: a fixture copy of the spec
//! would keep passing while the shipped kind stayed broken.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

struct AdminRoster;

#[async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-bootstrap-admin",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// The router, and the repository behind it — the cars are written
/// straight into the repository, as the gate's auto-park would leave
/// them, because what is under test is the ITEM's door.
fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
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
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
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

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn read(app: &axum::Router, job_id: &str) -> serde_json::Value {
    let (status, body) = send(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{job_id}"))
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read failed: {body}");
    body
}

fn step_of<'a>(job: &'a serde_json::Value, slug: &str) -> &'a serde_json::Value {
    job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("no step `{slug}` on the packet: {job:#?}"))
}

/// Complete a step: `extra`'s keys through the merge door, then the
/// status alone through the PUT (backlog e39a9d2a).
async fn try_complete(
    app: &axum::Router,
    job_id: &str,
    job: &serde_json::Value,
    slug: &str,
    extra: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let step_id = step_of(job, slug)["id"].as_str().expect("step id");
    if extra.as_object().is_some_and(|keys| !keys.is_empty()) {
        let (status, body) = send(
            app,
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{job_id}/steps/{step_id}/metadata"))
                .header("content-type", "application/json")
                .header("x-boss-user", admin_header())
                .body(Body::from(extra.to_string()))
                .unwrap(),
        )
        .await;
        if !status.is_success() {
            return (status, body);
        }
    }
    send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{step_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed" }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

/// A fresh item routed to `build`, returned as read after triage.
async fn routed_to_build(app: &axum::Router) -> (String, serde_json::Value) {
    let body = serde_json::json!({
        "kind": "backlog-item",
        "subject": { "subject_kind": "custom", "id": "bosspipeline" },
        "title": "An item whose car is landed and unproven",
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard",
        "status": "open",
        "metadata": { "area": "jobs" },
        "tags": ["protocol"],
    })
    .to_string();
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    let id = job["id"].as_str().expect("job id").to_string();
    let job = read(app, &id).await;
    let (status, body) = try_complete(
        app,
        &id,
        &job,
        "triage",
        serde_json::json!({
            "disposition": "build",
            "evidence": "The claim was measured against origin/main and held.",
        }),
    )
    .await;
    assert!(status.is_success(), "triage failed {status}: {body}");
    let after = read(app, &id).await;
    assert_eq!(step_of(&after, "build")["status"], "ready");
    (id, after)
}

const BRANCH: &str = "fix/the-car-that-answers-the-item";

/// A landed car as the yard holds it: `review` done (merged), then
/// `proven`, `landed-review` and `merged`, with `proven` in `proven`'s
/// status. `link` is the metadata naming the item.
async fn park_car(
    jobs: &InMemoryJobs,
    link: serde_json::Value,
    car_status: JobStatus,
    proven: StepStatus,
) -> Job {
    let mut car = Job::new(
        "ship-a-change",
        Subject::new("custom", BRANCH),
        format!("Backlog item: {BRANCH}"),
        "emp-bootstrap-admin",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 11, 10).expect("a date"),
    );
    car.status = car_status;
    let mut md = serde_json::json!({
        "branch": BRANCH,
        "merged": "true",
        "proof_probe": "git show HEAD:README.md | grep -q BOSS && echo proven",
    });
    for (k, v) in link.as_object().expect("link is an object") {
        md[k] = v.clone();
    }
    car.metadata = md;
    jobs.create_job(&car).await.expect("create the car");
    let landed_review = if proven == StepStatus::Completed {
        StepStatus::Ready
    } else {
        StepStatus::Pending
    };
    for (order, (slug, status)) in [
        ("review", StepStatus::Completed),
        ("proven", proven),
        ("landed-review", landed_review),
        ("merged", StepStatus::Pending),
    ]
    .into_iter()
    .enumerate()
    {
        let mut step = Step::new(car.id, "task", slug, order as i32);
        step.spec_slug = Some(slug.to_string());
        step.status = status;
        jobs.add_step(&step).await.expect("add a car step");
    }
    car
}

/// THE DEFECT. A hand completion of `build` while the item's car stands
/// at `proven` is refused, naming the car and the proof it awaits — and
/// the item's build is left exactly where it was.
#[tokio::test]
async fn a_hand_completed_build_is_refused_while_its_car_awaits_proof() {
    let (app, jobs) = app();
    let (item, after) = routed_to_build(&app).await;
    let car = park_car(
        &jobs,
        serde_json::json!({ "backlog_item": item }),
        JobStatus::Open,
        StepStatus::Ready,
    )
    .await;

    let (status, body) = try_complete(&app, &item, &after, "build", serde_json::json!({})).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a build ahead of its car's proof must be refused: {body}"
    );
    let said = body.to_string();
    let short = &car.id.to_string()[..8];
    for phrase in [
        short,
        BRANCH,
        "awaits its proof",
        "a probe is recorded and has not run yet",
        "complete-feedback-branch-on-car-merged",
    ] {
        assert!(
            said.contains(phrase),
            "the refusal names `{phrase}`: {said}"
        );
    }
    let still = read(&app, &item).await;
    assert_eq!(
        step_of(&still, "build")["status"],
        "ready",
        "nothing was written"
    );
    assert_eq!(still["status"], "open");
}

/// A car that answers the item through `also_answers` holds it the same
/// way: the arrival rule closes every item it lists (backlog a994f533).
#[tokio::test]
async fn an_item_a_car_also_answers_waits_for_the_same_proof() {
    let (app, jobs) = app();
    let (item, after) = routed_to_build(&app).await;
    park_car(
        &jobs,
        serde_json::json!({ "also_answers": [item] }),
        JobStatus::Open,
        StepStatus::Ready,
    )
    .await;

    let (status, body) = try_complete(&app, &item, &after, "build", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.to_string().contains("awaits its proof"), "{body}");
}

/// A PROVEN car's item completes: the proof link is already on the
/// record, so a hand completion loses nothing. This is car 2dd4b8fd's
/// shape at 11:15 on 2026-11-06 — proven, waiting on its landed review.
#[tokio::test]
async fn a_proven_cars_item_completes() {
    let (app, jobs) = app();
    let (item, after) = routed_to_build(&app).await;
    park_car(
        &jobs,
        serde_json::json!({ "backlog_item": item }),
        JobStatus::Open,
        StepStatus::Completed,
    )
    .await;

    let (status, body) = try_complete(&app, &item, &after, "build", serde_json::json!({})).await;
    assert!(status.is_success(), "a proven car frees the build: {body}");
    assert_eq!(
        step_of(&read(&app, &item).await, "build")["status"],
        "completed"
    );
}

/// The arrival rule's own completion runs AFTER the car closed, and a
/// closed car stands in no shed — so the rule is never refused by this.
#[tokio::test]
async fn a_closed_car_does_not_hold_the_build() {
    let (app, jobs) = app();
    let (item, after) = routed_to_build(&app).await;
    park_car(
        &jobs,
        serde_json::json!({ "backlog_item": item }),
        JobStatus::Closed,
        StepStatus::Completed,
    )
    .await;

    let (status, body) = try_complete(&app, &item, &after, "build", serde_json::json!({})).await;
    assert!(status.is_success(), "{body}");
}

/// The row declares the contract by the name the API judges it under —
/// one constant, so the two cannot drift (CLAUDE.md §9a).
#[test]
fn the_build_row_declares_the_contract_the_api_judges() {
    let wf = seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "backlog-item")
        .expect("backlog-item ships in the platform bundle");
    let build = wf
        .steps
        .iter()
        .find(|s| s.title == "build")
        .expect("backlog-item has a build step");
    assert_eq!(
        build.metadata_defaults.get("completion_contract"),
        Some(&serde_json::json!(boss_jobs::item_awaits_its_car::NAME)),
    );
}
