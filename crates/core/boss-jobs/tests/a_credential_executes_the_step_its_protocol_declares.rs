//! Design f623e425: a self-asserted runner is not a credentialed executor.
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use boss_core::{
    job::{Job, Priority, Step, Subject},
    port::EventBus,
    publisher::DomainPublisher,
};
use boss_jobs::{
    InMemoryJobs, JobsRepository,
    field_writer::CredentialedCaller,
    http::{JobsApiState, router},
};
use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

fn user() -> String {
    json!({"id":"automation:forged-proxy","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}).to_string()
}
fn app(jobs: Arc<InMemoryJobs>, caller: Option<CredentialedCaller>) -> Router {
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy = FakePolicyClient::builder()
        .allow(
            "platform-admin",
            Action::Update,
            Resource::step(),
            Scope::All,
        )
        .allow(
            "platform-admin",
            Action::Update,
            Resource::job(),
            Scope::All,
        )
        .build();
    let r = router(JobsApiState::minimal(
        jobs,
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        Arc::new(policy),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    match caller {
        Some(c) => r.layer(axum::Extension(c)),
        None => r,
    }
}
async fn fixture(declared: bool) -> (Arc<InMemoryJobs>, Step) {
    let jobs = Arc::new(InMemoryJobs::new());
    let mut job = Job::new(
        "executor-fixture",
        Subject::new("custom", "host"),
        "Execute",
        "automation:filer",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
    );
    job.metadata = json!({"host":"forge"});
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Execute", 0);
    step.status = boss_core::job::StepStatus::Ready;
    if declared {
        step.metadata = json!({"credential_executor":"runner:ops"});
    }
    jobs.add_step(&step).await.unwrap();
    (jobs, step)
}
async fn request(app: Router, step: &Step, method: &str, suffix: &str, body: Value) -> StatusCode {
    app.oneshot(
        Request::builder()
            .method(method)
            .uri(format!(
                "/api/jobs/{}/steps/{}{}",
                step.job_id, step.id, suffix
            ))
            .header("x-boss-user", user())
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}
fn caller(host: &str) -> CredentialedCaller {
    CredentialedCaller {
        principal: "runner:ops".into(),
        actor_id: "automation:ops-runner".into(),
        host: Some(host.into()),
    }
}

#[tokio::test]
async fn forged_execution_completion_and_assignment_are_refused() {
    for body in [
        json!({"status":"completed","completed_by":"automation:runner-credential-deposit"}),
        json!({"assignee_id":"automation:ops-runner"}),
    ] {
        let (jobs, step) = fixture(true).await;
        assert_eq!(
            request(app(jobs.clone(), None), &step, "PUT", "", body).await,
            StatusCode::CONFLICT
        );
        assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
    }
}
#[tokio::test]
async fn forged_claim_and_claim_for_are_refused() {
    for suffix in ["/claim", "/claim?claimed_for=automation:ops-runner"] {
        let (jobs, step) = fixture(true).await;
        assert_eq!(
            request(app(jobs, None), &step, "POST", suffix, json!({})).await,
            StatusCode::CONFLICT
        );
    }
}
#[tokio::test]
async fn wrong_host_is_refused() {
    let (jobs, step) = fixture(true).await;
    assert_eq!(
        request(
            app(jobs, Some(caller("boss-gcp"))),
            &step,
            "PUT",
            "",
            json!({"status":"completed"})
        )
        .await,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn credential_completion_ignores_proxy_attribution() {
    let (jobs, step) = fixture(true).await;
    assert_eq!(
        request(
            app(jobs.clone(), Some(caller("forge"))),
            &step,
            "PUT",
            "",
            json!({"status":"completed","completed_by":"emp-forged"})
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        jobs.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .completed_by
            .unwrap()
            .to_string(),
        "automation:ops-runner"
    );
    let events = jobs.recorded_events();
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .all(|e| e.payload["_actor"] == "automation:ops-runner"),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_resolved_credential_binds_current_user_and_ambient_actor_without_granting_a_role() {
    async fn inspect(
        boss_policy_client::CurrentUser(u): boss_policy_client::CurrentUser,
    ) -> axum::Json<Value> {
        axum::Json(
            json!({"id":u.id,"role":u.role,"actor":boss_core::actor_context::current_actor().map(|a|a.to_string())}),
        )
    }
    let dir = boss_testing::scratch_dir("executor-identity-door");
    boss_testing::write_file(&dir.join("forge.current"), "fixture-credential");
    let r = Router::new()
        .route("/inspect", axum::routing::get(inspect))
        .layer(axum::middleware::from_fn(
            boss_policy_client::request_context_middleware,
        ));
    let r = boss_jobs::runner_credential::mount(r, dir);
    let response = r
        .oneshot(
            Request::get("/inspect")
                .header("x-boss-user", user())
                .header(boss_jobs::runner_credential::HEADER, "fixture-credential")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["id"], "automation:ops-runner");
    assert_eq!(body["actor"], "automation:ops-runner");
    assert_eq!(body["role"], "platform-admin");
}
#[tokio::test]
async fn an_undeclared_proxy_keeps_its_existing_attribution() {
    let (jobs, step) = fixture(false).await;
    assert_eq!(
        request(
            app(jobs.clone(), None),
            &step,
            "PUT",
            "",
            json!({"status":"completed","completed_by":"emp-represented"})
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        jobs.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .completed_by
            .unwrap()
            .to_string(),
        "emp-represented"
    );
}
#[tokio::test]
async fn the_executor_declaration_is_not_a_metadata_writer_key() {
    let (jobs, step) = fixture(true).await;
    assert_eq!(
        request(
            app(jobs, Some(caller("forge"))),
            &step,
            "PATCH",
            "/metadata",
            json!({"credential_executor":null})
        )
        .await,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn a_credential_claim_names_the_authenticated_executor() {
    let (jobs, step) = fixture(true).await;
    assert_eq!(
        request(
            app(jobs.clone(), Some(caller("forge"))),
            &step,
            "POST",
            "/claim",
            json!({})
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        jobs.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .assignee_id
            .as_deref(),
        Some("automation:ops-runner")
    );
}

#[tokio::test]
async fn even_the_executor_cannot_claim_or_assign_another_actor() {
    for (method, suffix, body) in [
        ("POST", "/claim?claimed_for=emp-other", json!({})),
        ("PUT", "", json!({"assignee_id":"emp-other"})),
    ] {
        let (jobs, step) = fixture(true).await;
        assert_eq!(
            request(
                app(jobs, Some(caller("forge"))),
                &step,
                method,
                suffix,
                body
            )
            .await,
            StatusCode::CONFLICT
        );
    }
}

#[tokio::test]
async fn a_concurrent_change_refuses_the_execution_and_records_no_stale_fact() {
    let (jobs, step) = fixture(true).await;
    let before = jobs.recorded_events().len();
    jobs.change_before_next_judged_write(&step.id, |row| {
        row.status = boss_core::job::StepStatus::Active;
        row.assignee_id = Some("automation:ops-runner".into());
    });
    assert_eq!(
        request(
            app(jobs.clone(), Some(caller("forge"))),
            &step,
            "PUT",
            "",
            json!({"status":"completed"})
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().status,
        boss_core::job::StepStatus::Active
    );
    assert_eq!(jobs.recorded_events().len(), before);
}

#[tokio::test]
async fn simultaneous_credential_claims_leave_one_assignment_fact() {
    let (jobs, step) = fixture(true).await;
    let r = app(jobs.clone(), Some(caller("forge")));
    let (a, b) = tokio::join!(
        request(r.clone(), &step, "POST", "/claim", json!({})),
        request(r, &step, "POST", "/claim", json!({}))
    );
    assert_eq!(a, StatusCode::OK);
    assert_eq!(b, StatusCode::OK);
    let events = jobs.recorded_events();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "step.assigned.task")
            .count(),
        1,
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .all(|e| e.payload["_actor"] == "automation:ops-runner")
    );
}

#[tokio::test]
async fn a_real_door_refuses_absent_ambiguous_and_repeated_credentials() {
    for mode in ["absent", "ambiguous", "repeated", "invalid"] {
        let (jobs, step) = fixture(true).await;
        let dir = boss_testing::scratch_dir("executor-credential-door");
        boss_testing::write_file(&dir.join("forge.current"), "fixture-credential");
        if mode == "ambiguous" {
            boss_testing::write_file(&dir.join("boss-gcp.current"), "fixture-credential");
        }
        let r = boss_jobs::runner_credential::mount(app(jobs, None), dir);
        let mut req = Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{}/steps/{}", step.job_id, step.id))
            .header("x-boss-user", user())
            .header("content-type", "application/json");
        if mode != "absent" {
            req = req.header(
                boss_jobs::runner_credential::HEADER,
                if mode == "invalid" {
                    "unknown"
                } else {
                    "fixture-credential"
                },
            );
        }
        if mode == "repeated" {
            req = req.header(boss_jobs::runner_credential::HEADER, "fixture-credential");
        }
        assert_eq!(
            r.oneshot(
                req.body(Body::from(json!({"status":"completed"}).to_string()))
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
            StatusCode::CONFLICT,
            "{mode}"
        );
    }
}

#[test]
fn a_packet_with_only_an_executor_still_freezes_its_host() {
    let mut step = Step::new(boss_core::job::JobId::new(), "task", "Execute", 0);
    step.metadata = json!({"credential_executor":"runner:ops"});
    assert!(boss_jobs::field_writer::declares_a_writer([&step]));
}

#[test]
fn repinning_cannot_change_an_executor_declaration() {
    for next in [json!({}), json!({"credential_executor":"other"})] {
        assert!(
            !boss_jobs::field_writer::repin_refusals(
                &[],
                &[],
                &json!({"credential_executor":"runner:ops"}),
                &next
            )
            .is_empty()
        );
    }
    assert!(
        !boss_jobs::field_writer::repin_refusals(
            &[],
            &[],
            &json!({}),
            &json!({"credential_executor":"runner:ops"})
        )
        .is_empty()
    );
}

#[test]
fn an_executor_no_door_resolves_is_refused_at_publish() {
    use boss_jobs::{
        registry::{StepSpec, WorkflowSpec},
        step_registry::StepRegistry,
    };
    for principal in ["runner:ops", "runner:osp", "", " "] {
        let step: StepSpec = serde_json::from_value(
            json!({"title":"execute","kind":"task","ready_when":"true","executor":principal}),
        )
        .unwrap();
        let spec = WorkflowSpec::platform_seed(
            "executor-fixture",
            "Execute",
            "test",
            vec!["custom".into()],
            vec![step],
        );
        let errors = boss_jobs::workflow_lint::validate_workflow(&spec, &StepRegistry::v1());
        assert!(
            errors.iter().any(|e| e.reason.contains("executor")),
            "{principal:?}: {errors:?}"
        );
    }
}

#[test]
fn materialization_and_repin_conserve_the_executor_on_live_steps() {
    use boss_jobs::registry::{StepSpec, WorkflowSpec, materialize_steps_at};
    let spec = |principal: Option<&str>| {
        let step: StepSpec = serde_json::from_value(
            json!({"title":"execute","kind":"task","ready_when":"true","executor":principal}),
        )
        .unwrap();
        WorkflowSpec::platform_seed(
            "executor-fixture",
            "Execute",
            "test",
            vec!["custom".into()],
            vec![step],
        )
    };
    let mut job = Job::new(
        "executor-fixture",
        Subject::new("custom", "host"),
        "Execute",
        "automation:filer",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
    );
    job.metadata = json!({"host":"forge"});
    let from = spec(Some("runner:ops"));
    let rows = materialize_steps_at(
        &from,
        &job.subject,
        job.id,
        &job.metadata,
        boss_core::job::StepId::new,
        Some(job.opened_on),
        None,
    );
    assert_eq!(rows[0].metadata["credential_executor"], "runner:ops");
    assert!(boss_jobs::repin::plan(&from, &from, &job, &rows).is_ok());
    for to in [spec(None), spec(Some("other"))] {
        assert!(boss_jobs::repin::plan(&from, &to, &job, &rows).is_err());
    }
    let plain = spec(None);
    let plain_rows = materialize_steps_at(
        &plain,
        &job.subject,
        job.id,
        &job.metadata,
        boss_core::job::StepId::new,
        Some(job.opened_on),
        None,
    );
    assert!(boss_jobs::repin::plan(&plain, &from, &job, &plain_rows).is_err());
}

#[tokio::test]
async fn malformed_executor_declarations_refuse_without_a_fact() {
    for raw in [Value::Null, json!(7), json!(true), json!(""), json!(" ")] {
        let (jobs, mut step) = fixture(true).await;
        step.metadata["credential_executor"] = raw;
        jobs.update_step(&step).await.unwrap();
        let before = jobs.recorded_events().len();
        assert_eq!(
            request(
                app(jobs.clone(), Some(caller("forge"))),
                &step,
                "PUT",
                "",
                json!({"status":"completed"})
            )
            .await,
            StatusCode::CONFLICT
        );
        assert_eq!(jobs.recorded_events().len(), before);
        assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
    }
}

#[tokio::test]
async fn a_host_bound_executor_refuses_a_packet_with_no_frozen_host() {
    let (jobs, step) = fixture(true).await;
    let mut job = jobs.get_job(&step.job_id).await.unwrap().unwrap();
    job.metadata = json!({});
    jobs.update_job(&job).await.unwrap();
    let before = jobs.recorded_events().len();
    assert_eq!(
        request(
            app(jobs.clone(), Some(caller("forge"))),
            &step,
            "PUT",
            "",
            json!({"status":"completed"})
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(jobs.recorded_events().len(), before);
}

#[tokio::test]
async fn the_job_merge_door_freezes_a_host_for_an_executor_only_step() {
    for declared in [true, false] {
        let (jobs, step) = fixture(declared).await;
        let r = app(jobs.clone(), None);
        let response = r
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/jobs/{}/metadata", step.job_id))
                    .header("x-boss-user", user())
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"host":"boss-gcp"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if declared {
                StatusCode::CONFLICT
            } else {
                StatusCode::NO_CONTENT
            }
        );
        assert_eq!(
            jobs.get_job(&step.job_id).await.unwrap().unwrap().metadata["host"],
            if declared { "forge" } else { "boss-gcp" }
        );
    }
}

#[test]
fn a_repin_is_not_the_credential_executor_of_an_assignee_change() {
    use boss_jobs::registry::{StepSpec, WorkflowSpec, materialize_steps_at};
    for executor in [Some("runner:ops"), None] {
        let spec = |actor: &str| {
            let step:StepSpec=serde_json::from_value(json!({"title":"execute","kind":"task","ready_when":"true","executor":executor,"audience":{"individual":actor}})).unwrap();
            WorkflowSpec::platform_seed(
                "executor-fixture",
                "Execute",
                "test",
                vec!["custom".into()],
                vec![step],
            )
        };
        let mut job = Job::new(
            "executor-fixture",
            Subject::new("custom", "host"),
            "Execute",
            "automation:filer",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
        );
        job.metadata = json!({"host":"forge"});
        let from = spec("automation:ops-runner");
        let to = spec("emp-other");
        let rows = materialize_steps_at(
            &from,
            &job.subject,
            job.id,
            &job.metadata,
            boss_core::job::StepId::new,
            Some(job.opened_on),
            None,
        );
        assert_eq!(
            rows[0].assignee_id.as_deref(),
            Some("automation:ops-runner")
        );
        let result = boss_jobs::repin::plan(&from, &to, &job, &rows);
        assert_eq!(result.is_err(), executor.is_some(), "{result:?}");
    }
}

#[tokio::test]
async fn a_forged_proxy_cannot_return_an_active_execution_to_ready() {
    let (jobs, mut step) = fixture(true).await;
    step.status = boss_core::job::StepStatus::Active;
    step.assignee_id = Some("automation:ops-runner".into());
    jobs.update_step(&step).await.unwrap();
    assert_eq!(
        request(
            app(jobs.clone(), None),
            &step,
            "PUT",
            "",
            json!({"status":"ready"})
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
}
