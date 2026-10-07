//! Postgres half of backlog ce8b7d66: a `(kind, version)` pair names
//! ONE protocol forever, so a packet pinned to it runs what it was
//! admitted under.
//!
//! Until this car the Pg allocator took `MAX(version) + 1` over the
//! rows that exist, and a discarded draft's row does not exist — so
//! discarding the newest draft freed its number and the next draft
//! reused it, and every packet already pinned to that pair (an
//! experiment admits to its draft candidate; `boss job convert --to vN`
//! checks no status) silently ran a different protocol. The in-memory
//! adapter's matching contract is pinned in `registry::tests` and
//! `in_memory::tests`; this file proves the Pg adapters agree with it.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobStatus, Priority, Subject};
use boss_jobs::port::JobsRepository;
use boss_jobs::registry::{
    PgWorkflows, StepSpec, Terminal, WorkflowError, WorkflowRegistry, WorkflowSpec, WorkflowStatus,
};
use boss_testing::TestDb;
use chrono::NaiveDate;

fn spec(kind: &str) -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        kind,
        "Intake Review",
        "platform",
        vec!["account".into()],
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

fn author() -> ActorId {
    ActorId::Human("emp-cto".into())
}

fn packet(kind: &str, version: i32, status: JobStatus) -> Job {
    let mut job = Job::new(
        kind,
        Subject::new("account", "acct-1"),
        format!("{kind} packet"),
        "emp-1",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
    )
    .with_workflow_version(version);
    job.status = status;
    job
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_a_discarded_version_number_is_never_reused() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    let now = chrono::Utc::now();

    let v1 = registry
        .create_draft(spec("intake-review"), &author(), now)
        .await
        .expect("draft v1");
    registry
        .discard_draft("intake-review", v1.version, &author(), now)
        .await
        .expect("discard v1");
    let next = registry
        .create_draft(spec("intake-review"), &author(), now)
        .await
        .expect("draft after discard");
    assert_eq!(
        next.version, 2,
        "v1 was discarded, so the next draft must be v2, never v1 again"
    );

    // The authored-publish path allocates too, and must skip it as well.
    registry
        .discard_draft("intake-review", next.version, &author(), now)
        .await
        .expect("discard v2");
    let authored = registry
        .publish_authored(
            spec("intake-review"),
            boss_core::job::JobId::new(),
            &author(),
            now,
        )
        .await
        .expect("authored publish");
    assert_eq!(authored.version, 3, "v1 and v2 are spent; the next is v3");

    // A discarded number reads as gone, not as a row.
    assert!(
        registry.get_version("intake-review", 1).await.is_err(),
        "a discarded version is not served"
    );
}

/// The pin count the discard route refuses on: EVERY packet pinned to
/// the pair, closed ones included — a closed packet ran under that
/// text, and its record must go on reading it.
#[tokio::test(flavor = "multi_thread")]
async fn pg_the_pin_count_names_every_packet_on_the_pair() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let open = packet("intake-review", 4, JobStatus::Open);
    let closed = packet("intake-review", 4, JobStatus::Closed);
    repo.create_job(&open).await.expect("open packet");
    repo.create_job(&closed).await.expect("closed packet");
    repo.create_job(&packet("intake-review", 3, JobStatus::Open))
        .await
        .expect("other version");
    repo.create_job(&packet("exit-review", 4, JobStatus::Open))
        .await
        .expect("other kind");

    let pinned = repo
        .jobs_pinned_to_workflow("intake-review", 4)
        .await
        .expect("pin count");
    assert_eq!(pinned.count, 2);
    let first = pinned.first.expect("a packet is named");
    let lowest = if open.id.to_string() < closed.id.to_string() {
        open.id
    } else {
        closed.id
    };
    assert_eq!(
        first, lowest,
        "the named packet is the lowest id, as in-memory"
    );

    let none = repo
        .jobs_pinned_to_workflow("intake-review", 9)
        .await
        .expect("empty pin count");
    assert_eq!(none.count, 0);
    assert!(none.first.is_none());
}

/// THE PIN CHECK DECIDES INSIDE THE DISCARD (backlog ce8b7d66). The
/// route asked the jobs port first and the registry deleted in a
/// transaction of its own, so a packet pinned between the two was
/// orphaned. The adapter now counts the pinned packets in the
/// discard's own transaction, under a lock on the draft row, so a
/// caller that reaches the registry without the route — as this test
/// does — is refused the same way, and nothing is spent.
#[tokio::test(flavor = "multi_thread")]
async fn pg_the_discard_itself_refuses_a_draft_a_packet_is_pinned_to() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let now = chrono::Utc::now();
    let d = registry
        .create_draft(spec("intake-review"), &author(), now)
        .await
        .expect("draft");
    let pinned = packet("intake-review", d.version, JobStatus::Open);
    repo.create_job(&pinned).await.expect("pinned packet");

    match registry
        .discard_draft("intake-review", d.version, &author(), now)
        .await
    {
        Err(WorkflowError::Conflict(m)) => {
            assert!(m.contains("1 packet is pinned"), "names the count: {m}");
            assert!(m.contains(&pinned.id.to_string()), "names it: {m}");
        }
        other => panic!("a pinned draft must refuse its discard, got {other:?}"),
    }
    let kept = registry
        .get_version("intake-review", d.version)
        .await
        .expect("a refused discard removes nothing");
    assert_eq!(kept.status, WorkflowStatus::Draft);
    let next = registry
        .create_draft(spec("intake-review"), &author(), now)
        .await
        .expect("next draft");
    assert_eq!(
        next.version,
        d.version + 1,
        "a refused discard spends nothing either"
    );
}

/// BY DECISION (backlog ce8b7d66, an engineering call on the build): the
/// Pg `get_version` serves a DRAFT row, because an experiment's
/// candidate is a draft until a promote publishes it and admission
/// reads it here. The guard is at the doors that pin: a split onto an
/// unpublished version takes Publish on `workflow`, and `boss job
/// convert` refuses a draft target.
#[tokio::test(flavor = "multi_thread")]
async fn pg_get_version_serves_a_draft() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    let d = registry
        .create_draft(spec("intake-review"), &author(), chrono::Utc::now())
        .await
        .expect("draft");
    let served = registry
        .get_version("intake-review", d.version)
        .await
        .expect("draft is served");
    assert_eq!(served.status, WorkflowStatus::Draft);
}

/// How many `jobs.kind.published` facts the outbox holds.
async fn published_events(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'jobs.kind.published'")
        .fetch_one(pool)
        .await
        .expect("count published events")
}

/// Wait until some session of this database is blocked on a row lock —
/// the moment the racing writer has reached the lock the test holds.
async fn until_a_session_waits_on_a_lock(pool: &sqlx::PgPool) {
    for _ in 0..500 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity
             WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(pool)
        .await
        .expect("read pg_stat_activity");
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the racing publish never reached the draft row's lock");
}

/// The real discard must wait on the broad publication fence BEFORE
/// taking a row lock. Reversing those locks can deadlock a publisher.
#[tokio::test(flavor = "multi_thread")]
async fn pg_real_discard_waits_on_publication_fence_before_locking_a_draft() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    let now = chrono::Utc::now();
    let draft = registry
        .create_draft(spec("discard-fence"), &author(), now)
        .await
        .expect("draft");
    let published_before = published_events(&db.pool).await;
    let mut publication = db.pool.begin().await.expect("publication tx");
    sqlx::query("LOCK TABLE workflows, workflow_discarded_versions IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *publication)
        .await
        .expect("publication fence");
    let discarder = PgWorkflows::for_fixture(db.pool.clone());
    let discard = tokio::spawn(async move {
        discarder
            .discard_draft("discard-fence", draft.version, &author(), now)
            .await
    });
    until_a_session_waits_on_a_lock(&db.pool).await;
    let mut probe = db.pool.begin().await.expect("probe tx");
    let row =
        sqlx::query("SELECT version FROM workflows WHERE kind = 'discard-fence' FOR UPDATE NOWAIT")
            .fetch_optional(&mut *probe)
            .await;
    assert!(
        row.is_ok(),
        "discard took the draft row before the publication fence: {row:?}"
    );
    assert!(
        row.unwrap().is_some(),
        "blocked discard must not delete the draft"
    );
    probe.rollback().await.expect("release row probe");
    publication
        .rollback()
        .await
        .expect("release publication fence");
    tokio::time::timeout(std::time::Duration::from_secs(10), discard)
        .await
        .expect("discard completes after fence release")
        .expect("discard task")
        .expect("discard succeeds after fence");
    assert_eq!(published_events(&db.pool).await, published_before);
    let spent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_discarded_versions WHERE kind = 'discard-fence'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("spent number");
    assert_eq!(spent, 1);
}

/// A PUBLISH THAT LOSES ITS DRAFT TO A DISCARD REFUSES, AND RECORDS
/// NOTHING (the review of car 06973644, 2026-09-28, finding A). The
/// publish read its draft WITHOUT a lock, retired the active row, then
/// flipped the draft by `(kind, version)` without counting what the
/// flip touched. A discard holding the draft's row lock deleted it
/// underneath: the flip updated 0 rows, the publish committed anyway,
/// and the kind was left with NO active version — beside a
/// `jobs.kind.published` fact for a row that no longer exists.
///
/// The discard is played here by a transaction of the test's own that
/// does what `discard_draft` does — broad table fence, `FOR UPDATE`, then
/// DELETE, then the spent number — because the real one cannot be
/// paused between its lock and its delete. The publish starts while
/// those locks are held and is observed WAITING on the fence before the delete
/// commits, so the interleaving is the one the review walked, every
/// run, not a timing hope.
#[tokio::test(flavor = "multi_thread")]
async fn pg_a_publish_that_loses_its_draft_to_a_discard_refuses_and_records_nothing() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    let now = chrono::Utc::now();

    let v1 = registry
        .create_draft(spec("intake-review"), &author(), now)
        .await
        .expect("draft v1");
    registry
        .publish("intake-review", &author(), now)
        .await
        .expect("publish v1");
    let v2 = registry
        .create_draft(spec("intake-review"), &author(), now)
        .await
        .expect("draft v2");
    let published_before = published_events(&db.pool).await;

    // Same lock order as the real discard: fence, then draft row.
    let mut discard = db.pool.begin().await.expect("discard tx");
    sqlx::query("LOCK TABLE workflows, workflow_discarded_versions IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *discard)
        .await
        .expect("discard publication fence");
    sqlx::query("SELECT version FROM workflows WHERE kind = $1 AND version = $2 FOR UPDATE")
        .bind("intake-review")
        .bind(v2.version)
        .fetch_one(&mut *discard)
        .await
        .expect("lock the draft");

    // The publish races it, and waits on the discard's table fence.
    let racing = PgWorkflows::for_fixture(db.pool.clone());
    let publish =
        tokio::spawn(async move { racing.publish("intake-review", &author(), now).await });
    until_a_session_waits_on_a_lock(&db.pool).await;

    // The discard deletes the draft, spends its number, and commits.
    sqlx::query("DELETE FROM workflows WHERE kind = $1 AND version = $2 AND status = 'draft'")
        .bind("intake-review")
        .bind(v2.version)
        .execute(&mut *discard)
        .await
        .expect("delete the draft");
    sqlx::query(
        "INSERT INTO workflow_discarded_versions (kind, version, discarded_at, discarded_by)
         VALUES ($1, $2, $3, 'emp-cto')",
    )
    .bind("intake-review")
    .bind(v2.version)
    .bind(now)
    .execute(&mut *discard)
    .await
    .expect("spend the number");
    discard.commit().await.expect("discard commits");

    // NotFound, and only NotFound: publication waited on the common fence
    // and then read the draft as the discard left it — gone.
    // A Conflict here would mean the lock was lost and only the one-row
    // flip check (the documented backstop) caught the race.
    let answer = publish.await.expect("publish task");
    assert!(
        matches!(answer, Err(WorkflowError::NotFound(_))),
        "a publish whose draft was discarded under it must find nothing to publish, got {answer:?}"
    );
    let active = registry
        .get_active("intake-review")
        .await
        .expect("the kind still has an active version");
    assert_eq!(
        active.version, v1.version,
        "the refused publish retired nothing: v1 is still the live protocol"
    );
    assert_eq!(
        published_events(&db.pool).await,
        published_before,
        "no jobs.kind.published fact for a row that does not exist"
    );
}
