//! First binding authority is enforced by persistence, even without HTTP.
use boss_core::{
    actor::ActorId,
    job::{Job, JobStatus, Priority, Subject},
    publisher::EventStamp,
};
use boss_jobs::{InMemoryJobs, JobsRepository};
use serde_json::json;

fn protocol() -> boss_jobs::registry::WorkflowSpec {
    use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
    let mut spec = WorkflowSpec::platform_seed(
        "user-feedback",
        "Guarded fixture",
        "platform",
        vec!["custom".into()],
        vec![StepSpec {
            title: "done".into(),
            kind: "outcome".into(),
            ready_when: "true".into(),
            terminal: Some(Terminal {
                outcome: "completed".into(),
            }),
            metadata_defaults: json!({"outcome_kind":"completed"}),
            ..Default::default()
        }],
    );
    spec.metadata_schema = json!({"guarded_fields":["attempt"]});
    spec
}

fn first_command() -> boss_jobs::conditional_metadata::Command {
    serde_json::from_value(json!({"contract_version":2,"idempotency_key":uuid::Uuid::new_v4(),
        "expected":{"attempt":{"present":false}},"patch":{"attempt":{"state":"sending"}},"guarded_keys":["attempt"]})).unwrap()
}

async fn binding_contract(jobs: &dyn JobsRepository) {
    let mut job = Job::new(
        "user-feedback",
        Subject::new("custom", "fixture"),
        "Guard",
        "agent-test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
    );
    job.status = JobStatus::Open;
    job.metadata = json!({"ordinary":true});
    jobs.create_job(&job).await.unwrap();
    let raw = jobs
        .merge_job_metadata_at(
            &job.id,
            json!({"attempt":{"state":"sending"}}).as_object().unwrap(),
            None,
            &EventStamp::new("jobs", ActorId::Automation("test".into())),
        )
        .await;
    assert!(
        raw.is_err(),
        "an ordinary persistence writer cannot bypass the pinned first-binding door: {raw:?}"
    );
    let mut raw_create = job.clone();
    raw_create.id = boss_core::job::JobId::from_uuid(uuid::Uuid::new_v4());
    raw_create.metadata["attempt"] = json!({"state":"sending"});
    assert!(
        jobs.create_job(&raw_create).await.is_err(),
        "ordinary admission cannot introduce a declared protected field"
    );
    let mut raw_put = job.clone();
    raw_put.metadata["attempt"] = json!({"state":"sending"});
    assert!(
        jobs.update_job_at(&raw_put, JobStatus::Open, chrono::Utc::now(), &[])
            .await
            .is_err(),
        "ordinary whole-state writes cannot introduce a declared protected field"
    );
    for value in [json!({"state":"sending"}), serde_json::Value::Null] {
        let close = jobs
            .close_job_at(
                &job.id,
                job.opened_on,
                json!({"attempt":value}).as_object().unwrap(),
                &EventStamp::new("jobs", ActorId::Automation("test".into())),
                &|_| vec![],
            )
            .await;
        assert!(
            close.is_err(),
            "closing does not authorize first insertion of a protected field: {close:?}"
        );
    }
    let command = first_command();
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let original = jobs
        .conditional_job_metadata_at(&job.id, &command, None, &stamp)
        .await
        .unwrap();
    assert_eq!(original.job.metadata["attempt"]["state"], "sending");
    let before = jobs.get_job(&job.id).await.unwrap().unwrap();
    for patch in [
        json!({"attempt":null}),
        json!({"attempt":{"state":"not_started"}}),
        json!({"_guarded_metadata":null}),
    ] {
        assert!(
            jobs.merge_job_metadata_at(&job.id, patch.as_object().unwrap(), None, &stamp)
                .await
                .is_err()
        );
        assert_eq!(jobs.get_job(&job.id).await.unwrap().unwrap(), before);
    }
    let next=serde_json::from_value(json!({"contract_version":2,"idempotency_key":uuid::Uuid::new_v4(),
        "expected":{"attempt":{"present":true,"value":{"state":"sending"}}},"patch":{"attempt":{"state":"not_started"}}})).unwrap();
    let after = jobs
        .conditional_job_metadata_at(&job.id, &next, None, &stamp)
        .await
        .unwrap();
    assert_eq!(after.job.metadata["attempt"]["state"], "not_started");
    assert_eq!(
        jobs.conditional_job_metadata_at(&job.id, &command, None, &stamp)
            .await
            .unwrap(),
        original
    );

    // THE HISTORY a gate attempt id is judged fresh against (backlog
    // 0471152b): every receipt this packet retains, every actor's, and
    // no other packet's.
    let other = EventStamp::new("jobs", ActorId::Automation("other".into()));
    let third: boss_jobs::conditional_metadata::Command = serde_json::from_value(json!({"contract_version":2,"idempotency_key":uuid::Uuid::new_v4(),
        "expected":{"attempt":{"present":true,"value":{"state":"not_started"}}},"patch":{"attempt":{"state":"sending","by":"other"}}})).unwrap();
    jobs.conditional_job_metadata_at(&job.id, &third, None, &other)
        .await
        .unwrap();
    let mut elsewhere = job.clone();
    elsewhere.id = boss_core::job::JobId::from_uuid(uuid::Uuid::new_v4());
    jobs.create_job(&elsewhere).await.unwrap();
    let unrelated: boss_jobs::conditional_metadata::Command = serde_json::from_value(
        json!({"contract_version":2,"idempotency_key":uuid::Uuid::new_v4(),
        "expected":{"ordinary":{"present":true,"value":true}},"patch":{"ordinary":false}}),
    )
    .unwrap();
    jobs.conditional_job_metadata_at(&elsewhere.id, &unrelated, None, &stamp)
        .await
        .unwrap();
    let mut history: Vec<_> = jobs
        .conditional_job_metadata_receipts(&job.id)
        .await
        .unwrap()
        .into_iter()
        .map(|receipt| (receipt.actor, receipt.command.idempotency_key))
        .collect();
    history.sort();
    let mut want = vec![
        (stamp.actor().to_string(), command.idempotency_key),
        (stamp.actor().to_string(), next.idempotency_key),
        (other.actor().to_string(), third.idempotency_key),
    ];
    want.sort();
    assert_eq!(history, want);
}

#[tokio::test]
async fn memory_enforces_the_pinned_declaration_and_preserves_the_original_receipt() {
    let registry = std::sync::Arc::new(boss_jobs::registry::InMemoryWorkflows::new());
    registry.seed(protocol()).unwrap();
    let jobs = InMemoryJobs::new().with_workflows(registry);
    binding_contract(&jobs).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_enforces_the_pinned_declaration_inside_the_transaction() {
    use boss_jobs::registry::WorkflowRegistry;
    let db = boss_testing::TestDb::new().await;
    let registry = boss_jobs::registry::PgWorkflows::new(db.pool.clone());
    let result = registry
        .bootstrap_reconcile(
            &[protocol()],
            &ActorId::Automation("test".into()),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(result.rejected, 0);
    let jobs = boss_jobs::PgJobs::new(db.pool.clone());
    binding_contract(&jobs).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_real_relay_and_rebuild_conserve_guarded_commands_after_repin() {
    use boss_jobs::registry::WorkflowRegistry;
    let db = boss_testing::TestDb::new().await;
    let registry = boss_jobs::registry::PgWorkflows::new(db.pool.clone());
    assert_eq!(
        registry
            .bootstrap_reconcile(
                &[protocol()],
                &ActorId::Automation("test".into()),
                chrono::Utc::now()
            )
            .await
            .unwrap()
            .rejected,
        0
    );
    let jobs = boss_jobs::PgJobs::new(db.pool.clone());
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let mut job = Job::new(
        "user-feedback",
        Subject::new("custom", "fixture"),
        "Replay",
        "agent-test",
        Priority::Standard,
        stamp.timestamp.date_naive(),
    );
    job.status = JobStatus::Open;
    job.opened_at = Some(stamp.timestamp);
    job.metadata = json!({"ordinary":true});
    let admission = stamp.event("jobs.job.created", serde_json::to_value(&job).unwrap());
    jobs.create_job_at(&job, stamp.timestamp, &[admission])
        .await
        .unwrap();
    let first = first_command();
    let original = jobs
        .conditional_job_metadata_at(&job.id, &first, None, &stamp)
        .await
        .unwrap();
    jobs.repin_workflow_version_at(
        &job.id,
        2,
        &boss_jobs::repin::RepinPlan {
            reprojected: vec![],
            inserted: vec![],
        },
        &json!({"from":1,"to":2}),
        &stamp,
    )
    .await
    .unwrap();
    let next=serde_json::from_value(json!({"contract_version":2,"idempotency_key":uuid::Uuid::new_v4(),
        "expected":{"attempt":{"present":true,"value":{"state":"sending"}}},"patch":{"attempt":{"state":"not_started"}}})).unwrap();
    let updated = jobs
        .conditional_job_metadata_at(&job.id, &next, None, &stamp)
        .await
        .unwrap();
    let before = jobs.get_job(&job.id).await.unwrap().unwrap();
    let bus: std::sync::Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 200)
        .await
        .unwrap();
    for _ in 0..2 {
        boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
        assert_eq!(jobs.get_job(&job.id).await.unwrap().unwrap(), before);
        assert_eq!(
            jobs.conditional_job_metadata_at(&job.id, &first, None, &stamp)
                .await
                .unwrap(),
            original
        );
        assert_eq!(
            jobs.conditional_job_metadata_at(&job.id, &next, None, &stamp)
                .await
                .unwrap(),
            updated
        );
    }
    let mut forged = before.clone();
    forged.metadata.as_object_mut().unwrap().remove("attempt");
    forged
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("_guarded_metadata");
    let event = stamp.event("jobs.job.updated", serde_json::to_value(forged).unwrap());
    let mut tx = db.pool.begin().await.unwrap();
    boss_events::outbox::record_event_in_tx(&mut tx, &event)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 200)
        .await
        .unwrap();
    assert!(
        boss_jobs::rebuild_jobs_and_steps(&db.pool).await.is_err(),
        "unpaired later state cannot erase the guarded attempt on replay"
    );
    assert_eq!(
        jobs.get_job(&job.id).await.unwrap().unwrap(),
        before,
        "rebuild refusal rolls back its wipe"
    );
    assert_eq!(
        jobs.conditional_job_metadata_at(&job.id, &first, None, &stamp)
            .await
            .unwrap(),
        original
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn row_receipt_both_outbox_and_commit_failures_roll_back_the_whole_command() {
    for phase in ["row", "receipt", "state-event", "receipt-event", "commit"] {
        let db = boss_testing::TestDb::new().await;
        let jobs = boss_jobs::PgJobs::new(db.pool.clone());
        let mut job = Job::new(
            "user-feedback",
            Subject::new("custom", "fixture"),
            "Atomic",
            "agent-test",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
        );
        job.status = JobStatus::Open;
        job.metadata = json!({"ordinary":true});
        jobs.create_job(&job).await.unwrap();
        let function = match phase {
            "state-event" => {
                "BEGIN IF NEW.kind='jobs.job.updated' THEN RAISE EXCEPTION 'controlled state failure'; END IF; RETURN NEW; END"
            }
            "receipt-event" => {
                "BEGIN IF NEW.kind='jobs.metadata.conditionally_merged' THEN RAISE EXCEPTION 'controlled receipt event failure'; END IF; RETURN NEW; END"
            }
            _ => "BEGIN RAISE EXCEPTION 'controlled command failure'; RETURN NEW; END",
        };
        sqlx::query(&format!("CREATE FUNCTION fail_conditional_fixture() RETURNS trigger LANGUAGE plpgsql AS $body$ {function} $body$"))
            .execute(&db.pool).await.unwrap();
        let trigger = match phase {
            "row" => {
                "CREATE TRIGGER fail_conditional BEFORE UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION fail_conditional_fixture()"
            }
            "receipt" => {
                "CREATE TRIGGER fail_conditional BEFORE INSERT ON job_metadata_receipts FOR EACH ROW EXECUTE FUNCTION fail_conditional_fixture()"
            }
            "commit" => {
                "CREATE CONSTRAINT TRIGGER fail_conditional AFTER INSERT ON job_metadata_receipts DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fail_conditional_fixture()"
            }
            _ => {
                "CREATE TRIGGER fail_conditional BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION fail_conditional_fixture()"
            }
        };
        sqlx::query(trigger).execute(&db.pool).await.unwrap();
        let command = serde_json::from_value(
            json!({"contract_version":2,"idempotency_key":uuid::Uuid::new_v4(),
            "expected":{"ordinary":{"present":true,"value":true}},"patch":{"ordinary":false}}),
        )
        .unwrap();
        let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
        assert!(
            jobs.conditional_job_metadata_at(&job.id, &command, None, &stamp)
                .await
                .is_err(),
            "{phase}"
        );
        assert_eq!(
            jobs.get_job(&job.id).await.unwrap().unwrap(),
            job,
            "{phase}"
        );
        assert!(
            jobs.conditional_job_metadata_receipt(
                &job.id,
                &stamp.actor().to_string(),
                command.idempotency_key
            )
            .await
            .unwrap()
            .is_none(),
            "{phase}"
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "{phase} must not leave half a fact");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_postgres_first_claims_commit_one_pair_and_retries_return_one_original() {
    use boss_jobs::registry::WorkflowRegistry;
    let db = boss_testing::TestDb::new().await;
    let registry = boss_jobs::registry::PgWorkflows::new(db.pool.clone());
    assert_eq!(
        registry
            .bootstrap_reconcile(
                &[protocol()],
                &ActorId::Automation("test".into()),
                chrono::Utc::now()
            )
            .await
            .unwrap()
            .rejected,
        0
    );
    let jobs = boss_jobs::PgJobs::new(db.pool.clone());
    let mut job = Job::new(
        "user-feedback",
        Subject::new("custom", "fixture"),
        "Race",
        "agent-test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
    );
    job.status = JobStatus::Open;
    job.metadata = json!({"ordinary":true});
    jobs.create_job(&job).await.unwrap();
    let a = first_command();
    let b = first_command();
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let (a_result, b_result) = tokio::join!(
        jobs.conditional_job_metadata_decision_at(&job.id, &a, None, &stamp),
        jobs.conditional_job_metadata_decision_at(&job.id, &b, None, &stamp),
    );
    assert_eq!(
        usize::from(a_result.is_ok()) + usize::from(b_result.is_ok()),
        1
    );
    let winner = a_result.or(b_result).unwrap();
    assert!(winner.applied);
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM job_metadata_receipts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let events:i64=sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind IN ('jobs.job.updated','jobs.metadata.conditionally_merged')").fetch_one(&db.pool).await.unwrap();
    assert_eq!(receipts, 1);
    assert_eq!(events, 2);
    let (a_retry, b_retry) = tokio::join!(
        jobs.conditional_job_metadata_decision_at(&job.id, &winner.receipt.command, None, &stamp),
        jobs.conditional_job_metadata_decision_at(&job.id, &winner.receipt.command, None, &stamp),
    );
    for retry in [a_retry.unwrap(), b_retry.unwrap()] {
        assert!(!retry.applied);
        assert_eq!(retry.receipt, winner.receipt);
    }
    let after:i64=sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind IN ('jobs.job.updated','jobs.metadata.conditionally_merged')").fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        after, events,
        "idempotent receipt retries emit no second state/readiness effect"
    );
}

#[tokio::test]
async fn a_repository_cannot_first_bind_an_undeclared_guard_without_a_registry() {
    let jobs = InMemoryJobs::new();
    let mut job = Job::new(
        "fixture",
        Subject::new("custom", "fixture"),
        "Guard",
        "agent-test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
    );
    job.status = JobStatus::Open;
    job.metadata = json!({"ordinary":true});
    jobs.create_job(&job).await.unwrap();
    let command=serde_json::from_value(json!({"contract_version":2,"idempotency_key":uuid::Uuid::new_v4(),
        "expected":{"attempt":{"present":false}},"patch":{"attempt":{"state":"sending"}},"guarded_keys":["attempt"]})).unwrap();
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let result = jobs
        .conditional_job_metadata_at(&job.id, &command, None, &stamp)
        .await;
    assert!(
        result.is_err(),
        "persistence must refuse a first binding without the exact pinned declaration: {result:?}"
    );
    assert_eq!(jobs.get_job(&job.id).await.unwrap().unwrap(), job);
}

/// A gate attempt write: `next` pinned on `pinned` (None pins absence).
fn attempt_write(
    pinned: Option<serde_json::Value>,
    next: serde_json::Value,
) -> boss_jobs::conditional_metadata::Command {
    let key = boss_jobs::gate_reservation::ATTEMPT;
    let expected = match pinned {
        Some(value) => json!({"present": true, "value": value}),
        None => json!({"present": false}),
    };
    serde_json::from_value(
        json!({"contract_version":2,"idempotency_key":uuid::Uuid::new_v4(),
        "expected":{key: expected},"patch":{key: next}}),
    )
    .unwrap()
}

/// WHY (backlog 301b9060, review f6890829 N1/N2 of car 4814539f). The
/// door judged a gate attempt id fresh against a history it read BEFORE
/// the row lock, so a write pinned on a value that did not exist yet
/// could read a history missing W, wait while an accomplice (or itself)
/// committed W and then V naming W, and land W a second time once its
/// compare-and-set on V came true. Raced here for real: the colliding
/// write retries against the chain until the chain is done. W is
/// admitted exactly once whichever way the race falls, because the
/// repository judges freshness under its own row lock; and a replay of
/// the write that DID land W still returns its original receipt, because
/// the replay is judged before freshness (N2: a concurrent duplicate got
/// "already written" instead of its receipt).
async fn a_colliding_attempt_id_is_admitted_once(jobs: &dyn JobsRepository) {
    let key = boss_jobs::gate_reservation::ATTEMPT;
    let mut job = Job::new(
        "gate-run",
        Subject::new("custom", "fixture"),
        "Collide",
        "agent-test",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 22).unwrap(),
    );
    job.status = JobStatus::Open;
    jobs.create_job(&job).await.unwrap();
    let accomplice = EventStamp::new("jobs", ActorId::Automation("accomplice".into()));
    let racer = EventStamp::new("jobs", ActorId::Automation("racer".into()));
    let (u, w, v) = (
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
    );
    let at_u = json!({"id": u, "actor": "accomplice", "state": "not_started"});
    let at_w = json!({"id": w, "actor": "accomplice", "previous": u, "state": "not_started"});
    let at_v = json!({"id": v, "actor": "accomplice", "previous": w, "state": "not_started"});
    jobs.conditional_job_metadata_at(
        &job.id,
        &attempt_write(None, at_u.clone()),
        None,
        &accomplice,
    )
    .await
    .unwrap();
    // The racer pins the value the chain WILL leave and writes W back.
    let colliding = attempt_write(
        Some(at_v.clone()),
        json!({"id": w, "actor": "racer", "previous": v, "state": "sending"}),
    );
    let to_w = attempt_write(Some(at_u), at_w.clone());
    let to_v = attempt_write(Some(at_w.clone()), at_v.clone());
    let chain_done = std::sync::atomic::AtomicBool::new(false);
    let chain = async {
        jobs.conditional_job_metadata_at(&job.id, &to_w, None, &accomplice)
            .await
            .unwrap();
        jobs.conditional_job_metadata_at(&job.id, &to_v, None, &accomplice)
            .await
            .unwrap();
        chain_done.store(true, std::sync::atomic::Ordering::SeqCst);
    };
    let race = async {
        loop {
            let done = chain_done.load(std::sync::atomic::Ordering::SeqCst);
            match jobs
                .conditional_job_metadata_decision_at(&job.id, &colliding, None, &racer)
                .await
            {
                Err(boss_jobs::JobsError::JobChanged { .. }) if !done => {
                    tokio::task::yield_now().await
                }
                settled => return settled,
            }
        }
    };
    let ((), raced) = tokio::join!(chain, race);
    let refusal = raced
        .map(|outcome| outcome.receipt)
        .expect_err("W was admitted a second time")
        .to_string();
    assert!(
        refusal.contains(&format!("{key}.id {w}")) && refusal.contains(&v.to_string()),
        "the refusal names the reused id and the pinned attempt: {refusal}"
    );
    let ids_written: Vec<_> = jobs
        .conditional_job_metadata_receipts(&job.id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|receipt| receipt.command.patch.get(key).cloned())
        .filter(|attempt| attempt["id"] == json!(w))
        .collect();
    assert_eq!(ids_written, vec![at_w], "W was admitted exactly once");
    assert_eq!(
        jobs.get_job(&job.id).await.unwrap().unwrap().metadata[key],
        at_v,
        "the refused write left the row as the chain did"
    );
    let replay = jobs
        .conditional_job_metadata_decision_at(&job.id, &to_w, None, &accomplice)
        .await
        .expect("a replay of the write that landed W returns its receipt");
    assert!(!replay.applied);
    assert_eq!(replay.receipt.command, to_w);
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_admits_a_colliding_attempt_id_once() {
    a_colliding_attempt_id_is_admitted_once(&InMemoryJobs::new()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_admits_a_colliding_attempt_id_once_under_its_row_lock() {
    let db = boss_testing::TestDb::new().await;
    a_colliding_attempt_id_is_admitted_once(&boss_jobs::PgJobs::new(db.pool.clone())).await;
}
