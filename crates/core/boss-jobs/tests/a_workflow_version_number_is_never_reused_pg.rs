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
    PgWorkflows, StepSpec, Terminal, WorkflowRegistry, WorkflowSpec, WorkflowStatus,
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
    let registry = PgWorkflows::new(db.pool.clone());
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

/// TODAY'S BEHAVIOUR, pinned on purpose — not endorsed: the Pg
/// `get_version` serves a DRAFT row, which is how an experiment's
/// candidate and `boss job convert --to vN` can pin a packet to one.
/// Whether they should is the experiments design's decision (d8771dec,
/// recorded on ce8b7d66); this test is the one that changes with it.
#[tokio::test(flavor = "multi_thread")]
async fn pg_get_version_serves_a_draft_today() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());
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
