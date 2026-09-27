//! A car whose proof waits on a named actor's act puts that act on the
//! actor's queue, and takes it off everyone else's (backlog fb286c15).
//!
//! Measured 2026-09-26 on the live system of record: cars 4b05fe3e and
//! b94cb42f stood at a ready `Proven in prod` step with
//! `waits_on.owner = "emp-david"`, and `/api/jobs/assignments` for
//! emp-david returned neither while agent-claude's queue carried one —
//! the act David owns sat on the agent's My Day, and David learned of
//! it only by reading the shed. The read routes by
//! `boss_jobs::car::owned_wait`, the same reader the shed and
//! `boss orient` use, so all three agree on whose move a car is.

use boss_policy_client::types::{AccessTier, User};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn user_header() -> String {
    serde_json::to_string(&User {
        id: "emp-ceo".to_string(),
        role: "ceo".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

/// An open car at a ready `proven` step held by `holder`.
async fn car_at_proven(repo: &InMemoryJobs, title: &str, waits_on: Value, holder: &str) -> Job {
    let mut car = Job::new(
        "ship-a-change",
        Subject::new("custom", "bosspipeline"),
        title,
        "emp-ceo",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 9, 24).expect("date"),
    );
    car.status = JobStatus::Open;
    car.metadata = json!({ "branch": "feat/x", "waits_on": waits_on });
    repo.create_job(&car).await.expect("car");
    let mut proven = Step::new(car.id, "task", "Proven in prod", 5).with_assignee(holder);
    proven.spec_slug = Some("proven".into());
    proven.status = StepStatus::Ready;
    repo.add_step(&proven).await.expect("step");
    car
}

fn app(repo: Arc<InMemoryJobs>) -> axum::Router {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    router(JobsApiState::minimal(
        repo,
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    ))
}

async fn queue(app: &axum::Router, who: &str) -> Vec<Value> {
    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/api/jobs/assignments?assignee_id={who}"))
                .header("x-boss-user", user_header())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        v["total"].as_u64(),
        v["data"].as_array().map(|a| a.len() as u64),
        "the total counts the rows handed back"
    );
    v["data"].as_array().cloned().unwrap_or_default()
}

fn job_ids(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| r["job_id"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn the_owner_is_handed_the_act_with_its_words_and_the_car() {
    let repo = Arc::new(InMemoryJobs::new());
    let car = car_at_proven(
        &repo,
        "boss tenant publish leaves an audit trail",
        json!({ "on": "a tenant publish run (David runs it)", "owner": "emp-david" }),
        "agent-claude",
    )
    .await;
    let app = app(repo);

    let david = queue(&app, "emp-david").await;
    assert_eq!(job_ids(&david), vec![car.id.to_string()], "{david:#?}");
    let row = &david[0];
    assert_eq!(row["step"]["spec_slug"], "proven");
    assert_eq!(row["owned_wait"]["owner"], "emp-david");
    assert_eq!(
        row["owned_wait"]["on"],
        "a tenant publish run (David runs it)"
    );
    assert_eq!(row["owned_wait"]["car"], car.id.to_string());
}

#[tokio::test]
async fn the_act_leaves_the_queue_of_the_agent_holding_the_step() {
    let repo = Arc::new(InMemoryJobs::new());
    let car = car_at_proven(
        &repo,
        "a bounded tag-release forge verb",
        json!({ "on": "a cut-a-release packet (David opens it)", "owner": "emp-david" }),
        "agent-claude",
    )
    .await;
    // A car with no owned wait stays exactly where it is held — the
    // control that the agent's queue is read at all.
    let ours = car_at_proven(&repo, "an unowned car", json!(null), "agent-claude").await;
    let app = app(repo);

    let agent = queue(&app, "agent-claude").await;
    let ids = job_ids(&agent);
    assert!(!ids.contains(&car.id.to_string()), "{agent:#?}");
    assert_eq!(ids, vec![ours.id.to_string()]);
    assert!(agent[0].get("owned_wait").is_none(), "{agent:#?}");
}

#[tokio::test]
async fn a_wait_the_world_owns_goes_to_nobody() {
    let repo = Arc::new(InMemoryJobs::new());
    let car = car_at_proven(
        &repo,
        "a car waiting on a Stripe charge",
        json!({ "on": "a Stripe charge", "owner": "world", "seen": "true" }),
        "agent-claude",
    )
    .await;
    let app = app(repo);

    for nobody in ["world", "the%20world", "emp-david"] {
        let rows = queue(&app, nobody).await;
        assert!(
            rows.is_empty(),
            "{nobody} was handed a world wait: {rows:#?}"
        );
    }
    // Nobody's act is not re-routed: the step stays with its holder,
    // with no owned-wait words on it.
    let agent = queue(&app, "agent-claude").await;
    assert_eq!(job_ids(&agent), vec![car.id.to_string()]);
    assert!(agent[0].get("owned_wait").is_none(), "{agent:#?}");
}
