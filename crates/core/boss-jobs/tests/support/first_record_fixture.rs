use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use boss_core::{
    job::{Job, Priority, Step, StepStatus, Subject},
    port::EventBus,
    publisher::DomainPublisher,
};
use boss_jobs::{
    InMemoryJobs, JobsRepository,
    http::{JobsApiState, router},
};
use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
pub async fn build_app(allowed: bool) -> (Router, Arc<InMemoryJobs>, Job, Step) {
    let mut policy = FakePolicyClient::builder();
    if allowed {
        policy = policy.allow("service-tech", Action::Update, Resource::step(), Scope::All);
    }
    build_app_with_policy(Arc::new(policy.build())).await
}

pub async fn build_app_with_policy(
    policy: Arc<dyn boss_policy_client::PolicyClient>,
) -> (Router, Arc<InMemoryJobs>, Job, Step) {
    build_app_with_roster(policy, None).await
}

pub async fn build_app_with_roster(
    policy: Arc<dyn boss_policy_client::PolicyClient>,
    roster: Option<Arc<dyn boss_jobs::owner_resolution::RosterLookup>>,
) -> (Router, Arc<InMemoryJobs>, Job, Step) {
    let jobs = Arc::new(InMemoryJobs::new());
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "record-control"),
        "Record evidence",
        "emp-1",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
    );
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Record", 0);
    step.status = StepStatus::Active;
    jobs.add_step(&step).await.unwrap();
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        roster,
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs, job, step)
}
pub async fn post(app: &Router, job: &Job, step: &Step, body: Value) -> (StatusCode, Value) {
    post_as(app, job, step, body, "emp-1").await
}

pub async fn post_as(
    app: &Router,
    job: &Job,
    step: &Step,
    body: Value,
    actor: &str,
) -> (StatusCode, Value) {
    let user = json!({"id":actor,"role":"service-tech","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"service"});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/jobs/{}/steps/{}/metadata/records",
                    job.id, step.id
                ))
                .header("content-type", "application/json")
                .header("x-boss-user", user.to_string())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

pub async fn assert_refusal_is_unchanged(
    sink: Arc<dyn boss_core::request_latency::Recorder>,
    now: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
) {
    use boss_core::request_latency::{Timing, mount};
    let (app, jobs, job, step) = build_app(true).await;
    let before_step = serde_json::to_value(jobs.get_step(&step.id).await.unwrap()).unwrap();
    let before_job = serde_json::to_value(jobs.get_job(&job.id).await.unwrap()).unwrap();
    let before_events = jobs.recorded_events();
    let (timing, worker) = Timing::start(
        sink,
        "jobs".into(),
        "fixture".into(),
        now,
        boss_jobs::sensors::request_latency::classify,
    );
    let app = mount(app, timing);
    let response = post(
        &app,
        &job,
        &step,
        json!({"key":"evidence","value":1,"expected_absence":false}),
    )
    .await;
    assert_eq!(response.0, StatusCode::UNPROCESSABLE_ENTITY);
    worker.shutdown().await;
    assert_eq!(
        serde_json::to_value(jobs.get_step(&step.id).await.unwrap()).unwrap(),
        before_step
    );
    assert_eq!(
        serde_json::to_value(jobs.get_job(&job.id).await.unwrap()).unwrap(),
        before_job
    );
    assert_eq!(
        serde_json::to_value(jobs.recorded_events()).unwrap(),
        serde_json::to_value(before_events).unwrap()
    );
    assert!(
        jobs.step_write_refusals(10).await.unwrap().is_empty(),
        "NoPersistedRefusal still suppresses its original row"
    );
}
