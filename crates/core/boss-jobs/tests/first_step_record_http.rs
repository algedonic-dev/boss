use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use boss_core::{
    job::{Job, Priority, Step, StepField, StepStatus, Subject},
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

async fn build_app(allowed: bool) -> (Router, Arc<InMemoryJobs>, Job, Step) {
    let mut policy = FakePolicyClient::builder();
    if allowed {
        policy = policy.allow("service-tech", Action::Update, Resource::step(), Scope::All);
    }
    build_app_with_policy(Arc::new(policy.build())).await
}

async fn build_app_with_policy(
    policy: Arc<dyn boss_policy_client::PolicyClient>,
) -> (Router, Arc<InMemoryJobs>, Job, Step) {
    build_app_with_roster(policy, None).await
}

async fn build_app_with_roster(
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

struct ActiveEmployees(Vec<&'static str>);

#[async_trait::async_trait]
impl boss_jobs::owner_resolution::RosterLookup for ActiveEmployees {
    async fn active_holders(&self, _role: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(self.0.contains(&id))
    }
}

#[tokio::test]
async fn first_record_human_admission_uses_active_roster_and_preserves_context_carveout() {
    let policy = FakePolicyClient::builder()
        .allow("service-tech", Action::Update, Resource::step(), Scope::All)
        .build();
    let (app, jobs, job, mut step) = build_app_with_roster(
        Arc::new(policy),
        Some(Arc::new(ActiveEmployees(vec!["emp-1"]))),
    )
    .await;
    step.metadata = json!({"human_only":true});
    jobs.update_step(&step).await.unwrap();
    let answer = json!({"key":"answer","value":"approved","expected_absence":true});
    for actor in ["agent-codex", "inactive@example.test"] {
        assert_eq!(
            post_as(&app, &job, &step, answer.clone(), actor).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert!(jobs.recorded_events().is_empty());
    assert_eq!(
        post_as(
            &app,
            &job,
            &step,
            json!({"key":"context_md","value":"evidence for the person","expected_absence":true}),
            "agent-codex"
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (status, receipt) = post_as(&app, &job, &step, answer, "emp-1").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(receipt["record"]["actor"], "emp-1");
}

#[tokio::test]
async fn first_record_dispatcher_rule_header_keeps_canonical_rule_provenance() {
    let policy = FakePolicyClient::builder().with_default_rules().build();
    let (app, jobs, job, mut step) = build_app_with_policy(Arc::new(policy)).await;
    step.fields = vec![StepField::new("evidence", "string")];
    step.metadata = json!({"written_by":"rule:record-first-observation"});
    jobs.update_step(&step).await.unwrap();
    // The existing dispatcher rules::actor header shape; policy comes from its native default registry.
    let user = json!({"id":"rule:record-first-observation","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"});
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/jobs/{}/steps/{}/metadata/records",
                    job.id, step.id
                ))
                .header("content-type", "application/json")
                .header("x-boss-user", user.to_string())
                .body(Body::from(
                    json!({"key":"evidence","value":"observed","expected_absence":true})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let receipt: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        receipt["record"]["actor"],
        "automation:rule:record-first-observation"
    );
    assert!(
        jobs.recorded_events()
            .iter()
            .all(|event| event.payload["_actor"] == "automation:rule:record-first-observation")
    );
}

struct UnavailablePolicy;

#[async_trait::async_trait]
impl boss_policy_client::PolicyClient for UnavailablePolicy {
    async fn check(
        &self,
        _user: &boss_policy_client::User,
        _action: Action,
        _resource: Resource,
    ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
        Err(boss_policy_client::PolicyClientError::Unreachable(
            "injected outage".into(),
        ))
    }
    async fn scope_predicate(
        &self,
        _user: &boss_policy_client::User,
        _resource: Resource,
    ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
        Err(boss_policy_client::PolicyClientError::Unreachable(
            "injected outage".into(),
        ))
    }
}

#[tokio::test]
async fn first_record_policy_outage_reads_nothing_and_narrowed_scope_writes_nothing() {
    let (app, jobs, job, step) = build_app_with_policy(Arc::new(UnavailablePolicy)).await;
    jobs.merge_after_next_read(
        &step.id,
        json!({"after_read":true}).as_object().unwrap().clone(),
    );
    assert_eq!(
        post(
            &app,
            &job,
            &step,
            json!({"key":"evidence","value":1,"expected_absence":true})
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(
        jobs.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .get("after_read")
            .is_none()
    );
    assert!(jobs.recorded_events().is_empty());
    let policy = FakePolicyClient::builder()
        .allow(
            "service-tech",
            Action::Update,
            Resource::step(),
            Scope::Territory,
        )
        .build();
    let (app, jobs, job, step) = build_app_with_policy(Arc::new(policy)).await;
    assert_eq!(
        post(
            &app,
            &job,
            &step,
            json!({"key":"evidence","value":1,"expected_absence":true})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert!(jobs.recorded_events().is_empty());
    assert!(
        jobs.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .get("evidence")
            .is_none()
    );
}

async fn post(app: &Router, job: &Job, step: &Step, body: Value) -> (StatusCode, Value) {
    post_as(app, job, step, body, "emp-1").await
}

async fn post_as(
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

#[tokio::test]
async fn first_record_preserves_declared_field_shape_and_machine_writer_boundaries() {
    let (app, jobs, job, mut step) = build_app(true).await;
    step.fields = vec![StepField::new("evidence", "string")];
    step.metadata = json!({"written_by":"automation:record-worker"});
    jobs.update_step(&step).await.unwrap();
    let body = json!({"key":"evidence","value":"verified","expected_absence":true});
    assert_eq!(
        post_as(&app, &job, &step, body.clone(), "agent-codex")
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    for value in [Value::Null, json!(42)] {
        assert_eq!(
            post_as(
                &app,
                &job,
                &step,
                json!({"key":"evidence","value":value,"expected_absence":true}),
                "automation:record-worker"
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert!(jobs.recorded_events().is_empty());
    let (status, receipt) = post_as(&app, &job, &step, body, "automation:record-worker").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(receipt["record"]["actor"], "automation:record-worker");
}

#[tokio::test]
async fn first_record_declared_runner_requires_server_credential_and_exact_host() {
    use boss_jobs::field_writer::CredentialedCaller;
    for host in ["wrong-host", "record-host"] {
        let (app, jobs, mut job, mut step) = build_app(true).await;
        job.metadata = json!({"host":"record-host"});
        jobs.update_job(&job).await.unwrap();
        step.fields = vec![StepField {
            writer: Some("runner:ops".into()),
            ..StepField::new("evidence", "string")
        }];
        jobs.update_step(&step).await.unwrap();
        let app = app.layer(axum::Extension(CredentialedCaller {
            principal: "runner:ops".into(),
            actor_id: "automation:ops-runner".into(),
            host: Some(host.into()),
        }));
        let (status, receipt) = post_as(
            &app,
            &job,
            &step,
            json!({"key":"evidence","value":"verified","expected_absence":true}),
            "automation:ops-runner",
        )
        .await;
        if host == "record-host" {
            assert_eq!(status, StatusCode::CREATED);
            assert_eq!(receipt["record"]["actor"], "automation:ops-runner");
        } else {
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(jobs.recorded_events().is_empty());
        }
    }
}

#[tokio::test]
async fn first_record_http_preserves_null_and_server_receipt_on_replay() {
    let (app, jobs, job, step) = build_app(true).await;
    let body = json!({"key":"evidence","value":null,"expected_absence":true});
    let (status, recorded) = post(&app, &job, &step, body.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    let before = jobs.recorded_events().len();
    let (status, replay) = post(&app, &job, &step, body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(recorded["record"], replay["record"]);
    assert_eq!(before, jobs.recorded_events().len());
    assert_eq!(recorded["record"]["actor"], "emp-1");
    assert!(
        jobs.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .get("evidence")
            .unwrap()
            .is_null()
    );
    assert_eq!(
        post(
            &app,
            &job,
            &step,
            json!({"key":"evidence","value":"changed","expected_absence":true})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let user = json!({"id":"emp-1","role":"service-tech","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"service"});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{}/steps/{}/metadata", job.id, step.id))
                .header("content-type", "application/json")
                .header("x-boss-user", user.to_string())
                .body(Body::from(json!({"evidence":"rewrite"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "immutable record refusal is a named client conflict, not an internal server failure"
    );
}

#[tokio::test]
async fn original_first_record_read_preserves_receipt_without_a_replay_write() {
    let policy = FakePolicyClient::builder()
        .allow("service-tech", Action::Update, Resource::step(), Scope::All)
        .allow("service-tech", Action::Read, Resource::job(), Scope::All)
        .build();
    let (app, jobs, job, step) = build_app_with_policy(Arc::new(policy)).await;
    let (status, first) = post(
        &app,
        &job,
        &step,
        json!({"key":"evidence","value":9.0,"expected_absence":true}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let before = serde_json::to_value(jobs.recorded_events()).unwrap();
    let user = json!({"id":"emp-1","role":"service-tech","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"service"});
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/api/jobs/{}/steps/{}/records/evidence",
                    job.id, step.id
                ))
                .header("x-boss-user", user.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the immutable receipt needs an authorized original read, not a caller-value replay"
    );
    let original: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(original, first["record"]);
    assert_eq!(original["value_json"], "9.0");
    assert_eq!(
        serde_json::to_value(jobs.recorded_events()).unwrap(),
        before
    );
}

#[tokio::test]
async fn original_first_record_reads_preserve_policy_scope_and_resource_ownership() {
    for case in ["denied", "outage", "scope", "wrong-job", "absent-key"] {
        let policy: Arc<dyn boss_policy_client::PolicyClient> = match case {
            "denied" => Arc::new(FakePolicyClient::builder().build()),
            "outage" => Arc::new(UnavailablePolicy),
            "scope" => Arc::new(
                FakePolicyClient::builder()
                    .allow(
                        "service-tech",
                        Action::Read,
                        Resource::job(),
                        Scope::Territory,
                    )
                    .build(),
            ),
            _ => Arc::new(
                FakePolicyClient::builder()
                    .allow("service-tech", Action::Read, Resource::job(), Scope::All)
                    .build(),
            ),
        };
        let (app, jobs, job, step) = build_app_with_policy(policy).await;
        let stamp = boss_core::publisher::EventStamp::new(
            "control",
            boss_core::actor::ActorId::automation("read-control"),
        );
        jobs.record_step_metadata_at(&step.id, "evidence", &json!(9.0), &stamp)
            .await
            .unwrap();
        let before = serde_json::to_value(jobs.recorded_events()).unwrap();
        let jid = if case == "wrong-job" {
            boss_core::job::JobId::new()
        } else {
            job.id
        };
        let key = if case == "absent-key" {
            "absent"
        } else {
            "evidence"
        };
        let user = json!({"id":"emp-1","role":"service-tech","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"service"});
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/jobs/{jid}/steps/{}/records/{key}", step.id))
                    .header("x-boss-user", user.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let want = match case {
            "denied" => StatusCode::FORBIDDEN,
            "outage" => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::NOT_FOUND,
        };
        assert_eq!(response.status(), want, "{case}");
        assert_eq!(
            serde_json::to_value(jobs.recorded_events()).unwrap(),
            before,
            "{case}"
        );
    }
}

#[tokio::test]
async fn first_record_http_refuses_policy_denial_and_invalid_expectations_without_write() {
    let (app, jobs, job, step) = build_app(false).await;
    let before = jobs.recorded_events().len();
    assert_eq!(
        post(
            &app,
            &job,
            &step,
            json!({"key":"evidence","value":1,"expected_absence":true})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(before, jobs.recorded_events().len());
    let (app, jobs, job, step) = build_app(true).await;
    let before = jobs.recorded_events().len();
    for body in [
        json!({"value":1,"expected_absence":true}),
        json!({"key":null,"value":1,"expected_absence":true}),
        json!(["not an object"]),
        json!({"key":"x".repeat(257),"value":1,"expected_absence":true}),
        json!({"key":"evidence","value":1}),
        json!({"key":"evidence","value":1,"expected_absence":false}),
        json!({"key":"  ","value":1,"expected_absence":true}),
        json!({"key":"evidence","expected_absence":true}),
        json!({"key":"evidence","value":1,"expected_absence":true,"actor":"emp-david"}),
    ] {
        assert_eq!(
            post(&app, &job, &step, body).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(before, jobs.recorded_events().len());
}

#[tokio::test]
async fn first_record_http_cannot_create_reserved_protocol_or_forged_runner_evidence() {
    let (app, jobs, job, mut step) = build_app(true).await;
    step.fields = vec![StepField {
        writer: Some("runner:ops".into()),
        ..StepField::new("plan", "string")
    }];
    jobs.update_step(&step).await.unwrap();
    let before = jobs.recorded_events().len();
    assert_eq!(
        post(
            &app,
            &job,
            &step,
            json!({"key":"plan","value":"forged","expected_absence":true})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(
            &app,
            &job,
            &step,
            json!({"key":"human_only","value":false,"expected_absence":true})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(before, jobs.recorded_events().len());
}

#[tokio::test]
async fn first_record_policy_denial_precedes_repository_read_and_stale_admission_refuses() {
    let (app, jobs, job, step) = build_app(false).await;
    jobs.merge_after_next_read(
        &step.id,
        json!({"after_read":true}).as_object().unwrap().clone(),
    );
    assert_eq!(
        post(
            &app,
            &job,
            &step,
            json!({"key":"evidence","value":null,"expected_absence":true})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(
        jobs.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .get("after_read")
            .is_none(),
        "denied operation never consumed the repository read hook"
    );
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata["after_read"],
        true
    );
    let (app, jobs, job, step) = build_app(true).await;
    jobs.merge_after_next_read(
        &step.id,
        json!({"authority_role":"changed"})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(
        post(
            &app,
            &job,
            &step,
            json!({"key":"evidence","value":null,"expected_absence":true})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(
        jobs.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .get("evidence")
            .is_none()
    );
    assert!(jobs.recorded_events().is_empty());
}

#[tokio::test]
async fn checked_replay_refuses_authority_tightening_after_http_read() {
    let (app, jobs, job, mut step) = build_app(true).await;
    step.fields = vec![StepField::new("evidence", "string")];
    jobs.update_step(&step).await.unwrap();
    let body = json!({"key":"evidence","value":"observed","expected_absence":true});
    assert_eq!(
        post(&app, &job, &step, body.clone()).await.0,
        StatusCode::CREATED
    );
    let before = serde_json::to_value(jobs.recorded_events()).unwrap();
    jobs.merge_after_next_read(
        &step.id,
        json!({"human_only":true}).as_object().unwrap().clone(),
    );
    let (status, response) = post(&app, &job, &step, body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert!(response.get("record").is_none());
    assert_eq!(
        serde_json::to_value(jobs.recorded_events()).unwrap(),
        before
    );
    assert!(jobs.step_write_refusals(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn first_record_alias_receipt_uses_registered_actor_not_login() {
    use boss_jobs::agents::{InMemoryAgents, LoginDoor, resolve_login};
    let (app, jobs, job, step) = build_app(true).await;
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    let registry =
        Arc::new(InMemoryAgents::new().with_agent("agent-codex", ["codex@algedonic.dev"]));
    let door = Arc::new(LoginDoor::new(registry, DomainPublisher::new(bus, "jobs")));
    let app = app
        .layer(axum::middleware::from_fn(
            boss_policy_client::request_context_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(door, resolve_login));
    let user = json!({"id":"codex@algedonic.dev","role":"service-tech","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"service"});
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/jobs/{}/steps/{}/metadata/records",
                    job.id, step.id
                ))
                .header("content-type", "application/json")
                .header("x-boss-user", user.to_string())
                .body(Body::from(
                    json!({"key":"evidence","value":null,"expected_absence":true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["record"]["actor"], "agent-codex");
    assert!(
        jobs.recorded_events()
            .iter()
            .all(|event| event.payload["_actor"] == "agent-codex")
    );
}

mod rejection_count_controls {
    use super::*;
    use boss_core::job::{JobId, JobStatus, StepId};
    use boss_jobs::port::*;
    use chrono::{DateTime, Utc};

    struct CountedJobs {
        inner: Arc<InMemoryJobs>,
        calls: std::sync::Mutex<Vec<&'static str>>,
    }
    #[async_trait::async_trait]
    impl JobsRepository for CountedJobs {
        async fn create_job_with_steps_at(
            &self,
            job: &Job,
            steps: &[Step],
            now: DateTime<Utc>,
            job_events: &[boss_core::event::Event],
            step_events: &[boss_core::event::Event],
        ) -> Result<Admission, JobsError> {
            self.calls.lock().unwrap().push("create_job_with_steps_at");
            self.inner
                .create_job_with_steps_at(job, steps, now, job_events, step_events)
                .await
        }
        async fn get_job(&self, id: &JobId) -> Result<Option<Job>, JobsError> {
            self.calls.lock().unwrap().push("get_job");
            self.inner.get_job(id).await
        }
        async fn resolve_job_id_prefix(&self, prefix: &str) -> Result<Vec<JobId>, JobsError> {
            self.calls.lock().unwrap().push("resolve_job_id_prefix");
            self.inner.resolve_job_id_prefix(prefix).await
        }
        async fn update_job_at(
            &self,
            job: &Job,
            read: JobStatus,
            now: DateTime<Utc>,
            events: &[boss_core::event::Event],
        ) -> Result<(), JobsError> {
            self.calls.lock().unwrap().push("update_job_at");
            self.inner.update_job_at(job, read, now, events).await
        }
        async fn merge_job_metadata_at(
            &self,
            id: &JobId,
            patch: &serde_json::Map<String, serde_json::Value>,
            judged_on: Option<&Job>,
            stamp: &boss_core::publisher::EventStamp,
        ) -> Result<Job, JobsError> {
            self.calls.lock().unwrap().push("merge_job_metadata_at");
            self.inner
                .merge_job_metadata_at(id, patch, judged_on, stamp)
                .await
        }
        async fn close_job_at(
            &self,
            id: &JobId,
            closed_on: chrono::NaiveDate,
            owned: &serde_json::Map<String, serde_json::Value>,
            stamp: &boss_core::publisher::EventStamp,
            markers: &(dyn for<'j> Fn(&'j Job) -> Vec<boss_core::event::Event> + Send + Sync),
        ) -> Result<Option<Job>, JobsError> {
            self.calls.lock().unwrap().push("close_job_at");
            self.inner
                .close_job_at(id, closed_on, owned, stamp, markers)
                .await
        }
        async fn append_step_correction_at(
            &self,
            id: &JobId,
            entry: &serde_json::Value,
            stamp: &boss_core::publisher::EventStamp,
        ) -> Result<(Job, usize), JobsError> {
            self.calls.lock().unwrap().push("append_step_correction_at");
            self.inner.append_step_correction_at(id, entry, stamp).await
        }
        async fn list_estate_nodes(&self) -> Result<Vec<EstateNode>, JobsError> {
            self.calls.lock().unwrap().push("list_estate_nodes");
            self.inner.list_estate_nodes().await
        }
        async fn declare_estate_nodes(
            &self,
            declared: &[EstateNodeInput],
            stamp: &boss_core::publisher::EventStamp,
        ) -> Result<EstateBatchOutcome, JobsError> {
            self.calls.lock().unwrap().push("declare_estate_nodes");
            self.inner.declare_estate_nodes(declared, stamp).await
        }
        async fn recent_events_by_kind(
            &self,
            kind: &str,
            window: &EventWindow,
            limit: i64,
        ) -> Result<EventPage, JobsError> {
            self.calls.lock().unwrap().push("recent_events_by_kind");
            self.inner.recent_events_by_kind(kind, window, limit).await
        }
        async fn step_flow_cube(
            &self,
            since: DateTime<Utc>,
        ) -> Result<Vec<boss_jobs::station_flow::FlowCell>, JobsError> {
            self.calls.lock().unwrap().push("step_flow_cube");
            self.inner.step_flow_cube(since).await
        }
        async fn events_for_job(
            &self,
            job_id: &JobId,
            limit: i64,
        ) -> Result<Vec<boss_core::event::Event>, JobsError> {
            self.calls.lock().unwrap().push("events_for_job");
            self.inner.events_for_job(job_id, limit).await
        }
        async fn repin_workflow_version_at(
            &self,
            id: &JobId,
            to_version: i32,
            plan: &boss_jobs::repin::RepinPlan,
            record: &serde_json::Value,
            stamp: &boss_core::publisher::EventStamp,
        ) -> Result<Job, JobsError> {
            self.calls.lock().unwrap().push("repin_workflow_version_at");
            self.inner
                .repin_workflow_version_at(id, to_version, plan, record, stamp)
                .await
        }
        async fn list_jobs(
            &self,
            filter: &JobFilter,
            limit: i64,
            offset: i64,
        ) -> Result<(Vec<Job>, i64), JobsError> {
            self.calls.lock().unwrap().push("list_jobs");
            self.inner.list_jobs(filter, limit, offset).await
        }
        async fn add_step_at(
            &self,
            step: &Step,
            now: DateTime<Utc>,
            events: &[boss_core::event::Event],
        ) -> Result<(), JobsError> {
            self.calls.lock().unwrap().push("add_step_at");
            self.inner.add_step_at(step, now, events).await
        }
        async fn get_step(&self, id: &StepId) -> Result<Option<Step>, JobsError> {
            self.calls.lock().unwrap().push("get_step");
            self.inner.get_step(id).await
        }
        async fn get_step_versioned(
            &self,
            id: &StepId,
        ) -> Result<Option<(Step, StepVersion)>, JobsError> {
            self.calls.lock().unwrap().push("get_step_versioned");
            self.inner.get_step_versioned(id).await
        }
        async fn list_steps_versioned(
            &self,
            job_id: &JobId,
        ) -> Result<Vec<(Step, StepVersion)>, JobsError> {
            self.calls.lock().unwrap().push("list_steps_versioned");
            self.inner.list_steps_versioned(job_id).await
        }
        #[cfg(feature = "test-support")]
        async fn update_step_at(
            &self,
            step: &Step,
            now: DateTime<Utc>,
            events: &[boss_core::event::Event],
        ) -> Result<(), JobsError> {
            self.calls.lock().unwrap().push("update_step_at");
            self.inner.update_step_at(step, now, events).await
        }
        async fn update_step_if_unchanged_at(
            &self,
            step: &Step,
            read: StepVersion,
            now: DateTime<Utc>,
            events: &[boss_core::event::Event],
        ) -> Result<(), JobsError> {
            self.calls
                .lock()
                .unwrap()
                .push("update_step_if_unchanged_at");
            self.inner
                .update_step_if_unchanged_at(step, read, now, events)
                .await
        }
        async fn merge_step_metadata_at(
            &self,
            id: &StepId,
            patch: &serde_json::Map<String, serde_json::Value>,
            stamp: &boss_core::publisher::EventStamp,
        ) -> Result<Step, JobsError> {
            self.calls.lock().unwrap().push("merge_step_metadata_at");
            self.inner.merge_step_metadata_at(id, patch, stamp).await
        }
        async fn record_step_metadata_if_unchanged_at(
            &self,
            id: &StepId,
            key: &str,
            value: &serde_json::Value,
            read: Option<StepVersion>,
            stamp: &boss_core::publisher::EventStamp,
        ) -> Result<boss_jobs::first_record::FirstRecordResult, JobsError> {
            self.calls
                .lock()
                .unwrap()
                .push("record_step_metadata_if_unchanged_at");
            self.inner
                .record_step_metadata_if_unchanged_at(id, key, value, read, stamp)
                .await
        }
        async fn claim_step_displacing_at(
            &self,
            step_id: &StepId,
            actor: &str,
            displaceable: &[String],
            stamp: &boss_core::publisher::EventStamp,
            events: &[boss_core::event::Event],
        ) -> Result<Step, JobsError> {
            self.calls.lock().unwrap().push("claim_step_displacing_at");
            self.inner
                .claim_step_displacing_at(step_id, actor, displaceable, stamp, events)
                .await
        }
        async fn append_sign_off(
            &self,
            step_id: &StepId,
            stamp: &boss_core::job::SignOffStamp,
            event_stamp: &boss_core::publisher::EventStamp,
            events: &[boss_core::event::Event],
        ) -> Result<(), JobsError> {
            self.calls.lock().unwrap().push("append_sign_off");
            self.inner
                .append_sign_off(step_id, stamp, event_stamp, events)
                .await
        }
        async fn record_events(&self, events: &[boss_core::event::Event]) -> Result<(), JobsError> {
            self.calls.lock().unwrap().push("record_events");
            self.inner.record_events(events).await
        }
        async fn active_step_plugin_version(&self, kind: &str) -> Result<i32, JobsError> {
            self.calls
                .lock()
                .unwrap()
                .push("active_step_plugin_version");
            self.inner.active_step_plugin_version(kind).await
        }
        async fn list_steps(&self, job_id: &JobId) -> Result<Vec<Step>, JobsError> {
            self.calls.lock().unwrap().push("list_steps");
            self.inner.list_steps(job_id).await
        }
        async fn queue_age(&self, scope: &JobScope) -> Result<Vec<QueueAgeRow>, JobsError> {
            self.calls.lock().unwrap().push("queue_age");
            self.inner.queue_age(scope).await
        }
        async fn count_in_flight_steps_by_kind(&self, step_kind: &str) -> Result<i64, JobsError> {
            self.calls
                .lock()
                .unwrap()
                .push("count_in_flight_steps_by_kind");
            self.inner.count_in_flight_steps_by_kind(step_kind).await
        }
        async fn count_open_jobs_for_workflow(
            &self,
            kind: &str,
            version: i32,
        ) -> Result<i64, JobsError> {
            self.calls
                .lock()
                .unwrap()
                .push("count_open_jobs_for_workflow");
            self.inner.count_open_jobs_for_workflow(kind, version).await
        }
        async fn jobs_pinned_to_workflow(
            &self,
            kind: &str,
            version: i32,
        ) -> Result<PinnedJobs, JobsError> {
            self.calls.lock().unwrap().push("jobs_pinned_to_workflow");
            self.inner.jobs_pinned_to_workflow(kind, version).await
        }
        async fn count_jobs_by_kind(
            &self,
            status: Option<JobStatus>,
            scope: &JobScope,
        ) -> Result<Vec<(String, i64)>, JobsError> {
            self.calls.lock().unwrap().push("count_jobs_by_kind");
            self.inner.count_jobs_by_kind(status, scope).await
        }
        async fn resolve_blockers(
            &self,
            ids: &[StepId],
        ) -> Result<Vec<(StepId, StepStatus)>, JobsError> {
            self.calls.lock().unwrap().push("resolve_blockers");
            self.inner.resolve_blockers(ids).await
        }
        async fn record_step_write_refusal_at(
            &self,
            refusal: &boss_jobs::refusals::StepWriteRefusal,
            now: DateTime<Utc>,
        ) -> Result<(), JobsError> {
            self.calls
                .lock()
                .unwrap()
                .push("record_step_write_refusal_at");
            self.inner.record_step_write_refusal_at(refusal, now).await
        }
        async fn step_write_refusals(
            &self,
            limit: i64,
        ) -> Result<Vec<boss_jobs::refusals::RecordedRefusal>, JobsError> {
            self.calls.lock().unwrap().push("step_write_refusals");
            self.inner.step_write_refusals(limit).await
        }
    }

    async fn fixture(
        policy: Arc<dyn boss_policy_client::PolicyClient>,
    ) -> (Router, Arc<CountedJobs>, Job, Step) {
        let (_, inner, job, step) = build_app(true).await;
        let jobs = Arc::new(CountedJobs {
            inner,
            calls: std::sync::Mutex::new(Vec::new()),
        });
        let bus = RecordingEventBus::new();
        let bus_dyn: Arc<dyn EventBus> = bus.clone();
        let app = router(JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        ));
        (app, jobs, job, step)
    }

    #[tokio::test]
    async fn rejected_first_records_have_bounded_authorization_reads_and_no_writes_or_events() {
        let allow = || {
            Arc::new(
                FakePolicyClient::builder()
                    .allow("service-tech", Action::Update, Resource::step(), Scope::All)
                    .build(),
            ) as Arc<dyn boss_policy_client::PolicyClient>
        };
        let body = json!({"key":"evidence","value":1,"expected_absence":true});
        let mut not_found_bodies = Vec::new();
        let mut violations = Vec::new();
        for case in [
            "denied",
            "outage",
            "invalid",
            "missing-step",
            "wrong-job",
            "missing-job",
            "scope",
            "writer",
            "protocol",
            "schema",
        ] {
            let policy: Arc<dyn boss_policy_client::PolicyClient> = match case {
                "denied" => Arc::new(FakePolicyClient::builder().build()),
                "outage" => Arc::new(UnavailablePolicy),
                "scope" => Arc::new(
                    FakePolicyClient::builder()
                        .allow(
                            "service-tech",
                            Action::Update,
                            Resource::step(),
                            Scope::Territory,
                        )
                        .build(),
                ),
                _ => allow(),
            };
            let (app, jobs, mut job, mut step) = fixture(policy).await;
            match case {
                "missing-step" => step.id = StepId::new(),
                "wrong-job" => job.id = JobId::new(),
                "missing-job" => {
                    job.id = JobId::new();
                    step.job_id = job.id;
                    jobs.inner.update_step(&step).await.unwrap();
                }
                "writer" => {
                    step.fields = vec![StepField::new("evidence", "string")];
                    step.metadata = json!({"written_by":"someone-else"});
                    jobs.inner.update_step(&step).await.unwrap();
                }
                "schema" => {
                    step.fields = vec![StepField::new("evidence", "string")];
                    jobs.inner.update_step(&step).await.unwrap();
                }
                _ => (),
            }
            let before = jobs.inner.get_step(&step.id).await.unwrap();
            let events_before = jobs.inner.recorded_events();
            let request = match case {
                "invalid" => json!({"key":"evidence","value":1,"expected_absence":false}),
                "protocol" => json!({"key":"human_only","value":true,"expected_absence":true}),
                _ => body.clone(),
            };
            let (status, response) = post(&app, &job, &step, request).await;
            assert!(!status.is_success(), "{case}: {response}");
            let expected: &[&str] = match case {
                "denied" | "outage" | "invalid" => &[],
                "missing-step" | "wrong-job" => &["get_step_versioned"],
                _ => &["get_step_versioned", "get_job"],
            };
            let calls = jobs.calls.lock().unwrap().clone();
            if calls != expected {
                violations.push(format!("{case}: expected {expected:?}, actual {calls:?}"));
            }
            assert_eq!(
                jobs.inner.get_step(&step.id).await.unwrap(),
                before,
                "{case}"
            );
            assert_eq!(
                serde_json::to_value(jobs.inner.recorded_events()).unwrap(),
                serde_json::to_value(events_before).unwrap(),
                "{case}"
            );
            if matches!(case, "missing-step" | "wrong-job" | "missing-job" | "scope") {
                assert_eq!(status, StatusCode::NOT_FOUND);
                not_found_bodies.push(response);
            }
        }
        assert!(
            not_found_bodies.windows(2).all(|pair| pair[0] == pair[1]),
            "protected not-found responses must agree"
        );
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }

    #[tokio::test]
    async fn existing_metadata_merge_rejection_still_records_refusal_telemetry() {
        let (app, jobs, job, step) = fixture(Arc::new(FakePolicyClient::builder().build())).await;
        let user = json!({"id":"emp-1","role":"service-tech","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"service"});
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/jobs/{}/steps/{}/metadata", job.id, step.id))
                    .header("content-type", "application/json")
                    .header("x-boss-user", user.to_string())
                    .body(Body::from(json!({"evidence":1}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            jobs.calls.lock().unwrap().as_slice(),
            &["record_step_write_refusal_at"]
        );
        assert_eq!(jobs.inner.step_write_refusals(10).await.unwrap().len(), 1);
        assert!(jobs.inner.recorded_events().is_empty());
    }
}
