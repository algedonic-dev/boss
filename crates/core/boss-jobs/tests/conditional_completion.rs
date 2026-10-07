//! A recovered alarm can withdraw unclaimed work without completing a
//! claimant's work. The evidence and completion are one conditional act.
use std::sync::Arc;

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
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn operator() -> User {
    User {
        id: "automation:observer".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

#[tokio::test]
async fn malformed_conditional_expectation_persists_no_refusal() {
    let (app, jobs, job, step) = fixture().await;
    let before = jobs.step_write_refusals(100).await.unwrap();
    let events = serde_json::to_value(jobs.recorded_events()).unwrap();
    let mut candidate = request();
    candidate["expected"]
        .as_object_mut()
        .unwrap()
        .remove("assignee_id");
    let (status, response) = post(&app, &job, &step, candidate).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert_eq!(jobs.get_step(&step.id).await.unwrap(), Some(step));
    assert_eq!(
        serde_json::to_value(jobs.recorded_events()).unwrap(),
        events
    );
    assert_eq!(
        jobs.step_write_refusals(100).await.unwrap().len(),
        before.len()
    );
}

#[tokio::test]
async fn ordinary_step_refusal_keeps_its_attempt_record() {
    let (app, jobs, job, step) = fixture_with_policy(Arc::new(FakePolicyClient::deny_all())).await;
    let events = serde_json::to_value(jobs.recorded_events()).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job.id, step.id))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&operator()).unwrap())
                .body(Body::from(json!({"status":"completed"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(jobs.get_step(&step.id).await.unwrap(), Some(step));
    assert_eq!(
        serde_json::to_value(jobs.recorded_events()).unwrap(),
        events
    );
    assert_eq!(jobs.step_write_refusals(100).await.unwrap().len(), 1);
}

async fn fixture() -> (Router, Arc<InMemoryJobs>, Job, Step) {
    fixture_with_policy(Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    ))
    .await
}

async fn fixture_with_policy(
    policy: Arc<dyn PolicyClient>,
) -> (Router, Arc<InMemoryJobs>, Job, Step) {
    fixture_with_registry(policy, None).await
}

async fn fixture_with_registry(
    policy: Arc<dyn PolicyClient>,
    registry: Option<Arc<dyn boss_jobs::registry::WorkflowRegistry>>,
) -> (Router, Arc<InMemoryJobs>, Job, Step) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let publisher = DomainPublisher::new(bus.clone() as Arc<dyn EventBus>, "jobs");
    let mut state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        publisher,
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    state.kind_registry = registry;
    let app = router(state);
    let job = Job::new(
        "test-alarm",
        Subject::new("custom", "watch"),
        "Recovered alarm",
        "emp-owner",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(),
    );
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Triage", 0);
    step.status = StepStatus::Ready;
    step.metadata = json!({"retained": {"context": "unchanged"}});
    jobs.add_step(&step).await.unwrap();
    (app, jobs, job, step)
}

fn request() -> Value {
    json!({"operation_id": "63f6e276-4a5e-40f8-84c6-38d8315d27cd",
        "expected": {"status": "ready", "assignee_id": null},
        "evidence": {"disposition": "stale", "evidence": "The missing receipt is now recorded"}})
}

async fn post(app: &Router, job: &Job, step: &Step, body: Value) -> (StatusCode, Value) {
    post_as(app, job, step, body, operator()).await
}

async fn post_as(
    app: &Router,
    job: &Job,
    step: &Step,
    body: Value,
    user: User,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/jobs/{}/steps/{}/complete-if",
                    job.id, step.id
                ))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&user).unwrap())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"text": String::from_utf8_lossy(&bytes)}));
    (status, body)
}

#[tokio::test]
async fn unclaimed_completion_returns_original_receipt_on_equal_replay() {
    let (app, jobs, job, step) = fixture().await;
    let (status, first) = post(&app, &job, &step, request()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["outcome"], "completed");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Completed);
    assert_eq!(stored.metadata["retained"], step.metadata["retained"]);
    assert_eq!(stored.metadata["disposition"], "stale");
    let events = jobs.recorded_events();
    let (status, second) = post(&app, &job, &step, request()).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["outcome"], "replayed");
    assert_eq!(first["receipt"], second["receipt"]);
    assert_eq!(jobs.recorded_events().len(), events.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn equal_in_memory_competitors_share_one_committed_receipt() {
    let (app, jobs, job, step) = fixture().await;
    let before = jobs.recorded_events().len();
    let start = Arc::new(tokio::sync::Barrier::new(2));
    let first_start = start.clone();
    let first_app = app.clone();
    let first_job = job.clone();
    let first_step = step.clone();
    let first = tokio::spawn(async move {
        first_start.wait().await;
        post(&first_app, &first_job, &first_step, request()).await
    });
    start.wait().await;
    let second = post(&app, &job, &step, request()).await;
    let first = first.await.unwrap();
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert_eq!(second.0, StatusCode::OK, "{}", second.1);
    let mut outcomes = vec![
        first.1["outcome"].as_str().unwrap(),
        second.1["outcome"].as_str().unwrap(),
    ];
    outcomes.sort();
    assert_eq!(outcomes, ["completed", "replayed"]);
    assert_eq!(first.1["receipt"], second.1["receipt"]);
    let after = jobs.recorded_events();
    let kinds: Vec<&str> = after[before..]
        .iter()
        .map(|event| event.kind.as_str())
        .collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == boss_jobs::events::STEP_UPDATED)
            .count(),
        1
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| kind.starts_with("step.done."))
            .count(),
        1
    );
    assert_eq!(
        kinds,
        [
            boss_jobs::events::STEP_UPDATED,
            boss_jobs::events::STEP_COMPLETED,
            "step.done.task"
        ]
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    let (status, replay) = post(&app, &job, &step, request()).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["receipt"], first.1["receipt"]);
    assert_eq!(
        serde_json::to_value(jobs.get_step(&step.id).await.unwrap().unwrap()).unwrap(),
        serde_json::to_value(stored).unwrap()
    );
    assert_eq!(jobs.recorded_events().len(), after.len());
}

#[tokio::test]
async fn a_claim_before_server_read_refuses_evidence_and_completion() {
    let (app, jobs, job, mut step) = fixture().await;
    step.status = StepStatus::Active;
    step.assignee_id = Some("emp-reviewer".into());
    jobs.update_step(&step).await.unwrap();
    let before = jobs.recorded_events().len();
    let (status, body) = post(&app, &job, &step, request()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["outcome"], "precondition_failed");
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata,
        step.metadata
    );
    assert_eq!(jobs.recorded_events().len(), before);
}

#[tokio::test]
async fn a_claim_between_read_and_commit_refuses_the_whole_operation() {
    let (app, jobs, job, step) = fixture().await;
    let before = jobs.recorded_events().len();
    jobs.change_before_next_judged_write(&step.id, |row| {
        row.status = StepStatus::Active;
        row.assignee_id = Some("emp-reviewer".into());
    });
    let (status, body) = post(&app, &job, &step, request()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["outcome"], "precondition_failed");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);
    assert_eq!(stored.assignee_id.as_deref(), Some("emp-reviewer"));
    assert_eq!(stored.metadata, step.metadata);
    assert_eq!(jobs.recorded_events().len(), before);
}

#[tokio::test]
async fn omitted_holder_or_client_provenance_is_refused_without_writes() {
    let (app, jobs, job, step) = fixture().await;
    let before = jobs.recorded_events().len();
    for body in [
        json!({"operation_id": "63f6e276-4a5e-40f8-84c6-38d8315d27cd", "expected": {"status": "ready"}, "evidence": {}}),
        json!({"operation_id": "63f6e276-4a5e-40f8-84c6-38d8315d27cd", "expected": {"status": "ready", "assignee_id": null}, "evidence": {}, "completed_by": "emp-forged"}),
    ] {
        let (status, answer) = post(&app, &job, &step, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
    }
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata,
        step.metadata
    );
    assert_eq!(jobs.recorded_events().len(), before);
}

#[tokio::test]
async fn nomination_drift_and_conflicting_terminal_replay_are_distinct() {
    let (app, jobs, job, mut step) = fixture().await;
    step.assignee_id = Some("emp-nominated".into());
    jobs.update_step(&step).await.unwrap();
    let (status, body) = post(&app, &job, &step, request()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["outcome"], "precondition_failed");
    let mut nominated = request();
    nominated["expected"]["assignee_id"] = json!("emp-nominated");
    let (status, body) = post(&app, &job, &step, nominated.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let before = jobs.recorded_events().len();
    nominated["evidence"]["evidence"] = json!("conflicting replacement");
    let (status, body) = post(&app, &job, &step, nominated).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["outcome"], "terminal_conflict");
    assert_eq!(jobs.recorded_events().len(), before);
}

#[tokio::test]
async fn unsupported_inline_or_unknown_effect_never_claims_completion_or_replay() {
    unsupported_effect_with_publication_coverage(true).await;
}

#[tokio::test]
async fn unconfigured_publication_stays_unavailable_without_writing_a_registry_fact() {
    unsupported_effect_with_publication_coverage(false).await;
}

struct PublicationHolders;

#[async_trait::async_trait]
impl boss_policy_client::coverage::CoverageSnapshotSource for PublicationHolders {
    async fn snapshot(&self) -> Result<boss_policy_client::coverage::CoverageSnapshot, String> {
        use boss_policy_client::coverage::{CoverageSnapshot, Key, Person};
        Ok(CoverageSnapshot {
            rules: boss_policy_client::defaults::default_rules(),
            overrides: vec![],
            roster: vec![Person {
                id: "emp-publication-holder".into(),
                role: Some("platform-admin".into()),
                active: true,
                hire_date: None,
            }],
            keys: vec![Key {
                employee_id: "emp-publication-holder".into(),
                access_tier: AccessTier::Operator,
            }],
        })
    }
}

async fn unsupported_effect_with_publication_coverage(covered: bool) {
    use boss_jobs::registry::{
        InMemoryWorkflows, StepSpec, Terminal, WorkflowRegistry, WorkflowSpec,
    };
    // This positive control must remain a viable publisher when assembled
    // with holder coverage. The unconfigured control separately preserves
    // the production fail-closed default (red train26de, alarm a08253ea).
    let registry = Arc::new(if covered {
        InMemoryWorkflows::guarded(Arc::new(PublicationHolders))
    } else {
        InMemoryWorkflows::new()
    });
    let policy = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let (app, jobs, job, step) = fixture_with_registry(policy, Some(registry.clone())).await;
    let spec = WorkflowSpec::platform_seed(
        "must-not-publish",
        "Publication",
        "production",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "start".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                ..Default::default()
            },
            StepSpec {
                title: "finish".into(),
                kind: "task".into(),
                ready_when: "steps.start.done".into(),
                terminal: Some(Terminal {
                    outcome: "finished".into(),
                }),
                ..Default::default()
            },
        ],
    );
    for kind in ["undeclared-inline-kind", "workflow-publish"] {
        let mut unsupported = Step::new(job.id, kind, "Effect", 1);
        unsupported.status = StepStatus::Ready;
        unsupported.metadata = json!({"workflow_spec": spec});
        jobs.add_step(&unsupported).await.unwrap();
        let before = jobs.recorded_events().len();
        for _ in 0..2 {
            let (status, body) = post(&app, &job, &unsupported, request()).await;
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert_eq!(body["outcome"], "unsupported_capability");
            assert_eq!(body["kind"], kind);
            assert!(
                body["error"]
                    .as_str()
                    .unwrap()
                    .contains("no evidence, completion or inline effect")
            );
            assert!(body.get("receipt").is_none());
        }
        assert_eq!(
            jobs.get_step(&unsupported.id)
                .await
                .unwrap()
                .unwrap()
                .metadata,
            unsupported.metadata
        );
        assert_eq!(
            jobs.get_step(&unsupported.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            StepStatus::Ready
        );
        assert_eq!(jobs.recorded_events().len(), before);
        assert!(
            registry
                .list_versions("must-not-publish")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(registry.recorded_events().is_empty());
        if kind == "workflow-publish" {
            // Positive control: this is a wired viable publisher, not an
            // unavailable port that could hide an accidental side effect.
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(format!("/api/jobs/{}/steps/{}", job.id, unsupported.id))
                        .header("content-type", "application/json")
                        .header("x-boss-user", serde_json::to_string(&operator()).unwrap())
                        .body(Body::from("{\"status\":\"completed\"}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(
                status,
                if covered {
                    StatusCode::NO_CONTENT
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                "{}",
                String::from_utf8_lossy(&bytes)
            );
            assert_eq!(
                registry
                    .list_versions("must-not-publish")
                    .await
                    .unwrap()
                    .len(),
                usize::from(covered)
            );
            if !covered {
                assert!(String::from_utf8_lossy(&bytes).contains("no coverage snapshot source"));
                assert!(registry.recorded_events().is_empty());
                assert_eq!(jobs.recorded_events().len(), before);
                assert_eq!(
                    jobs.get_step(&unsupported.id).await.unwrap(),
                    Some(unsupported)
                );
            } else {
                assert_eq!(registry.recorded_events().len(), 1);
                assert_eq!(
                    registry.recorded_events()[0].kind,
                    boss_jobs::events::WORKFLOW_PUBLISHED
                );
            }
        }
    }
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().status,
        StepStatus::Ready
    );
}

#[tokio::test]
async fn unrelated_terminal_completion_or_forged_row_marker_is_not_replay() {
    let (app, jobs, job, mut step) = fixture().await;
    step.status = StepStatus::Completed;
    step.completed_by = Some(boss_core::actor::ActorId::Automation("observer".into()));
    step.metadata["_conditional_completion"] = request();
    jobs.update_step(&step).await.unwrap();
    let before = jobs.recorded_events().len();
    let (status, body) = post(&app, &job, &step, request()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["outcome"], "terminal_conflict");
    assert_eq!(jobs.recorded_events().len(), before);
}

#[tokio::test]
async fn denied_or_narrowed_policy_records_neither_evidence_nor_completion() {
    for (policy, expected) in [
        (
            Arc::new(FakePolicyClient::deny_all()) as Arc<dyn PolicyClient>,
            StatusCode::FORBIDDEN,
        ),
        (
            Arc::new(
                FakePolicyClient::builder()
                    .allow(
                        "platform-admin",
                        Action::Update,
                        Resource::step(),
                        Scope::Self_,
                    )
                    .build(),
            ),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (app, jobs, job, step) = fixture_with_policy(policy).await;
        let before = jobs.recorded_events().len();
        let (status, body) = post(&app, &job, &step, request()).await;
        assert_eq!(status, expected, "{body}");
        assert_eq!(
            jobs.get_step(&step.id).await.unwrap().unwrap().metadata,
            step.metadata
        );
        assert_eq!(
            jobs.get_step(&step.id).await.unwrap().unwrap().status,
            StepStatus::Ready
        );
        assert_eq!(jobs.recorded_events().len(), before);
        assert!(jobs.step_write_refusals(100).await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn required_evidence_writer_and_protocol_guards_share_the_normal_boundary() {
    let (app, jobs, job, mut step) = fixture().await;
    let mut required = boss_core::job::StepField::new("finding", "string");
    required.required = true;
    step.fields.push(required);
    jobs.update_step(&step).await.unwrap();
    let before = jobs.recorded_events().len();
    let (status, body) = post(&app, &job, &step, request()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(jobs.recorded_events().len(), before);
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata,
        step.metadata
    );

    step.metadata["written_by"] = json!("rule:authorized");
    jobs.update_step(&step).await.unwrap();
    let mut authorized = request();
    authorized["evidence"]["finding"] = json!("Measured recovery");
    let (status, body) = post(&app, &job, &step, authorized.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata,
        step.metadata
    );
    assert_eq!(jobs.recorded_events().len(), before);

    step.metadata.as_object_mut().unwrap().remove("written_by");
    jobs.update_step(&step).await.unwrap();
    let mut reshape = authorized.clone();
    reshape["evidence"]["human_only"] = json!(true);
    let (status, body) = post(&app, &job, &step, reshape).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = post(&app, &job, &step, authorized).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata["finding"],
        "Measured recovery"
    );
}

#[tokio::test]
async fn changed_evidence_cannot_use_a_signoff_on_the_previous_shape() {
    let (app, jobs, job, mut step) = fixture().await;
    step.sign_offs_required = vec!["platform-admin".into()];
    step.id = boss_core::job::StepId::new();
    step.sign_offs.push(
        serde_json::from_value(json!({
            "authority_id": "emp-reviewer", "role": "platform-admin",
            "stamped_at": chrono::Utc::now(), "shape_hash": step.shape_hash(),
            "assurance": "session"
        }))
        .unwrap(),
    );
    jobs.add_step(&step).await.unwrap();
    let before = jobs.recorded_events().len();
    let (status, body) = post(&app, &job, &step, request()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata,
        step.metadata
    );
    assert_eq!(jobs.recorded_events().len(), before);
    let mut unchanged = request();
    unchanged["evidence"] = json!({});
    let (status, body) = post(&app, &job, &step, unchanged.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.shape_hash(), step.shape_hash());
    assert_eq!(stored.live_stamps().count(), 1);
    let before = jobs.recorded_events().len();
    let (status, replay) = post(&app, &job, &step, unchanged).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["receipt"], body["receipt"]);
    assert_eq!(jobs.recorded_events().len(), before);
}

#[tokio::test]
async fn version_token_refuses_an_unrelated_change_without_rechecking_a_new_row() {
    let (app, jobs, job, mut step) = fixture().await;
    let (_, version) = jobs.get_step_versioned(&step.id).await.unwrap().unwrap();
    let mut body = request();
    body["expected"]["version"] = json!(version.scoped_token(&job.id, &step.id));
    step.notes = Some("Changed after the reader's observation".into());
    jobs.update_step(&step).await.unwrap();
    let before = jobs.recorded_events().len();
    let (status, answer) = post(&app, &job, &step, body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["outcome"], "precondition_failed");
    assert_eq!(jobs.recorded_events().len(), before);
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().notes,
        step.notes
    );
}

#[tokio::test]
async fn anonymous_or_blank_identity_cannot_create_completion_provenance() {
    for id in [
        "",
        "   ",
        boss_core::roles::ANONYMOUS_USER_ID,
        boss_core::roles::GUEST_EMAIL,
    ] {
        let (app, jobs, job, step) = fixture().await;
        let mut user = operator();
        user.id = id.into();
        let before = jobs.recorded_events().len();
        let (status, body) = post_as(&app, &job, &step, request(), user).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "id={id:?}: {body}");
        assert_eq!(jobs.recorded_events().len(), before);
        assert_eq!(
            jobs.get_step(&step.id).await.unwrap().unwrap().status,
            StepStatus::Ready
        );
    }
}

#[tokio::test]
async fn version_read_is_scoped_to_the_packet_and_never_writes() {
    for (policy, expected) in [
        (
            Arc::new(FakePolicyClient::deny_all()) as Arc<dyn PolicyClient>,
            StatusCode::FORBIDDEN,
        ),
        (
            Arc::new(
                FakePolicyClient::builder()
                    .allow(
                        "platform-admin",
                        Action::Read,
                        Resource::job(),
                        Scope::Self_,
                    )
                    .build(),
            ),
            StatusCode::NOT_FOUND,
        ),
        (
            Arc::new(
                FakePolicyClient::builder()
                    .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
                    .build(),
            ),
            StatusCode::OK,
        ),
    ] {
        let (app, jobs, job, step) = fixture_with_policy(policy).await;
        let before = jobs.recorded_events().len();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/jobs/{}/steps/{}/version", job.id, step.id))
                    .header("x-boss-user", serde_json::to_string(&operator()).unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        if expected == StatusCode::OK {
            let answer: Value = serde_json::from_slice(&bytes).unwrap();
            let (stored, version) = jobs.get_step_versioned(&step.id).await.unwrap().unwrap();
            assert_eq!(answer["step"], serde_json::to_value(stored).unwrap());
            assert_eq!(answer["version"], version.scoped_token(&job.id, &step.id));
            let other = boss_core::job::JobId::new();
            let response = app
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/jobs/{other}/steps/{}/version", step.id))
                        .header("x-boss-user", serde_json::to_string(&operator()).unwrap())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        } else {
            assert!(!String::from_utf8_lossy(&bytes).contains("retained"));
        }
        assert_eq!(jobs.recorded_events().len(), before);
    }
}

struct UnavailablePolicy;
#[async_trait::async_trait]
impl PolicyClient for UnavailablePolicy {
    async fn check(
        &self,
        _: &User,
        _: Action,
        _: Resource,
    ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
        Err(boss_policy_client::PolicyClientError::Unreachable(
            "private policy diagnostic".into(),
        ))
    }
    async fn scope_predicate(
        &self,
        _: &User,
        _: Resource,
    ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
        Err(boss_policy_client::PolicyClientError::Unreachable(
            "private policy diagnostic".into(),
        ))
    }
}

#[tokio::test]
async fn unavailable_policy_cannot_record_evidence_or_a_receipt() {
    let (app, jobs, job, step) = fixture_with_policy(Arc::new(UnavailablePolicy)).await;
    let before = jobs.recorded_events().len();
    let (status, answer) = post(&app, &job, &step, request()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert!(answer.get("receipt").is_none());
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata,
        step.metadata
    );
    assert_eq!(jobs.recorded_events().len(), before);
    assert!(jobs.step_write_refusals(100).await.unwrap().is_empty());
}

#[tokio::test]
async fn conditional_evidence_obeys_credentialed_field_writer_not_asserted_identity() {
    use boss_jobs::field_writer::CredentialedCaller;
    let (app, jobs, job, mut step) = fixture().await;
    step.id = boss_core::job::StepId::new();
    step.fields = vec![serde_json::from_value(json!({"name":"finding", "field_type":"string", "required":true, "filled_by":"executor", "writer":"runner:ops"})).unwrap()];
    jobs.add_step(&step).await.unwrap();
    let mut forged = operator();
    forged.id = "automation:ops-runner".into();
    let mut evidence = request();
    evidence["evidence"]["finding"] = json!("Measured by the credentialed writer");
    let before = jobs.recorded_events().len();
    let (status, answer) = post_as(&app, &job, &step, evidence.clone(), forged.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap()
            .contains("declared writer")
    );
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata,
        step.metadata
    );
    assert_eq!(jobs.recorded_events().len(), before);
    let trusted = app.layer(axum::Extension(CredentialedCaller {
        principal: "runner:ops".into(),
        actor_id: forged.id.clone(),
        host: None,
    }));
    let (status, answer) = post_as(&trusted, &job, &step, evidence, forged).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata["finding"],
        "Measured by the credentialed writer"
    );
}

#[tokio::test]
async fn conditional_execution_and_replay_require_the_actual_declared_executor() {
    use boss_jobs::field_writer::CredentialedCaller;
    let (app, jobs, job, mut step) = fixture().await;
    step.id = boss_core::job::StepId::new();
    step.metadata["credential_executor"] = json!("runner:ops");
    jobs.add_step(&step).await.unwrap();
    let mut user = operator();
    user.id = "automation:ops-runner".into();
    let before = jobs.recorded_events().len();
    let (status, _) = post_as(&app, &job, &step, request(), user.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(jobs.recorded_events().len(), before);
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().status,
        StepStatus::Ready
    );
    let trusted = app.clone().layer(axum::Extension(CredentialedCaller {
        principal: "runner:ops".into(),
        actor_id: user.id.clone(),
        host: None,
    }));
    let (status, completed) = post_as(&trusted, &job, &step, request(), user.clone()).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["outcome"], "completed");
    let count = jobs.recorded_events().len();
    let (status, _) = post_as(&app, &job, &step, request(), user.clone()).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "terminal replay cannot bypass credential authority"
    );
    let (status, replay) = post_as(&trusted, &job, &step, request(), user).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["outcome"], "replayed");
    assert_eq!(completed["receipt"], replay["receipt"]);
    assert_eq!(jobs.recorded_events().len(), count);
}
