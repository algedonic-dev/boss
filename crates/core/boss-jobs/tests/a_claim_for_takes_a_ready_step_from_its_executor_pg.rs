//! A CLAIM FOR SOMEONE ELSE TAKES A READY STEP FROM ITS DECLARED
//! EXECUTOR — the Postgres half (backlog 5d1c0b7a). Materialisation hands
//! every `individual`-audience step to its executor, so the claim CAS,
//! which admitted only a NULL holder or the new one, refused the
//! executor's claim-for on every real step. The route names who a claim
//! may displace; the adapter admits a READY step held by one of them, by
//! exact spelling, and nothing else. The in-memory half is in
//! `step_claim_cas.rs`, the same three cases, so the adapters agree.

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_jobs::JobsRepository;
use boss_jobs::port::JobsError;
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

const EXECUTOR: &str = "automation:boss-step";

async fn seeded_step(repo: &boss_jobs::PgJobs, holder: &str, status: StepStatus) -> StepId {
    let job_id = JobId::from_uuid(Uuid::new_v4());
    let job = Job {
        id: job_id,
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "/it/backlog"),
        title: "A step born held by its declared executor".into(),
        owner_id: "emp-owner".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    };
    let mut step = Step::new(job_id, "task", "Run it", 0).with_assignee(holder);
    step.spec_slug = Some("run".into());
    step.status = status;
    step.metadata = serde_json::json!({
        boss_jobs::agent_runs::EDGE_KEY: "run-that-went-before",
    });
    repo.create_job(&job).await.unwrap();
    repo.add_step(&step).await.unwrap();
    step.id
}

fn stamp() -> boss_core::publisher::EventStamp {
    boss_core::publisher::EventStamp::new("jobs", boss_core::actor::ActorId::automation("test"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ready_step_held_by_a_displaceable_holder_goes_to_the_nominee() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step_id = seeded_step(&repo, EXECUTOR, StepStatus::Ready).await;

    let won = repo
        .claim_step_displacing_at(&step_id, "emp-b", &[EXECUTOR.to_string()], &stamp(), &[])
        .await
        .expect("a step held by the declared executor goes to the nominee");
    assert_eq!(won.assignee_id.as_deref(), Some("emp-b"));
    assert_eq!(won.status, StepStatus::Active);
    assert!(
        won.metadata.get(boss_jobs::agent_runs::EDGE_KEY).is_none(),
        "a new holder does not inherit the displaced holder's run edge: {}",
        won.metadata
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_active_step_is_never_displaced() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step_id = seeded_step(&repo, EXECUTOR, StepStatus::Active).await;

    match repo
        .claim_step_displacing_at(&step_id, "emp-b", &[EXECUTOR.to_string()], &stamp(), &[])
        .await
    {
        Err(JobsError::ClaimConflict { holder, status }) => {
            assert_eq!(holder.as_deref(), Some(EXECUTOR));
            assert_eq!(status, "active");
        }
        other => panic!("an active step must not be displaced, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_holder_the_claim_does_not_name_is_not_displaced() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let held_by_other = seeded_step(&repo, "emp-a", StepStatus::Ready).await;
    let named_other = repo
        .claim_step_displacing_at(
            &held_by_other,
            "emp-b",
            &[EXECUTOR.to_string()],
            &stamp(),
            &[],
        )
        .await;
    assert!(
        matches!(named_other, Err(JobsError::ClaimConflict { .. })),
        "{named_other:?}"
    );

    let held_by_executor = seeded_step(&repo, EXECUTOR, StepStatus::Ready).await;
    let plain = repo
        .claim_step_at(&held_by_executor, "emp-b", &stamp(), &[])
        .await;
    assert!(
        matches!(plain, Err(JobsError::ClaimConflict { .. })),
        "{plain:?}"
    );
    let after = repo.get_step(&held_by_executor).await.unwrap().unwrap();
    assert_eq!(after.assignee_id.as_deref(), Some(EXECUTOR));
    assert_eq!(after.status, StepStatus::Ready);
}
