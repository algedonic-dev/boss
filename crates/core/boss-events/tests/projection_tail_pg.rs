use boss_events::projection_tail::{self, Selection};
use boss_events::statement_budget::{
    FIXTURE_BUDGET, MEASURED_STALL, StatementBudget, after_the_measured_stall,
};
use boss_testing::TestDb;

// Every read here states its budget (backlog ebbf38c6). The tail took a
// literal 2 s until then, so a test judging what it SELECTS lost to a
// loaded gate drive instead; these take a fold fixture's budget from a
// clock that has already lost the measured stall, and the two tests at
// the end judge the budget itself. The stall and the fixture budget are
// the crate's own, spelled once in `statement_budget` (backlog 03a5dbdc).

/// A budget the stall has spent: one millisecond short of it, the sharpest
/// there is. This was a copy of production's 2 s until backlog 03a5dbdc,
/// and it is not one now on purpose: the 2 s is the claims history's in
/// boss-jobs, which depends on this crate, so no test here can read it,
/// and this crate's reads own no production budget of their own — they
/// spend the one their caller hands them. What the refusal needs is a
/// budget shorter than the stall, and boss-jobs' fixture_budget holds
/// every production budget it has shorter than the stall at compile
/// time, so each of them refuses as this one does.
const SPENT_BUDGET: std::time::Duration =
    MEASURED_STALL.saturating_sub(std::time::Duration::from_millis(1));

/// `budget`, measured from a clock that has already lost the stall.
fn after_the_stall(budget: std::time::Duration) -> StatementBudget {
    StatementBudget::until(after_the_measured_stall() + budget)
}
fn fixture() -> StatementBudget {
    after_the_stall(FIXTURE_BUDGET)
}
fn one_fact_selection() -> Selection<'static> {
    Selection {
        source: "jobs",
        kinds: &["jobs.pool.published"],
        payload_keys: &["name"],
        metadata_keys: &[],
        witness_keys: &[],
    }
}
async fn one_fact(db: &TestDb) {
    sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,now(),'jobs','jobs.pool.published','{\"name\":\"vcpu\"}'::jsonb)")
        .bind(uuid::Uuid::new_v4()).execute(&db.pool).await.unwrap();
}

/// The seam: a read on the fixture budget answers after a stall longer
/// than production's whole 2 s, so what it returns is the read's and never
/// the gate drive's.
#[tokio::test]
async fn a_tail_read_on_a_fixture_budget_outlasts_a_stall_past_production() {
    let db = TestDb::new().await;
    one_fact(&db).await;
    let tail = projection_tail::read(&db.pool, 0, 1024, one_fact_selection(), fixture())
        .await
        .unwrap();
    assert_eq!(tail.facts.len(), 1);
}

/// The guarded refusal, deterministically: the same read on a budget the
/// stall has spent (production's 2 s is one, held so by boss-jobs), from a
/// clock that has lost the stall, refuses by name before it reads
/// anything. It fails if the read stops spending the budget it is handed,
/// which is the defect it was (backlog ebbf38c6).
#[tokio::test]
async fn a_tail_read_whose_budget_the_stall_has_spent_refuses_by_name() {
    let db = TestDb::new().await;
    one_fact(&db).await;
    let refused = projection_tail::read(
        &db.pool,
        0,
        1024,
        one_fact_selection(),
        after_the_stall(SPENT_BUDGET),
    )
    .await;
    match refused {
        Err(why) => assert!(why.contains("pre-commit statement budget expired"), "{why}"),
        Ok(tail) => panic!(
            "a spent budget must refuse, not read {} fact(s)",
            tail.facts.len()
        ),
    }
}

#[tokio::test]
async fn a_tail_never_advances_through_a_partial_original_producer_group() {
    use boss_core::{event::Event, port::EventBus};
    use boss_events::outbox::{drain_outbox_once, record_event_in_tx};
    let db = TestDb::new().await;
    let job = uuid::Uuid::new_v4();
    let a = Event::new(
        "jobs",
        "jobs.job.created",
        serde_json::json!({"id":job,"status":"open"}),
        chrono::Utc::now(),
    );
    let b = Event::new(
        "jobs",
        "jobs.job.updated",
        serde_json::json!({"id":job,"status":"closed"}),
        chrono::Utc::now(),
    );
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &a).await.unwrap();
    record_event_in_tx(&mut tx, &b).await.unwrap();
    tx.commit().await.unwrap();
    let bus: std::sync::Arc<dyn EventBus> = boss_testing::RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 1).await.unwrap();
    let read = || Selection {
        source: "jobs",
        kinds: &["jobs.job.created", "jobs.job.updated"],
        payload_keys: &["id", "status"],
        metadata_keys: &[],
        witness_keys: &[],
    };
    assert!(
        projection_tail::read(&db.pool, 0, 1024, read(), fixture())
            .await
            .is_err(),
        "a visible prefix of one producer transaction is not complete"
    );
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let all = projection_tail::read(&db.pool, 0, 1024, read(), fixture())
        .await
        .unwrap();
    assert_eq!(all.facts.len(), 2);
    let group = all.facts[0].original_group.as_ref().unwrap();
    assert!(group.validate(all.through).unwrap());
    assert_eq!(group.event_ids, vec![a.id, b.id]);
    assert_eq!(
        all.facts[1].original_group.as_ref().unwrap().reference.id,
        group.reference.id
    );
    assert!(
        projection_tail::read(&db.pool, 0, 1, read(), fixture())
            .await
            .is_err(),
        "a batch bound cannot split the original group"
    );
}

#[tokio::test]
async fn a_native_tail_preserves_raw_decimal_text_and_progress_across_unrelated_facts() {
    let db = TestDb::new().await;
    let first = uuid::Uuid::new_v4();
    let unrelated = uuid::Uuid::new_v4();
    let raw = r#"{"name":"vcpu","capacity":{"state":"configured","amount":0.1234567890123456789012345678},"metadata":{"waiting_on":{"resource":"vcpu"},"private":"not-a-projection-field"}}"#;
    sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,now(),'jobs','jobs.pool.published',$2::jsonb)")
        .bind(first).bind(raw).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,now(),'other','jobs.job.updated','{\"unrelated\":\"not-needed\"}'::jsonb)")
        .bind(unrelated).execute(&db.pool).await.unwrap();
    let read = || Selection {
        source: "jobs",
        kinds: &["jobs.pool.published"],
        payload_keys: &["name", "capacity", "metadata"],
        metadata_keys: &["waiting_on"],
        witness_keys: &[],
    };
    let batch = projection_tail::read(&db.pool, 0, 1, read(), fixture())
        .await
        .unwrap();
    assert_eq!(batch.facts.len(), 1);
    assert_eq!(batch.facts[0].event_id, first);
    assert!(
        batch.facts[0]
            .payload
            .contains("0.1234567890123456789012345678"),
        "raw source must not round through f64"
    );
    assert!(!batch.facts[0].payload.contains("not-a-projection-field"));
    assert!(
        batch.observed_head > batch.through,
        "the prefix must expose its measured remaining work"
    );
    let next = projection_tail::read(&db.pool, batch.through, 1, read(), fixture())
        .await
        .unwrap();
    assert_eq!(next.facts[0].event_id, unrelated);
    assert_eq!(
        next.facts[0].payload, "{}",
        "unrelated rows carry sequence/identity, not whole foreign payloads"
    );
    assert_eq!(next.through, next.observed_head);
    assert!(
        projection_tail::read(&db.pool, next.through, 0, read(), fixture())
            .await
            .is_err()
    );
    assert!(
        projection_tail::read(&db.pool, next.through + 1, 1, read(), fixture())
            .await
            .is_err()
    );
}
