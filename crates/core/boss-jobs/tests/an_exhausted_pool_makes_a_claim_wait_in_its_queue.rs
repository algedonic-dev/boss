//! An exhausted pool makes a claim WAIT in its queue (backlog 587f395a,
//! allocation item 1 of design 012822ea).
//!
//! Measured on origin/main 3a7be357: a claim refused `capacity-exhausted`
//! wrote `metadata.waiting_on` and answered 409 with nothing about where
//! the step now stood, and claim-next offered the same refused head to
//! the next caller, which was refused again. David's steer: queues are
//! everywhere, so this is only sequencing — an exhausted pool means wait.
//! The conductor's reading (decided_questions_20261106_1507Z): wait is
//! both halves, a position answer and a re-offer when the pool frees.
//!
//! This file pins the store half through the real router on both
//! adapters: the refusal answers the step's position in its pool's line,
//! the step stays READY, a second refusal on the same pool keeps its
//! place, the line is readable at `GET /api/claims/waiting`, and
//! claim-next's head passes over a step holding its place until a release
//! re-offers it. The re-offer rule itself is pinned beside its handler
//! (boss-dispatcher-handlers, `a_released_pool_reoffers_its_oldest_waiting_step`).

use std::sync::Arc;

use axum::http::StatusCode;
use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::pools::{PgPools, PoolRegistry};
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry, WorkflowSpec};
use boss_jobs::{JobsRepository, PgJobs};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use serde_json::json;

struct Fixture<J> {
    jobs: Arc<J>,
    pools: Arc<dyn PoolRegistry>,
    workflows: Arc<dyn WorkflowRegistry>,
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => {
            let pools=Arc::new(boss_jobs::pools::InMemoryPools::default());
            let workflows=Arc::new(boss_jobs::registry::InMemoryWorkflows::for_fixture());
            let jobs=Arc::new(boss_jobs::InMemoryJobs::new().with_workflows(workflows.clone()).with_resource_pools(pools.clone()));
            (Fixture{jobs,pools,workflows},())
        },
        postgres => {
            let db=TestDb::new().await;
            (Fixture{jobs:Arc::new(PgJobs::new(db.pool.clone())),pools:Arc::new(PgPools::new(db.pool.clone())),workflows:Arc::new(PgWorkflows::for_fixture(db.pool.clone()))},db)
        },
    }
    cases {
        an_exhausted_claim_answers_its_position_and_the_step_stays_ready,
        a_closed_packets_step_leaves_the_line,
    }
}

/// One pool, `vcpu`, of capacity 1, and a protocol whose `work` step
/// bills one unit of it.
async fn seed<J: JobsRepository>(f: &Fixture<J>) -> boss_core::publisher::EventStamp {
    let stamp = boss_core::publisher::EventStamp::new("jobs", ActorId::automation("wait-test"));
    let spec: WorkflowSpec = serde_json::from_value(json!({"kind":"flow-contract","label":"Flow","category":"test","version":1,"status":"active","subject_kinds":["custom"],"owning_team":"platform","created_at":stamp.timestamp,
        "steps":[{"title":"start","kind":"trigger","ready_when":"true"},{"title":"work","kind":"task","duration_hours":1,"ready_when":"steps.start.done","resources":{"vcpu":"1"}},{"title":"done","kind":"outcome","ready_when":"steps.work.done","terminal":{"outcome":"done"}}]})).unwrap();
    f.workflows
        .publish_authored(spec, JobId::new(), stamp.actor(), stamp.timestamp)
        .await
        .unwrap();
    f.pools.publish_declared(serde_json::from_value(json!({"name":"vcpu","version":1,"status":"active","unit":"vCPU","kind":"flow","capacity":{"state":"configured","amount":"1"},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    stamp
}

async fn ready_work<J: JobsRepository>(
    f: &Fixture<J>,
    stamp: &boss_core::publisher::EventStamp,
) -> Step {
    let job = Job::new(
        "flow-contract",
        Subject::new("custom", "flow-contract"),
        "Waiting test",
        "emp-owner",
        Priority::Standard,
        stamp.timestamp.date_naive(),
    );
    f.jobs
        .create_job_at(
            &job,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::JOB_CREATED, json!(job))],
        )
        .await
        .unwrap();
    let mut step = Step::new(job.id, "task", "Work", 1);
    step.spec_slug = Some("work".into());
    step.status = StepStatus::Ready;
    f.jobs
        .add_step_at(
            &step,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_CREATED, json!(step))],
        )
        .await
        .unwrap();
    step
}

fn app<J: JobsRepository + 'static>(f: &Fixture<J>) -> axum::Router {
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
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
    router(JobsApiState {
        kind_registry: Some(f.workflows.clone()),
        ..JobsApiState::minimal(
            f.jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

async fn claim(app: &axum::Router, step: &Step, who: &str) -> (StatusCode, serde_json::Value) {
    let response = TestRequest::post(format!("/api/jobs/{}/steps/{}/claim", step.job_id, step.id))
        .as_user(who, "platform-admin")
        .send(app)
        .await;
    let body = serde_json::from_slice(&response.body_bytes).unwrap_or(serde_json::Value::Null);
    (response.status, body)
}

async fn the_line(app: &axum::Router) -> Vec<String> {
    let response = TestRequest::get("/api/claims/waiting?pool=vcpu")
        .as_user("emp-reader", "platform-admin")
        .send(app)
        .await;
    response.assert_status(StatusCode::OK);
    let body: serde_json::Value = response.assert_json();
    assert_eq!(body["pool"], "vcpu", "{body}");
    body["line"]
        .as_array()
        .unwrap_or_else(|| panic!("the line is a list: {body}"))
        .iter()
        .enumerate()
        .map(|(i, w)| {
            assert_eq!(
                w["position"],
                i + 1,
                "positions count from 1 in order: {body}"
            );
            w["step_id"].as_str().unwrap().to_string()
        })
        .collect()
}

async fn an_exhausted_claim_answers_its_position_and_the_step_stays_ready<
    J: JobsRepository + 'static,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed(f).await;
    let held = ready_work(f, &stamp).await;
    let first = ready_work(f, &stamp).await;
    let second = ready_work(f, &stamp).await;
    let app = app(f);

    let (status, _) = claim(&app, &held, "emp-one").await;
    assert_eq!(status, StatusCode::OK, "the one unit is taken");

    let (status, body) = claim(&app, &first, "emp-two").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["waiting_on"]["reason"], "capacity-exhausted",
        "the refusal still names its pool and reason in the shape it always did: {body}"
    );
    assert_eq!(body["queued"]["pool"], "vcpu", "{body}");
    assert_eq!(body["queued"]["state"], "queued", "{body}");
    assert_eq!(
        body["queued"]["position"], 1,
        "an exhausted pool answers WHERE the step waits, not only that it was refused: {body}"
    );
    let after = f.jobs.get_step(&first.id).await.unwrap().unwrap();
    assert_eq!(after.status, StepStatus::Ready, "the step stays queued");
    assert!(after.assignee_id.is_none());
    let since = after.metadata["waiting_on"]["since"].clone();
    assert!(
        since.is_string(),
        "the wait records when it began: {}",
        after.metadata
    );

    let (status, body) = claim(&app, &second, "emp-three").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["queued"]["position"], 2, "behind the first: {body}");
    assert_eq!(body["queued"]["waiting"], 2, "{body}");

    // Refused again on the same pool, the first keeps its place: the
    // instant it began waiting is kept, so it is still first.
    let (status, body) = claim(&app, &first, "emp-two").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["queued"]["position"], 1, "its place is kept: {body}");
    assert_eq!(
        f.jobs.get_step(&first.id).await.unwrap().unwrap().metadata["waiting_on"]["since"],
        since,
        "a second refusal on the same pool does not restart the wait"
    );

    assert_eq!(
        the_line(&app).await,
        vec![first.id.to_string(), second.id.to_string()],
        "the line reads in the order the steps began waiting"
    );
}

async fn a_closed_packets_step_leaves_the_line<J: JobsRepository + 'static>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed(f).await;
    let held = ready_work(f, &stamp).await;
    let waiting = ready_work(f, &stamp).await;
    let app = app(f);
    assert_eq!(claim(&app, &held, "emp-one").await.0, StatusCode::OK);
    assert_eq!(
        claim(&app, &waiting, "emp-two").await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(the_line(&app).await, vec![waiting.id.to_string()]);
    let mut job = f.jobs.get_job(&waiting.job_id).await.unwrap().unwrap();
    job.status = JobStatus::Cancelled;
    f.jobs
        .update_job_at(
            &job,
            JobStatus::Open,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::JOB_UPDATED, json!(job))],
        )
        .await
        .unwrap();
    assert!(
        the_line(&app).await.is_empty(),
        "a packet no longer open has nothing waiting in any pool"
    );
}

/// claim-next's head passes over a step that holds its place in a pool's
/// line — offering it again would only be refused again — and takes it
/// once a release has re-offered it.
#[test]
fn the_head_passes_over_a_step_holding_its_place_until_it_is_reoffered() {
    use boss_jobs::port::JobScope;
    use boss_jobs::station_queue::{StationPredicate, StepMatch};
    use boss_jobs::{StationKind, StationSpec};
    let now = chrono::Utc::now();
    let packet = |title: &str, opened: chrono::DateTime<chrono::Utc>| {
        let mut job = Job::new(
            "fixture",
            Subject::new("custom", title),
            title,
            "emp-owner",
            Priority::Standard,
            opened.date_naive(),
        );
        job.status = JobStatus::Open;
        let mut step = Step::new(job.id, "task", "Work", 0);
        step.status = StepStatus::Ready;
        step.spec_slug = Some("work".into());
        (job, step)
    };
    let (older_job, mut older) = packet("older", now - chrono::Duration::days(2));
    let (newer_job, newer) = packet("newer", now);
    let mut station = StationSpec::draft(
        "fixture",
        "Fixture",
        StationKind::Group,
        StationPredicate {
            kind: Some("fixture".into()),
            step: Some(StepMatch {
                slug: Some("work".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        now,
    );
    station.version = 1;
    station.status = boss_jobs::registry::WorkflowStatus::Active;
    let head = |older: &Step| {
        boss_jobs::station_claim::head(
            &station,
            &JobScope::All,
            &Default::default(),
            &[
                (older_job.clone(), vec![older.clone()]),
                (newer_job.clone(), vec![newer.clone()]),
            ],
        )
        .unwrap()
        .map(|(_, step)| step.id)
    };
    assert_eq!(head(&older), Some(older.id), "FIFO: the older packet heads");
    older.metadata["waiting_on"] =
        json!({"resource":"vcpu","reason":"capacity-exhausted","since":now});
    assert_eq!(
        head(&older),
        Some(newer.id),
        "a step waiting on an exhausted pool is not offered again"
    );
    older.metadata["waiting_on"]["reoffered_at"] = json!(now);
    assert_eq!(
        head(&older),
        Some(older.id),
        "re-offered, it heads its station again"
    );
    older.metadata["waiting_on"] = json!({"on":"packet","id":"6bc062b1-8d49-464f-abe1-6912c7d25fa3","since":now,"why":"dependency"});
    assert_eq!(
        head(&older),
        Some(older.id),
        "only a pool's line is passed over — a wait on a packet is not this rule's"
    );
}
