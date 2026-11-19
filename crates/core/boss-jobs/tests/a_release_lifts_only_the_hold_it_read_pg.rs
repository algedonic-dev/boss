//! Postgres half of the merge precondition (review 792b871a N1).
//!
//! The release door judges a held car, then writes `{hold: null,
//! release}` through the step merge door. A hold rewritten between that
//! read and the write — a person's `boss hold` keeps `hold_sha` — must
//! not be lifted, so the write carries the `hold` and `hold_sha` the door
//! read as a precondition, judged on the row `FOR UPDATE` in the write's
//! own transaction. The in-memory adapter judges it under its lock
//! (`in_memory::tests::a_merge_with_a_precondition_lands_only_on_the_row_it_read`);
//! this pins the same rule against the real SQL: a row that no longer
//! reads as expected is refused as `StepChanged` and writes neither row
//! nor outbox event, and a row that does lands, whatever unrelated key
//! was written since.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::JobsRepository;
use boss_jobs::port::JobsError;
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("test".into()))
}

fn map(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("test patches are objects"),
    }
}

async fn held(repo: &boss_jobs::PgJobs, id: &str) -> Step {
    let j = Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        status: JobStatus::Open,
        metadata: serde_json::json!({ "branch": "feat/x" }),
        ..Job::new(
            "ship-a-change",
            Subject::new("custom", "car"),
            "A held car",
            "emp-1",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(),
        )
    };
    repo.create_job(&j).await.unwrap();
    let mut step = Step::new(j.id, "task", "Open for review", 1);
    step.status = StepStatus::Active;
    step.metadata = serde_json::json!({ "hold": "trust-boundary car", "hold_sha": "h1" });
    repo.add_step(&step).await.unwrap();
    step
}

async fn outbox_updates(db: &TestDb, step: &Step) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM event_outbox \
         WHERE kind = 'jobs.step.updated' AND payload->>'step_id' = $1",
    )
    .bind(step.id.to_string())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    n
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_rewritten_after_the_read_is_not_lifted() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step = held(&repo, "00000000-0000-0000-0000-0000792b0001").await;
    let as_read = map(serde_json::json!({ "hold": "trust-boundary car", "hold_sha": "h1" }));

    // A person's hold lands after the door read the car, at the same head.
    repo.merge_step_metadata_at(
        &step.id,
        &map(serde_json::json!({ "hold": "David reads it first" })),
        &stamp(),
    )
    .await
    .unwrap();
    let before = outbox_updates(&db, &step).await;

    let release = map(serde_json::json!({ "hold": null, "release": { "sha": "h1" } }));
    let answer = repo
        .merge_step_metadata_guarded_at(&step.id, &release, &stamp(), None, Some(&as_read))
        .await;
    assert!(
        matches!(answer, Err(JobsError::StepChanged { id }) if id == step.id),
        "a write judged on another hold is refused by name, got {answer:?}"
    );
    let stored = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        stored.metadata["hold"], "David reads it first",
        "not lifted"
    );
    assert!(stored.metadata.get("release").is_none(), "nothing written");
    assert_eq!(outbox_updates(&db, &step).await, before, "and no event");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_on_the_hold_it_read_lands_past_an_unrelated_key() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step = held(&repo, "00000000-0000-0000-0000-0000792b0002").await;
    let as_read = map(serde_json::json!({ "hold": "trust-boundary car", "hold_sha": "h1" }));
    repo.merge_step_metadata_at(
        &step.id,
        &map(serde_json::json!({ "note": "unrelated" })),
        &stamp(),
    )
    .await
    .unwrap();
    let release = map(serde_json::json!({ "hold": null, "release": { "sha": "h1" } }));
    let landed = repo
        .merge_step_metadata_guarded_at(&step.id, &release, &stamp(), None, Some(&as_read))
        .await
        .expect("lands on the hold it read");
    assert!(landed.metadata.get("hold").is_none(), "{}", landed.metadata);
    assert_eq!(landed.metadata["release"]["sha"], "h1");
    assert_eq!(landed.metadata["note"], "unrelated");
}
