//! The generic first-record contract starts with its measured overwrite counterexample.
use boss_core::{
    actor::ActorId,
    job::{Job, Priority, Step, StepStatus, Subject},
    publisher::EventStamp,
};
use boss_jobs::{InMemoryJobs, JobsRepository};

async fn race_records<R: JobsRepository>(repo: &R, equal: bool) {
    use boss_jobs::first_record::FirstRecordResult;
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "race"),
        "Race evidence",
        "automation:test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
    );
    repo.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Record", 0);
    step.status = StepStatus::Active;
    repo.add_step(&step).await.unwrap();
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    assert!(matches!(
        repo.record_step_metadata_at(
            &boss_core::job::StepId::new(),
            "evidence",
            &serde_json::Value::Null,
            &stamp
        )
        .await
        .unwrap(),
        boss_jobs::first_record::FirstRecordResult::NotFound
    ));
    assert!(
        repo.record_step_metadata_at(&step.id, " \u{2003}", &serde_json::Value::Null, &stamp)
            .await
            .is_err()
    );
    let barrier = tokio::sync::Barrier::new(2);
    let first = serde_json::json!({"null":null,"number":1.0});
    let other = if equal {
        first.clone()
    } else {
        serde_json::json!({"other":true})
    };
    let attempt = |value: serde_json::Value| {
        let barrier = &barrier;
        let id = step.id;
        async move {
            assert!(
                repo.get_step(&id)
                    .await
                    .unwrap()
                    .unwrap()
                    .metadata
                    .get("evidence")
                    .is_none()
            );
            barrier.wait().await;
            repo.record_step_metadata_at(
                &id,
                "evidence",
                &value,
                &EventStamp::new("jobs", ActorId::Automation("test".into())),
            )
            .await
            .unwrap()
        }
    };
    let (a, b) = tokio::join!(attempt(first), attempt(other));
    let (record, loser) = match (a, b) {
        (FirstRecordResult::Recorded(record), loser)
        | (loser, FirstRecordResult::Recorded(record)) => (record, loser),
        outcomes => panic!("exactly one recording required: {outcomes:?}"),
    };
    if equal {
        assert_eq!(loser, FirstRecordResult::Replayed(record.clone()));
    } else {
        assert!(matches!(loser, FirstRecordResult::Conflict { .. }));
    }
    assert_eq!(
        repo.get_step(&step.id).await.unwrap().unwrap().metadata["evidence"],
        record.value
    );
}

async fn stale_record_refuses<R: JobsRepository>(repo: &R) {
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "stale"),
        "Stale authority",
        "automation:test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
    );
    repo.create_job(&job).await.unwrap();
    let step = Step::new(job.id, "task", "Record", 0);
    repo.add_step(&step).await.unwrap();
    let (_, version) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    repo.merge_step_metadata_at(
        &step.id,
        serde_json::json!({"writer":"changed"}).as_object().unwrap(),
        &stamp,
    )
    .await
    .unwrap();
    assert!(matches!(
        repo.record_step_metadata_if_unchanged_at(
            &step.id,
            "evidence",
            &serde_json::Value::Null,
            Some(version),
            &stamp
        )
        .await,
        Err(boss_jobs::port::JobsError::StepChanged { .. })
    ));
    assert!(
        repo.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .get("evidence")
            .is_none()
    );
}

async fn stale_checked_replay_refuses<R: JobsRepository>(repo: &R) {
    use boss_jobs::first_record::FirstRecordResult;
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "stale-replay"),
        "Stale replay authority",
        "automation:test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
    );
    repo.create_job(&job).await.unwrap();
    let step = Step::new(job.id, "task", "Record", 0);
    repo.add_step(&step).await.unwrap();
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let FirstRecordResult::Recorded(original) = repo
        .record_step_metadata_at(&step.id, "evidence", &serde_json::Value::Null, &stamp)
        .await
        .unwrap()
    else {
        panic!("first record");
    };
    let (_, stale) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    repo.merge_step_metadata_at(
        &step.id,
        serde_json::json!({"human_only":true}).as_object().unwrap(),
        &stamp,
    )
    .await
    .unwrap();
    let before = repo.get_step(&step.id).await.unwrap().unwrap();
    assert!(
        matches!(
            repo.record_step_metadata_if_unchanged_at(
                &step.id,
                "evidence",
                &serde_json::Value::Null,
                Some(stale),
                &stamp
            )
            .await,
            Err(boss_jobs::port::JobsError::StepChanged { .. })
        ),
        "checked replay must not reuse stale authorization"
    );
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), before);
    let (_, fresh) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    for read in [Some(fresh), None] {
        let FirstRecordResult::Replayed(replay) = repo
            .record_step_metadata_if_unchanged_at(
                &step.id,
                "evidence",
                &serde_json::Value::Null,
                read,
                &stamp,
            )
            .await
            .unwrap()
        else {
            panic!("fresh/trusted replay");
        };
        assert_eq!(replay, original);
    }
}

#[tokio::test]
async fn memory_stale_checked_replay_refuses_without_changing_receipt() {
    let repo = InMemoryJobs::new();
    stale_checked_replay_refuses(&repo).await;
    assert_eq!(
        repo.recorded_events()
            .iter()
            .filter(|event| event.kind == boss_jobs::events::STEP_FIRST_RECORDED)
            .count(),
        1
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_stale_checked_replay_refuses_without_changing_receipt() {
    let db = boss_testing::TestDb::new().await;
    stale_checked_replay_refuses(&boss_jobs::PgJobs::new(db.pool.clone())).await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind=$1")
        .bind(boss_jobs::events::STEP_FIRST_RECORDED)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

async fn completion_record_race<R: JobsRepository>(repo: &R) {
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "completion-race"),
        "Completion race",
        "automation:test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
    );
    repo.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Record", 0);
    step.status = StepStatus::Active;
    repo.add_step(&step).await.unwrap();
    let (mut completion, version) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    completion.status = StepStatus::Completed;
    let barrier = tokio::sync::Barrier::new(2);
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let record = async {
        barrier.wait().await;
        repo.record_step_metadata_at(&step.id, "evidence", &serde_json::Value::Null, &stamp)
            .await
            .unwrap()
    };
    let finish = async {
        barrier.wait().await;
        repo.update_step_if_unchanged_at(
            &completion,
            version,
            stamp.timestamp,
            &[stamp.event(
                boss_jobs::events::STEP_UPDATED,
                boss_jobs::events::step_state_payload(&completion),
            )],
        )
        .await
    };
    let (record, finish) = tokio::join!(record, finish);
    let current = repo.get_step(&step.id).await.unwrap().unwrap();
    match record {
        boss_jobs::first_record::FirstRecordResult::Recorded(_) => {
            assert!(
                finish.is_err(),
                "stale completion cannot erase the new record"
            );
            assert_eq!(current.status, StepStatus::Active);
            assert!(current.metadata.get("evidence").unwrap().is_null());
        }
        boss_jobs::first_record::FirstRecordResult::Terminal => {
            assert!(finish.is_ok());
            assert_eq!(current.status, StepStatus::Completed);
            assert!(current.metadata.get("evidence").is_none());
        }
        result => panic!("invalid completion race result: {result:?}"),
    }
}

#[tokio::test]
async fn memory_barrier_races_and_stale_authority_conserve_records() {
    for equal in [false, true] {
        let repo = InMemoryJobs::new();
        race_records(&repo, equal).await;
        let events = repo.recorded_events();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == boss_jobs::events::STEP_FIRST_RECORDED)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == boss_jobs::events::STEP_UPDATED)
                .count(),
            1
        );
    }
    stale_record_refuses(&InMemoryJobs::new()).await;
    completion_record_race(&InMemoryJobs::new()).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_barrier_races_and_stale_authority_conserve_records() {
    for equal in [false, true] {
        let db = boss_testing::TestDb::new().await;
        race_records(&boss_jobs::PgJobs::new(db.pool.clone()), equal).await;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind=$1")
            .bind(boss_jobs::events::STEP_FIRST_RECORDED)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }
    let db = boss_testing::TestDb::new().await;
    stale_record_refuses(&boss_jobs::PgJobs::new(db.pool.clone())).await;
    let db = boss_testing::TestDb::new().await;
    completion_record_race(&boss_jobs::PgJobs::new(db.pool.clone())).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_outbox_refusal_rolls_back_evidence_and_receipt() {
    let db = boss_testing::TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "rollback"),
        "Atomic failure",
        "automation:test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
    );
    repo.create_job(&job).await.unwrap();
    let step = Step::new(job.id, "task", "Record", 0);
    repo.add_step(&step).await.unwrap();
    sqlx::query("ALTER TABLE event_outbox ADD CONSTRAINT refuse_first_record_control CHECK (kind <> 'jobs.step.first_recorded')")
        .execute(&db.pool).await.unwrap();
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    assert!(
        repo.record_step_metadata_at(&step.id, "evidence", &serde_json::Value::Null, &stamp)
            .await
            .is_err()
    );
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), step);
    let records: i64 = sqlx::query_scalar("SELECT count(*) FROM step_first_records")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!((records, events), (0, 0));
    sqlx::query("ALTER TABLE event_outbox DROP CONSTRAINT refuse_first_record_control")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(matches!(
        repo.record_step_metadata_at(&step.id, "evidence", &serde_json::Value::Null, &stamp)
            .await
            .unwrap(),
        boss_jobs::first_record::FirstRecordResult::Recorded(_)
    ));
}

async fn first_record_cannot_be_replaced<R: JobsRepository>(repo: &R) {
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "first-record-control"),
        "Preserve first evidence",
        "automation:test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
    );
    repo.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Record", 0);
    step.status = StepStatus::Active;
    repo.add_step(&step).await.unwrap();
    // Both candidates observe absence before either reaches the recording boundary.
    assert!(
        repo.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .get("evidence")
            .is_none()
    );
    assert!(
        repo.get_step(&step.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .get("evidence")
            .is_none()
    );
    let first = serde_json::json!({"value":"first"});
    let second = serde_json::json!({"value":"second"});
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    assert!(matches!(
        repo.record_step_metadata_at(&step.id, "evidence", &first, &stamp)
            .await
            .unwrap(),
        boss_jobs::first_record::FirstRecordResult::Recorded(_)
    ));
    assert!(matches!(
        repo.record_step_metadata_at(&step.id, "evidence", &second, &stamp)
            .await
            .unwrap(),
        boss_jobs::first_record::FirstRecordResult::Conflict { .. }
    ));
    assert_eq!(
        repo.get_step(&step.id).await.unwrap().unwrap().metadata["evidence"],
        first,
        "a first recording door must conserve the original evidence"
    );
    let replay = repo
        .record_step_metadata_at(&step.id, "evidence", &first, &stamp)
        .await
        .unwrap();
    let recorded = repo
        .record_step_metadata_at(&step.id, "evidence", &first, &stamp)
        .await
        .unwrap();
    assert_eq!(replay, recorded, "replay returns the original receipt");
    let original = match &replay {
        boss_jobs::first_record::FirstRecordResult::Replayed(record) => record.clone(),
        other => panic!("expected original replay, got {other:?}"),
    };
    assert_eq!(
        repo.first_step_record(&step.id, "evidence").await.unwrap(),
        Some(original.clone())
    );
    assert_eq!(
        repo.first_step_record(&step.id, "absent").await.unwrap(),
        None
    );
    let replacement = serde_json::json!({"evidence":{"value":"replacement"}});
    assert!(
        repo.merge_step_metadata_at(&step.id, replacement.as_object().unwrap(), &stamp)
            .await
            .is_err(),
        "ordinary merge cannot alter a first record"
    );
    let removal = serde_json::json!({"evidence":null});
    assert!(
        repo.merge_step_metadata_at(&step.id, removal.as_object().unwrap(), &stamp)
            .await
            .is_err(),
        "ordinary merge cannot erase a first record"
    );
    let mut rewrite = repo.get_step(&step.id).await.unwrap().unwrap();
    rewrite.metadata = serde_json::json!({});
    assert!(
        repo.update_step(&rewrite).await.is_err(),
        "generic row update cannot erase evidence"
    );
    let before_job = repo.get_job(&job.id).await.unwrap().unwrap();
    let plan = boss_jobs::repin::RepinPlan {
        reprojected: vec![boss_jobs::repin::Reprojected {
            step: rewrite,
            changed: vec!["evidence".into()],
            kept: vec![],
            unskipped: false,
        }],
        inserted: vec![],
    };
    assert!(
        repo.repin_workflow_version_at(&job.id, 99, &plan, &serde_json::json!({"to":99}), &stamp)
            .await
            .is_err()
    );
    assert_eq!(
        repo.get_job(&job.id).await.unwrap().unwrap(),
        before_job,
        "a refused reprojection changes no envelope or history"
    );
    let mut completed = repo.get_step(&step.id).await.unwrap().unwrap();
    completed.status = StepStatus::Completed;
    repo.update_step(&completed).await.unwrap();
    assert_eq!(
        repo.first_step_record(&step.id, "evidence").await.unwrap(),
        Some(original)
    );
    assert_eq!(repo.get_step(&step.id).await.unwrap(), Some(completed));
    assert_eq!(
        repo.record_step_metadata_at(&step.id, "evidence", &first, &stamp)
            .await
            .unwrap(),
        replay
    );
    assert!(matches!(
        repo.record_step_metadata_at(&step.id, "evidence", &second, &stamp)
            .await
            .unwrap(),
        boss_jobs::first_record::FirstRecordResult::Conflict { .. }
    ));
    assert!(matches!(
        repo.record_step_metadata_at(&step.id, "new", &serde_json::Value::Null, &stamp)
            .await
            .unwrap(),
        boss_jobs::first_record::FirstRecordResult::Terminal
    ));
}

async fn claim_cannot_erase_record<R: JobsRepository>(repo: &R) {
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "claim-record"),
        "Conserve every owned namespace",
        "automation:test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
    );
    repo.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Record", 0);
    step.status = StepStatus::Ready;
    repo.add_step(&step).await.unwrap();
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    repo.record_step_metadata_at(
        &step.id,
        boss_jobs::agent_runs::EDGE_KEY,
        &serde_json::json!("original-run"),
        &stamp,
    )
    .await
    .unwrap();
    let before = repo.get_step(&step.id).await.unwrap().unwrap();
    assert!(
        repo.claim_step_at(&step.id, "agent-other", &stamp, &[])
            .await
            .is_err(),
        "a claim cannot erase a namespace owned by an immutable record"
    );
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), before);
}

async fn stale_claim_event_cannot_erase_record<R: JobsRepository>(repo: &R) {
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "stale-claim"),
        "Protect the recorded claim event",
        "automation:test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
    );
    repo.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Record", 0);
    step.status = StepStatus::Ready;
    repo.add_step(&step).await.unwrap();
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let mut stale = step.clone();
    stale.status = StepStatus::Active;
    stale.assignee_id = Some("agent-control".into());
    let stale_event = stamp.event(
        boss_jobs::events::STEP_UPDATED,
        boss_jobs::events::step_state_payload(&stale),
    );
    repo.record_step_metadata_at(&step.id, "evidence", &serde_json::Value::Null, &stamp)
        .await
        .unwrap();
    let before = repo.get_step(&step.id).await.unwrap().unwrap();
    assert!(
        repo.claim_step_at(&step.id, "agent-control", &stamp, &[stale_event])
            .await
            .is_err(),
        "a stale claim event cannot contradict the protected row"
    );
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), before);
    let mut fresh = before;
    fresh.status = StepStatus::Active;
    fresh.assignee_id = Some("agent-control".into());
    repo.claim_step_at(
        &step.id,
        "agent-control",
        &stamp,
        &[stamp.event(
            boss_jobs::events::STEP_UPDATED,
            boss_jobs::events::step_state_payload(&fresh),
        )],
    )
    .await
    .unwrap();
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), fresh);
}

#[tokio::test]
async fn memory_stale_claim_event_refuses_and_fresh_retry_conserves_record() {
    stale_claim_event_cannot_erase_record(&InMemoryJobs::new()).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_stale_claim_event_refuses_and_fresh_retry_conserves_record() {
    let db = boss_testing::TestDb::new().await;
    stale_claim_event_cannot_erase_record(&boss_jobs::PgJobs::new(db.pool.clone())).await;
}

#[tokio::test]
async fn memory_claim_preserves_every_first_record_namespace() {
    claim_cannot_erase_record(&InMemoryJobs::new()).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_claim_preserves_every_first_record_namespace() {
    let db = boss_testing::TestDb::new().await;
    claim_cannot_erase_record(&boss_jobs::PgJobs::new(db.pool.clone())).await;
}

#[test]
fn canonical_record_equality_preserves_scalar_and_encoded_key_identity() {
    let canonical = boss_core::job::canonical_json_bytes;
    assert_eq!(
        canonical(&serde_json::from_str("{\"b\":2,\"a\":1}").unwrap()),
        canonical(&serde_json::json!({"a":1,"b":2}))
    );
    assert_ne!(
        canonical(&serde_json::json!(1)),
        canonical(&serde_json::json!(1.0))
    );
    assert_ne!(
        canonical(&serde_json::json!({"zz":1,"zzz":2})),
        canonical(&serde_json::json!({"zz:1,zzz":2}))
    );
    assert_ne!(
        canonical(&serde_json::json!([1, 2])),
        canonical(&serde_json::json!([2, 1]))
    );
    assert_eq!(
        canonical(&serde_json::json!({"é":null})),
        canonical(&serde_json::from_str("{\"\\u00e9\":null}").unwrap())
    );
}

#[tokio::test]
async fn in_memory_first_record_conserves_original_evidence() {
    let repo = InMemoryJobs::new();
    first_record_cannot_be_replaced(&repo).await;
    let events = repo.recorded_events();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == boss_jobs::events::STEP_FIRST_RECORDED)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == boss_jobs::events::STEP_UPDATED)
            .count(),
        1,
        "the first-record write preserves the ordinary metadata state notification"
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_first_record_conserves_original_evidence() {
    let db = boss_testing::TestDb::new().await;
    first_record_cannot_be_replaced(&boss_jobs::PgJobs::new(db.pool.clone())).await;
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        events, 2,
        "all Pg retries and refusals retain exactly the first receipt and ordinary state event"
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_first_receipt_and_exact_value_survive_audit_rebuild() {
    let db = boss_testing::TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "record-rebuild"),
        "Rebuild first evidence",
        "automation:test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
    );
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    repo.create_job_at(
        &job,
        stamp.timestamp,
        &[stamp.event(
            boss_jobs::events::JOB_CREATED,
            serde_json::to_value(&job).unwrap(),
        )],
    )
    .await
    .unwrap();
    let mut step = Step::new(job.id, "task", "Record", 0);
    step.status = StepStatus::Active;
    repo.add_step_at(
        &step,
        stamp.timestamp,
        &[stamp.event(
            boss_jobs::events::STEP_CREATED,
            serde_json::to_value(&step).unwrap(),
        )],
    )
    .await
    .unwrap();
    let value =
        serde_json::json!({"floating":1.0,"integer":1,"é":null,"encoded:key,":[true,false]});
    let original = repo
        .record_step_metadata_at(&step.id, "evidence", &value, &stamp)
        .await
        .unwrap();
    let before = repo.get_step(&step.id).await.unwrap().unwrap();
    let bus: std::sync::Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    boss_jobs::rebuild::rebuild_jobs_and_steps(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        repo.get_step(&step.id).await.unwrap().unwrap(),
        before,
        "rebuild conserves complete step state"
    );
    let replay = repo
        .record_step_metadata_at(&step.id, "evidence", &value, &stamp)
        .await
        .unwrap();
    let boss_jobs::first_record::FirstRecordResult::Recorded(original) = original else {
        panic!("initial record missing")
    };
    let boss_jobs::first_record::FirstRecordResult::Replayed(replay) = replay else {
        panic!("rebuilt receipt missing")
    };
    assert_eq!(
        original, replay,
        "receipt identity, scalar spelling and provenance survive rebuild"
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_rebuild_refuses_corrupt_receipt_and_later_erasure_atomically() {
    for corruption in ["receipt", "later-erasure"] {
        let db = boss_testing::TestDb::new().await;
        let repo = boss_jobs::PgJobs::new(db.pool.clone());
        let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
        let job = Job::new(
            "user-feedback",
            Subject::new("custom", "negative-replay"),
            "Refuse contradictory replay",
            "automation:test",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
        );
        repo.create_job_at(
            &job,
            stamp.timestamp,
            &[stamp.event(
                boss_jobs::events::JOB_CREATED,
                serde_json::to_value(&job).unwrap(),
            )],
        )
        .await
        .unwrap();
        let step = Step::new(job.id, "task", "Record", 0);
        repo.add_step_at(
            &step,
            stamp.timestamp,
            &[stamp.event(
                boss_jobs::events::STEP_CREATED,
                serde_json::to_value(&step).unwrap(),
            )],
        )
        .await
        .unwrap();
        let value = serde_json::json!({"complete":[1,null,true]});
        let original = repo
            .record_step_metadata_at(&step.id, "evidence", &value, &stamp)
            .await
            .unwrap();
        let before = repo.get_step(&step.id).await.unwrap().unwrap();
        if corruption == "receipt" {
            // Corrupt the unrelayed fixture, never the immutable audit log.
            sqlx::query("UPDATE event_outbox SET payload=jsonb_set(payload,'{record,value_json}','\"null\"'::jsonb) WHERE kind=$1")
                .bind(boss_jobs::events::STEP_FIRST_RECORDED).execute(&db.pool).await.unwrap();
        } else {
            let mut erased = before.clone();
            erased.metadata = serde_json::json!({});
            let mut later = EventStamp::new("jobs", ActorId::Automation("test".into()));
            later.timestamp = stamp.timestamp + chrono::Duration::seconds(1);
            repo.record_events(&[later.event(
                boss_jobs::events::STEP_UPDATED,
                boss_jobs::events::step_state_payload(&erased),
            )])
            .await
            .unwrap();
        }
        let bus: std::sync::Arc<dyn boss_core::port::EventBus> =
            boss_testing::RecordingEventBus::new();
        boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
            .await
            .unwrap();
        assert!(
            boss_jobs::rebuild::rebuild_jobs_and_steps(&db.pool)
                .await
                .is_err(),
            "{corruption} must not become a successful projection"
        );
        assert_eq!(
            repo.get_step(&step.id).await.unwrap().unwrap(),
            before,
            "failed replay rolls back projection replacement"
        );
        let boss_jobs::first_record::FirstRecordResult::Recorded(original) = original else {
            panic!("record missing")
        };
        assert_eq!(
            repo.record_step_metadata_at(&step.id, "evidence", &value, &stamp)
                .await
                .unwrap(),
            boss_jobs::first_record::FirstRecordResult::Replayed(original)
        );
    }
}
