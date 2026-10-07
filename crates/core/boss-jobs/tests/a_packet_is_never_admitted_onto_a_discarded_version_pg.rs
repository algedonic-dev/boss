//! A PACKET IS NEVER ADMITTED ONTO A WORKFLOW VERSION THE DISCARD IS
//! REMOVING — the Postgres half of part 4 of backlog ce8b7d66 (finding
//! E of the review of car 06973644).
//!
//! The discard counts the packets pinned to its draft inside its own
//! transaction, under `FOR UPDATE` on the draft's row (part 1). The
//! admission never looked at that row: the route read the draft through
//! `get_version` (an experiment admits to its draft candidate) and the
//! adapter inserted the packet in a transaction of its own. So a packet
//! whose transaction was still open when the discard counted was
//! invisible to the count, and a packet that began after the discard's
//! delete but before its commit never waited for it — either way it
//! committed pinned to a `(kind, version)` with no protocol, the
//! orphaning part 1 exists to refuse.
//!
//! The admission now takes `FOR KEY SHARE` on the row it pins to, in its
//! own transaction, before it writes anything; and when there is no row
//! it asks `workflow_discarded_versions` whether the number was spent by
//! a discard, and refuses with [`JobsError::VersionDiscarded`]. The two
//! locks conflict, so the two writers are ORDERED: an admission that
//! locks first is counted by the discard, and one that waits reads the
//! discard's verdict when it is let through.
//!
//! Both interleavings run the REAL adapter code on both sides. Each
//! writer is held after its workflow row lock: the discard's committed
//! insert trigger waits on an advisory lock, and the admission waits
//! on an uncommitted subject insert. The discard's pause must own no
//! data-table lock that blocks its preceding publication fence. The second
//! writer is observed WAITING on the first before the hold is released,
//! so the interleaving is the one the review walked, every run. The
//! in-memory adapter's sequential answer is the adapters-agree case
//! `a_packet_is_never_admitted_onto_a_discarded_version`.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobStatus, Priority, Subject};
use boss_jobs::port::JobsError;
use boss_jobs::registry::{
    PgWorkflows, StepSpec, Terminal, WorkflowError, WorkflowRegistry, WorkflowSpec, WorkflowStatus,
};
use boss_jobs::{JobsRepository, PgJobs};
use boss_testing::TestDb;
use chrono::NaiveDate;

const KIND: &str = "intake-review";

fn spec() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        KIND,
        "Intake Review",
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

fn author() -> ActorId {
    ActorId::Human("emp-cto".into())
}

/// A packet pinned to `version`, about a `custom` subject — a kind born
/// by its first Job, so the admission mints its identity row first
/// (the insert the admission-first case holds it on).
fn packet(version: i32, subject: &str) -> Job {
    let mut job = Job::new(
        KIND,
        Subject::new("custom", subject),
        "A packet admitted while its draft is discarded",
        "emp-1",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 9, 29).unwrap(),
    )
    .with_workflow_version(version);
    job.status = JobStatus::Open;
    job
}

/// Sessions of this database blocked on a lock right now.
async fn lock_waiters(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity
         WHERE datname = current_database() AND wait_event_type = 'Lock'",
    )
    .fetch_one(pool)
    .await
    .expect("read pg_stat_activity")
}

/// Wait until `n` sessions wait on a lock, or `racer` has finished —
/// which it does at once when it takes no lock at all (the defect), and
/// then the assertions after the release say so rather than a timeout.
async fn until_waiting<T>(pool: &sqlx::PgPool, n: i64, racer: &tokio::task::JoinHandle<T>) {
    for _ in 0..500 {
        if lock_waiters(pool).await >= n || racer.is_finished() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("{n} session(s) never reached a lock and the racer never finished");
}

async fn packets_on(pool: &sqlx::PgPool, version: i32) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = $1 AND workflow_version = $2")
        .bind(KIND)
        .bind(version)
        .fetch_one(pool)
        .await
        .expect("count pinned packets")
}

/// Observe the specific blocked backend and its blocker, not an anonymous
/// count that could describe a different phase of the transaction.
async fn until_blocked_by<T>(
    pool: &sqlx::PgPool,
    blocker: i32,
    query_fragment: &str,
    racer: &tokio::task::JoinHandle<T>,
) -> i32 {
    for _ in 0..500 {
        let pid: Option<i32> = sqlx::query_scalar(
            "SELECT pid FROM pg_stat_activity WHERE datname = current_database()
             AND $1 = ANY(pg_blocking_pids(pid)) AND position($2 in query) > 0",
        )
        .bind(blocker)
        .bind(query_fragment)
        .fetch_optional(pool)
        .await
        .expect("read exact blocking edge");
        if let Some(pid) = pid {
            println!("backend {pid} executing {query_fragment:?} blocked by {blocker}");
            return pid;
        }
        assert!(
            !racer.is_finished(),
            "racer finished before its required blocking edge"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("no backend executing {query_fragment:?} blocked by {blocker}");
}

/// THE DISCARD FIRST. It has locked the draft, counted no packet,
/// deleted the row, and is held before its commit (on the spent-number
/// insert). An admission onto that version starts now: it must wait for
/// the discard, and — let through — refuse, because the number it pins
/// to was spent. Before this car it did not wait, committed first, and
/// left a packet pinned to a version the discard then committed away.
#[tokio::test(flavor = "multi_thread")]
async fn pg_an_admission_behind_a_discard_is_refused_and_writes_nothing() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    let now = chrono::Utc::now();
    let d = registry
        .create_draft(spec(), &author(), now)
        .await
        .expect("draft");

    // A committed BEFORE INSERT trigger pauses the real discard AFTER
    // its table fence, draft row lock, pin count and DELETE. The holder
    // owns only an advisory lock: a pre-held spent-table INSERT would
    // block the new table fence before the discard acquires its row.
    sqlx::raw_sql(
        "CREATE FUNCTION pause_discard_insert() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_advisory_xact_lock(7391, 1); RETURN NEW; END $$;
         CREATE TRIGGER pause_discard_insert BEFORE INSERT ON workflow_discarded_versions
         FOR EACH ROW EXECUTE FUNCTION pause_discard_insert();",
    )
    .execute(&db.pool)
    .await
    .expect("commit the post-row discard pause");
    let mut hold = db.pool.begin().await.expect("hold tx");
    let holder: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *hold)
        .await
        .expect("holder backend");
    sqlx::query("SELECT pg_advisory_xact_lock(7391, 1)")
        .execute(&mut *hold)
        .await
        .expect("hold the advisory pause");

    let discarding = PgWorkflows::for_fixture(db.pool.clone());
    let version = d.version;
    let discard = tokio::spawn(async move {
        discarding
            .discard_draft(KIND, version, &author(), now)
            .await
    });
    let discard_pid = until_blocked_by(
        &db.pool,
        holder,
        "INSERT INTO workflow_discarded_versions",
        &discard,
    )
    .await;
    let advisory_wait: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid=$1 AND locktype='advisory'
         AND classid=7391 AND objid=1 AND objsubid=2 AND NOT granted)",
    )
    .bind(discard_pid)
    .fetch_one(&db.pool)
    .await
    .expect("exact advisory lock tag");
    assert!(
        advisory_wait,
        "discard is paused at the committed insert trigger"
    );
    assert!(
        !discard.is_finished(),
        "case error: the discard must be held before its commit"
    );

    // The admission races it.
    let admitting = PgJobs::new(db.pool.clone());
    let job = packet(d.version, "race-behind-a-discard");
    let admitted = job.clone();
    let admission = tokio::spawn(async move { admitting.create_job(&admitted).await });
    until_blocked_by(&db.pool, discard_pid, "FOR KEY SHARE", &admission).await;

    hold.rollback().await.expect("release the discard");
    discard
        .await
        .expect("discard task")
        .expect("the discard, holding the row first, removes the draft");

    match admission.await.expect("admission task") {
        Err(JobsError::VersionDiscarded { kind, version }) => {
            assert_eq!((kind.as_str(), version), (KIND, d.version));
        }
        other => {
            panic!("an admission onto a version its discard removed must be refused, got {other:?}")
        }
    }
    assert_eq!(
        packets_on(&db.pool, d.version).await,
        0,
        "no packet is pinned to the discarded number"
    );
    let minted: i64 = sqlx::query_scalar("SELECT count(*) FROM subjects WHERE id = $1")
        .bind("race-behind-a-discard")
        .fetch_one(&db.pool)
        .await
        .expect("count subjects");
    assert_eq!(minted, 0, "a refused admission mints no subject either");
    let created: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'jobs.job.created'")
            .fetch_one(&db.pool)
            .await
            .expect("count admitted job facts");
    assert_eq!(
        created, 0,
        "a refused admission records no job-created fact"
    );
}

/// THE ADMISSION FIRST. It has locked the row it pins to and is held
/// mid-transaction (on its subject mint). A discard of that draft
/// starts now: it must wait for the admission, and — let through —
/// count the packet and refuse. Before this car the discard did not
/// wait, counted nothing, removed the draft, and the admission then
/// committed onto a number with no protocol.
#[tokio::test(flavor = "multi_thread")]
async fn pg_a_discard_behind_an_admission_counts_it_and_refuses() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    let now = chrono::Utc::now();
    let d = registry
        .create_draft(spec(), &author(), now)
        .await
        .expect("draft");

    // Hold the admission mid-transaction: an uncommitted identity row
    // for its subject makes its mint wait.
    let subject = "race-ahead-of-a-discard";
    let mut hold = db.pool.begin().await.expect("hold tx");
    sqlx::query("INSERT INTO subjects (kind, id) VALUES ('custom', $1)")
        .bind(subject)
        .execute(&mut *hold)
        .await
        .expect("hold the subject mint");

    let admitting = PgJobs::new(db.pool.clone());
    let job = packet(d.version, subject);
    let admitted = job.clone();
    let admission = tokio::spawn(async move { admitting.create_job(&admitted).await });
    until_waiting(&db.pool, 1, &admission).await;
    assert!(
        !admission.is_finished(),
        "case error: the admission must be held mid-transaction"
    );

    // The discard races it.
    let discarding = PgWorkflows::for_fixture(db.pool.clone());
    let version = d.version;
    let discard = tokio::spawn(async move {
        discarding
            .discard_draft(KIND, version, &author(), now)
            .await
    });
    until_waiting(&db.pool, 2, &discard).await;

    hold.rollback().await.expect("release the admission");
    admission
        .await
        .expect("admission task")
        .expect("the admission, holding the row first, is admitted");

    match discard.await.expect("discard task") {
        Err(WorkflowError::Conflict(m)) => {
            assert!(m.contains("1 packet is pinned"), "names the count: {m}");
            assert!(m.contains(&job.id.to_string()), "names the packet: {m}");
        }
        other => panic!("a discard behind a pinning admission must refuse, got {other:?}"),
    }
    let kept = registry
        .get_version(KIND, d.version)
        .await
        .expect("the refused discard removed nothing");
    assert_eq!(kept.status, WorkflowStatus::Draft);
    assert_eq!(packets_on(&db.pool, d.version).await, 1);
}

/// The refusal is about a SPENT number, not an unknown one: a packet
/// pinned to a version no row and no discard names is admitted as it
/// always was (a kind outside the registry, a fixture's bare version),
/// and one pinned to a live draft is admitted and holds nothing after
/// its commit — the draft is still discardable once the packet is gone.
#[tokio::test(flavor = "multi_thread")]
async fn pg_only_a_discarded_number_refuses_admission() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::for_fixture(db.pool.clone());
    let repo = PgJobs::new(db.pool.clone());
    let now = chrono::Utc::now();

    repo.create_job(&packet(42, "never-written"))
        .await
        .expect("a version no row and no discard names is admitted");

    let d = registry
        .create_draft(spec(), &author(), now)
        .await
        .expect("draft");
    repo.create_job(&packet(d.version, "on-a-live-draft"))
        .await
        .expect("a live draft admits");

    let spent = registry
        .create_draft(spec(), &author(), now)
        .await
        .expect("second draft");
    registry
        .discard_draft(KIND, spent.version, &author(), now)
        .await
        .expect("discard the unpinned draft");
    match repo
        .create_job(&packet(spent.version, "after-the-discard"))
        .await
    {
        Err(JobsError::VersionDiscarded { kind, version }) => {
            assert_eq!((kind.as_str(), version), (KIND, spent.version));
        }
        other => panic!("a spent number must refuse admission, got {other:?}"),
    }
}
