//! Competing approved authoring steps exercise the native conditional
//! workflow write through HTTP, with a real row/outbox transaction.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use boss_core::{
    actor::ActorId,
    job::{Job, JobId, JobStatus, Priority, StepId, StepStatus, Subject},
    port::EventBus,
    publisher::DomainPublisher,
};
use boss_jobs::{
    JobsRepository, PgJobs,
    events::WORKFLOW_PUBLISHED,
    http::{JobsApiState, router},
    registry::{
        PgWorkflows, StepSpec, Terminal, WorkflowRegistry, WorkflowSpec, materialize_steps,
        seedable_platform_workflows,
    },
};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::NaiveDate;
use serde_json::json;
use tower::ServiceExt;

fn definition() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "conditional-http-race",
        "Native publication",
        "platform",
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
                    outcome: "done".into(),
                }),
                ..Default::default()
            },
        ],
    )
}

async fn authoring_job(
    jobs: &PgJobs,
    protocol: &WorkflowSpec,
    spec: WorkflowSpec,
) -> (JobId, StepId) {
    let mut job = Job::new(
        "workflow-design",
        Subject::new("custom", &spec.kind),
        "Approved conditional definition",
        "emp-cto",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(),
    );
    job.status = JobStatus::Open;
    job.workflow_version = protocol.version;
    jobs.create_job(&job).await.unwrap();
    let mut publication = None;
    for mut step in materialize_steps(protocol, &job.subject, job.id, &job.metadata, StepId::new) {
        match step.spec_slug.as_deref() {
            Some("author" | "validate") => step.status = StepStatus::Completed,
            Some("approve") => {
                step.status = StepStatus::Completed;
                step.metadata["decision"] = json!("approved");
            }
            Some("publish") => {
                step.status = StepStatus::Active;
                step.metadata["workflow_spec"] = serde_json::to_value(&spec).unwrap();
                step.metadata["insert_if_absent"] = json!(true);
                publication = Some(step.id);
            }
            _ => {}
        }
        jobs.add_step(&step).await.unwrap();
    }
    (job.id, publication.unwrap())
}

async fn complete(
    app: &Router,
    job: JobId,
    step: StepId,
    user: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{job}/steps/{step}"))
                .header("content-type", "application/json")
                .header("x-boss-user", user)
                .body(Body::from(json!({"status":"completed"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn conditional_publish_http_race_records_one_native_definition_and_actor() {
    let db = TestDb::new().await;
    let kinds = Arc::new(PgWorkflows::for_fixture(db.pool.clone()));
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Update, Resource::step(), Scope::All)
            .allow("cto", Action::Read, Resource::job(), Scope::All)
            .allow("cto", Action::Publish, Resource::workflow(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        kind_registry: Some(kinds.clone()),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            publisher,
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let app = router(state);
    let protocol = seedable_platform_workflows()
        .into_iter()
        .find(|row| row.kind == "workflow-design")
        .unwrap();
    kinds
        .bootstrap_reconcile(
            std::slice::from_ref(&protocol),
            &ActorId::Automation("conditional-test-fixture".into()),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let mut other = definition();
    other.description = Some("Different racing authored description".into());
    let (a, a_step) = authoring_job(&jobs, &protocol, definition()).await;
    let (b, b_step) = authoring_job(&jobs, &protocol, other).await;
    let user = serde_json::to_string(&User {
        id: "emp-cto".into(),
        role: "cto".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("executive".into()),
    })
    .unwrap();
    let (first, second) = tokio::join!(
        complete(&app, a, a_step, &user),
        complete(&app, b, b_step, &user)
    );
    assert_eq!(
        usize::from(first.status() == StatusCode::NO_CONTENT)
            + usize::from(second.status() == StatusCode::NO_CONTENT),
        1,
        "{}/{}",
        first.status(),
        second.status()
    );
    assert_eq!(
        usize::from(first.status() == StatusCode::CONFLICT)
            + usize::from(second.status() == StatusCode::CONFLICT),
        1
    );
    let active = kinds.get_active("conditional-http-race").await.unwrap();
    assert_eq!(
        kinds.list_versions(&active.kind).await.unwrap(),
        vec![active.clone()]
    );
    let payloads: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind = $1 AND payload->>'kind' = $2",
    )
    .bind(WORKFLOW_PUBLISHED)
    .bind(&active.kind)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(payloads.len(), 1);
    let mut expected = serde_json::to_value(&active).unwrap();
    expected["_actor"] = json!("emp-cto");
    assert_eq!(payloads[0], expected);
    let (winner, winner_step, loser_step) = if first.status() == StatusCode::NO_CONTENT {
        (a, a_step, b_step)
    } else {
        (b, b_step, a_step)
    };
    assert_eq!(active.authoring_job_id, Some(*winner.inner().as_uuid()));
    assert_eq!(
        jobs.get_step(&winner_step).await.unwrap().unwrap().status,
        StepStatus::Completed
    );
    assert_eq!(
        jobs.get_step(&loser_step).await.unwrap().unwrap().status,
        StepStatus::Active
    );
}
