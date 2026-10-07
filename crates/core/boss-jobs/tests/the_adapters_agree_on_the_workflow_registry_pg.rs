//! The Workflow registry answers the same on both `WorkflowRegistry`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree", on the third port it reaches (backlog be459ab9; the jobs
//! store's list filters and the agent-run log came first).
//!
//! WHY IT EXISTS. The registry's contract was stated twice and held to
//! itself one method at a time: `registry::tests` asks the in-memory
//! adapter, and five `*_pg.rs` files (`workflow_reconcile_pg`,
//! `a_workflow_version_number_is_never_reused_pg`, …) each re-ask a
//! slice of it of Postgres, by hand, in their own words. This file is
//! the one body of cases both answer — every method of the port, and
//! the publish/discard semantics decided on backlog ce8b7d66, which
//! landed 2026-09-28 in the Pg adapter alone.
//!
//! The first run found two disagreements, both in the in-memory
//! adapter, each fixed in this car:
//! - `discard_draft` refused a draft a packet is pinned to only on
//!   Postgres, which counts the pins in the discard's own transaction.
//!   The in-memory adapter could not see packets at all, so a caller
//!   that reached it without the HTTP route's pre-check discarded a
//!   pinned draft — the very orphaning ce8b7d66 closed. It now counts
//!   them through the jobs port it is handed (`with_packets`), as the
//!   Pg adapter counts them in the jobs table of its own database.
//!   Case `a_discard_refuses_a_draft_a_packet_is_pinned_to`.
//! - `bootstrap_reconcile` stamped an inserted or republished row with
//!   the DEFAULT's `created_at` (the instant the seed was built in
//!   memory), where Postgres stamps the `now` it is handed — the same
//!   instant every other write takes. Case
//!   `the_reconcile_inserts_republishes_preserves_and_refuses`.
//!
//! Part 4 of ce8b7d66 added one case and changed one wiring: an
//! admission onto a number a discard spent is refused on both adapters
//! (`a_packet_is_never_admitted_onto_a_discarded_version`), and
//! `with_packets` now takes the in-memory jobs store itself, because the
//! discard's count-and-spend and the admission's check-and-insert are
//! ordered by that store's lock.
//!
//! THE SHAPE, the other two suites', so a case cannot pass by accident:
//! - Each case states its answer as `(version, status)` pairs or kinds,
//!   and each adapter is held to that stated answer, not merely to the
//!   other adapter.
//! - A filter's answer (`list_active` by category) must be a STRICT
//!   subset of the kinds the case wrote: an ignored filter answers all.
//!
//! What is NOT compared, and why:
//! - The events each write records. The in-memory adapter collects
//!   them in a Vec and Postgres writes them to `event_outbox` in the
//!   row's transaction; `workflow_registry_events_pg.rs` pins the Pg
//!   half and `registry::tests` the in-memory half, and a port method
//!   does not return them.
//! - Rows the world starts with. A migration seeds one active row
//!   (`repair-a-train`, 132-repair-a-train.sql) into every Postgres
//!   database, and the in-memory adapter starts empty, so every kind
//!   this file writes starts `suite-` and an unfiltered read is judged
//!   over those kinds only.
//! - `created_by`. Postgres holds who wrote a row in a column the port
//!   never returns; the in-memory adapter holds only the one fact the
//!   port acts on (bootstrap-owned or not), and the reconcile case
//!   reads that fact through its effect: preserved or republished.

use std::sync::Arc;

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::{JobsError, JobsRepository};
use boss_jobs::registry::{
    JobTrigger, KindReconcileStats, StepSpec, Terminal, WorkflowError, WorkflowRegistry,
    WorkflowSpec, WorkflowStatus,
};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};

/// The registry under test and the jobs store it shares a world with —
/// one database on Postgres, and on the in-memory side the store the
/// registry is handed to count pinned packets in.
struct World<R, J> {
    registry: R,
    jobs: Arc<J>,
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => {
            let jobs = Arc::new(InMemoryJobs::new());
            let registry = InMemoryWorkflows::for_fixture().with_packets(jobs.clone());
            (World { registry, jobs }, ())
        },
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let registry = boss_jobs::registry::PgWorkflows::for_fixture(db.pool.clone());
            let jobs = Arc::new(boss_jobs::PgJobs::new(db.pool.clone()));
            (World { registry, jobs }, db)
        },
    }
    cases {
        a_draft_reads_back_as_written,
        get_version_serves_a_draft,
        publish_promotes_the_newest_draft_and_retires_the_active,
        a_publish_that_lost_its_draft_refuses_and_flips_nothing,
        an_unviable_draft_refuses_publish_and_flips_nothing,
        retire_is_idempotent_and_keeps_history_readable,
        a_discard_refuses_history_and_a_typo,
        a_discard_refuses_a_draft_a_packet_is_pinned_to,
        a_packet_is_never_admitted_onto_a_discarded_version,
        a_spent_version_number_is_never_handed_out_again,
        publish_authored_retires_the_active_and_names_its_job,
        conditional_publication_keeps_equal_and_refuses_every_history_boundary,
        concurrent_conditional_publications_insert_one_complete_definition,
        list_active_narrows_by_category_in_byte_order,
        the_reconcile_inserts_republishes_preserves_and_refuses,
    }
}

/// Whole seconds: Postgres holds microseconds, so an instant finer than
/// that would read back as a different value on one adapter only.
fn instant(secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 28, 1, 0, 0)
        .single()
        .expect("an instant")
        + Duration::seconds(secs)
}

fn author() -> ActorId {
    ActorId::Human("emp-cto".into())
}

/// A minimal VIABLE spec — start → finish (terminal). Publish and the
/// reconcile both run the viability gate.
fn spec(kind: &str, label: &str, category: &str) -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        kind,
        label,
        category,
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

fn viable(kind: &str) -> WorkflowSpec {
    spec(kind, "Suite", "suite")
}

async fn conditional_publication_keeps_equal_and_refuses_every_history_boundary<
    R: WorkflowRegistry,
    J: JobsRepository,
>(
    w: &World<R, J>,
    adapter: &str,
) {
    let authored = viable("suite-conditional");
    let first_job = JobId::new();
    let first = w
        .registry
        .publish_authored_if_absent(authored.clone(), first_job, &author(), instant(1))
        .await
        .unwrap();
    assert_eq!(first.version, 1, "{adapter}");
    assert_eq!(
        first.authoring_job_id,
        Some(*first_job.inner().as_uuid()),
        "{adapter}"
    );
    let equal = w
        .registry
        .publish_authored_if_absent(authored.clone(), JobId::new(), &author(), instant(2))
        .await
        .unwrap();
    assert_eq!(
        equal, first,
        "{adapter}: equal returns original complete row"
    );
    let mut changed = authored.clone();
    changed.description = Some("A different full authored field".into());
    assert!(
        matches!(
            w.registry
                .publish_authored_if_absent(changed, JobId::new(), &author(), instant(3))
                .await,
            Err(WorkflowError::Conflict(_))
        ),
        "{adapter}"
    );
    assert_eq!(
        w.registry.get_active(&authored.kind).await.unwrap(),
        first,
        "{adapter}"
    );
    w.registry
        .retire(&authored.kind, &author(), instant(4))
        .await
        .unwrap();
    assert!(
        matches!(
            w.registry
                .publish_authored_if_absent(authored, JobId::new(), &author(), instant(5))
                .await,
            Err(WorkflowError::Conflict(_))
        ),
        "{adapter}: retired history refuses"
    );
    let draft = viable("suite-conditional-draft");
    let row = w
        .registry
        .create_draft(draft.clone(), &author(), instant(6))
        .await
        .unwrap();
    assert!(
        matches!(
            w.registry
                .publish_authored_if_absent(draft.clone(), JobId::new(), &author(), instant(7))
                .await,
            Err(WorkflowError::Conflict(_))
        ),
        "{adapter}: draft refuses"
    );
    w.registry
        .discard_draft(&draft.kind, row.version, &author(), instant(8))
        .await
        .unwrap();
    assert!(
        matches!(
            w.registry
                .publish_authored_if_absent(draft, JobId::new(), &author(), instant(9))
                .await,
            Err(WorkflowError::Conflict(_))
        ),
        "{adapter}: discarded identity refuses"
    );
}

async fn concurrent_conditional_publications_insert_one_complete_definition<
    R: WorkflowRegistry,
    J: JobsRepository,
>(
    w: &World<R, J>,
    adapter: &str,
) {
    let first = viable("suite-conditional-race");
    let mut second = first.clone();
    second.label = "Different racing definition".into();
    let first_job = JobId::new();
    let second_job = JobId::new();
    let actor = author();
    let (a, b) = tokio::join!(
        w.registry
            .publish_authored_if_absent(first, first_job, &actor, instant(1)),
        w.registry
            .publish_authored_if_absent(second, second_job, &actor, instant(2)),
    );
    assert_eq!(
        usize::from(a.is_ok()) + usize::from(b.is_ok()),
        1,
        "{adapter}: {a:?} / {b:?}"
    );
    let held = a.or(b).unwrap();
    assert_eq!(
        w.registry.list_versions(&held.kind).await.unwrap(),
        vec![held],
        "{adapter}: never supersede a racing insertion"
    );
}

/// No terminal: the viability gate refuses it.
fn unviable(kind: &str) -> WorkflowSpec {
    let mut s = viable(kind);
    s.steps.truncate(1);
    s
}

/// Every version of `kind`, as `(version, status)`, oldest first.
async fn versions<R: WorkflowRegistry>(reg: &R, kind: &str) -> Vec<(i32, WorkflowStatus)> {
    reg.list_versions(kind)
        .await
        .expect("list_versions")
        .into_iter()
        .map(|s| (s.version, s.status))
        .collect()
}

async fn draft<R: WorkflowRegistry>(reg: &R, s: WorkflowSpec) -> WorkflowSpec {
    reg.create_draft(s, &author(), instant(0))
        .await
        .expect("create_draft")
}

async fn publish<R: WorkflowRegistry>(reg: &R, kind: &str) -> WorkflowSpec {
    reg.publish(kind, &author(), instant(0))
        .await
        .expect("publish")
}

fn packet(kind: &str, version: i32) -> Job {
    let mut job = Job::new(
        kind,
        Subject::new("account", "acct-1"),
        format!("{kind} packet"),
        "emp-1",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 9, 28).expect("a date"),
    )
    .with_workflow_version(version);
    job.status = JobStatus::Open;
    job
}

/// A draft reads back field for field as `create_draft` returned it —
/// through Postgres's JSONB columns as through the in-memory map — with
/// the version and status the port assigns and the `now` it was handed.
async fn a_draft_reads_back_as_written<R: WorkflowRegistry, J: JobsRepository>(
    w: &World<R, J>,
    adapter: &str,
) {
    let mut s = viable("suite-round-trip");
    s.version = 41;
    s.status = WorkflowStatus::Active;
    s.description = Some("every column carries something".into());
    s.metadata_schema = serde_json::json!({"type": "object"});
    s.entitlements = serde_json::json!({"read": ["platform-admin"]});
    s.metadata = serde_json::json!({"station": "suite"});
    s.on_complete_create = vec![
        serde_json::from_value::<JobTrigger>(serde_json::json!({"kind": "suite-follow-up"}))
            .expect("a trigger"),
    ];
    s.owning_team = "suite-team".into();
    s.authoring_job_id = None;

    let stored = w
        .registry
        .create_draft(s.clone(), &author(), instant(5))
        .await
        .expect("create_draft");
    assert_eq!(
        stored.version, 1,
        "{adapter}: numbered by the port, not the caller"
    );
    assert_eq!(stored.status, WorkflowStatus::Draft, "{adapter}");
    assert_eq!(
        stored.created_at,
        instant(5),
        "{adapter}: the now handed in"
    );
    let mut expected = s;
    expected.version = 1;
    expected.status = WorkflowStatus::Draft;
    expected.created_at = instant(5);
    assert_eq!(stored, expected, "{adapter}: the returned row");

    let read = w
        .registry
        .get_version("suite-round-trip", 1)
        .await
        .expect("get_version");
    assert_eq!(read, stored, "{adapter}: the row read back");
}

/// BY DECISION (backlog ce8b7d66): `get_version` has no status filter,
/// so it serves a DRAFT — experiment admission reads its candidate
/// here. `get_active` does not, and a version never written is
/// `NotFound`.
async fn get_version_serves_a_draft<R: WorkflowRegistry, J: JobsRepository>(
    w: &World<R, J>,
    adapter: &str,
) {
    let d = draft(&w.registry, viable("suite-drafted")).await;
    let served = w
        .registry
        .get_version("suite-drafted", d.version)
        .await
        .expect("a draft is served");
    assert_eq!(served.status, WorkflowStatus::Draft, "{adapter}");

    match w.registry.get_active("suite-drafted").await {
        Err(WorkflowError::NotFound(_)) => {}
        other => panic!("{adapter}: a draft is not active, got {other:?}"),
    }
    match w.registry.get_version("suite-drafted", d.version + 1).await {
        Err(WorkflowError::NotFound(_)) => {}
        other => panic!("{adapter}: an unwritten version answered {other:?}"),
    }
}

/// Publish promotes the NEWEST draft and retires the active row; an
/// older draft stays a draft. A second publish then promotes that
/// older draft over the newer active one — today's semantics on both
/// adapters (the endpoint takes no version), stated so a change to
/// them is deliberate.
async fn publish_promotes_the_newest_draft_and_retires_the_active<
    R: WorkflowRegistry,
    J: JobsRepository,
>(
    w: &World<R, J>,
    adapter: &str,
) {
    let kind = "suite-publish";
    draft(&w.registry, viable(kind)).await;
    let first = publish(&w.registry, kind).await;
    assert_eq!(
        (first.version, first.status),
        (1, WorkflowStatus::Active),
        "{adapter}"
    );

    draft(&w.registry, viable(kind)).await;
    draft(&w.registry, viable(kind)).await;
    let promoted = publish(&w.registry, kind).await;
    assert_eq!(promoted.version, 3, "{adapter}: the newest draft");
    assert_eq!(
        versions(&w.registry, kind).await,
        [
            (1, WorkflowStatus::Retired),
            (2, WorkflowStatus::Draft),
            (3, WorkflowStatus::Active),
        ],
        "{adapter}"
    );
    assert_eq!(
        w.registry.get_active(kind).await.expect("active").version,
        3,
        "{adapter}"
    );

    let again = publish(&w.registry, kind).await;
    assert_eq!(again.version, 2, "{adapter}: the only draft left");
    assert_eq!(
        versions(&w.registry, kind).await,
        [
            (1, WorkflowStatus::Retired),
            (2, WorkflowStatus::Active),
            (3, WorkflowStatus::Retired),
        ],
        "{adapter}"
    );
}

/// A publish whose draft was discarded before it arrived refuses
/// `NotFound` — it must not retire the active row with nothing to put
/// in its place, nor promote anything else. The sequential shape of
/// the race the discard's row lock closes (backlog ce8b7d66).
async fn a_publish_that_lost_its_draft_refuses_and_flips_nothing<
    R: WorkflowRegistry,
    J: JobsRepository,
>(
    w: &World<R, J>,
    adapter: &str,
) {
    let kind = "suite-lost-draft";
    draft(&w.registry, viable(kind)).await;
    publish(&w.registry, kind).await;
    let d = draft(&w.registry, viable(kind)).await;
    w.registry
        .discard_draft(kind, d.version, &author(), instant(0))
        .await
        .expect("discard");

    match w.registry.publish(kind, &author(), instant(0)).await {
        Err(WorkflowError::NotFound(_)) => {}
        other => panic!("{adapter}: a publish with no draft answered {other:?}"),
    }
    assert_eq!(
        versions(&w.registry, kind).await,
        [(1, WorkflowStatus::Active)],
        "{adapter}: the active row stands, and nothing else appeared"
    );
    match w
        .registry
        .publish("suite-never-written", &author(), instant(0))
        .await
    {
        Err(WorkflowError::NotFound(_)) => {}
        other => panic!("{adapter}: a kind with no rows answered {other:?}"),
    }
}

/// The viability gate refuses before any row flips.
async fn an_unviable_draft_refuses_publish_and_flips_nothing<
    R: WorkflowRegistry,
    J: JobsRepository,
>(
    w: &World<R, J>,
    adapter: &str,
) {
    let kind = "suite-unviable";
    draft(&w.registry, viable(kind)).await;
    publish(&w.registry, kind).await;
    draft(&w.registry, unviable(kind)).await;
    match w.registry.publish(kind, &author(), instant(0)).await {
        Err(WorkflowError::Unviable(problems)) => {
            assert!(!problems.is_empty(), "{adapter}: names its problems")
        }
        other => panic!("{adapter}: an unviable draft answered {other:?}"),
    }
    assert_eq!(
        versions(&w.registry, kind).await,
        [(1, WorkflowStatus::Active), (2, WorkflowStatus::Draft)],
        "{adapter}"
    );
}

/// Retire takes the kind out of service and keeps its row readable —
/// a pinned packet goes on reading the text it ran under. A second
/// retire, and a retire of a kind never written, are no-ops.
async fn retire_is_idempotent_and_keeps_history_readable<R: WorkflowRegistry, J: JobsRepository>(
    w: &World<R, J>,
    adapter: &str,
) {
    let kind = "suite-retire";
    draft(&w.registry, viable(kind)).await;
    publish(&w.registry, kind).await;
    w.registry
        .retire(kind, &author(), instant(0))
        .await
        .expect("retire");
    match w.registry.get_active(kind).await {
        Err(WorkflowError::NotFound(_)) => {}
        other => panic!("{adapter}: a retired kind answered {other:?}"),
    }
    assert_eq!(
        w.registry
            .get_version(kind, 1)
            .await
            .expect("history")
            .status,
        WorkflowStatus::Retired,
        "{adapter}"
    );
    w.registry
        .retire(kind, &author(), instant(0))
        .await
        .expect("retire again");
    w.registry
        .retire("suite-never-written", &author(), instant(0))
        .await
        .expect("retire nothing");
    assert_eq!(
        versions(&w.registry, kind).await,
        [(1, WorkflowStatus::Retired)],
        "{adapter}"
    );
}

/// An active or retired version is history and refuses `Conflict`; a
/// version that does not exist refuses `NotFound`, so a typo never
/// reads as success. A discarded draft is gone from every read.
async fn a_discard_refuses_history_and_a_typo<R: WorkflowRegistry, J: JobsRepository>(
    w: &World<R, J>,
    adapter: &str,
) {
    let kind = "suite-discard";
    draft(&w.registry, viable(kind)).await;
    publish(&w.registry, kind).await; // v1 active
    draft(&w.registry, viable(kind)).await;
    publish(&w.registry, kind).await; // v1 retired, v2 active
    let d = draft(&w.registry, viable(kind)).await; // v3 draft

    for version in [1, 2] {
        match w
            .registry
            .discard_draft(kind, version, &author(), instant(0))
            .await
        {
            Err(WorkflowError::Conflict(_)) => {}
            other => panic!("{adapter}: discarding history v{version} answered {other:?}"),
        }
    }
    match w
        .registry
        .discard_draft(kind, 9, &author(), instant(0))
        .await
    {
        Err(WorkflowError::NotFound(_)) => {}
        other => panic!("{adapter}: discarding a typo answered {other:?}"),
    }

    w.registry
        .discard_draft(kind, d.version, &author(), instant(0))
        .await
        .expect("a draft discards");
    match w.registry.get_version(kind, d.version).await {
        Err(WorkflowError::NotFound(_)) => {}
        other => panic!("{adapter}: a discarded draft answered {other:?}"),
    }
    assert_eq!(
        versions(&w.registry, kind).await,
        [(1, WorkflowStatus::Retired), (2, WorkflowStatus::Active)],
        "{adapter}"
    );
}

/// A draft a packet is pinned to is not pre-history: its discard
/// refuses `Conflict`, naming the count and a packet, and removes and
/// spends nothing (backlog ce8b7d66). Asked of the ADAPTER, not the
/// route — the route's own pre-check is a read in another transaction.
async fn a_discard_refuses_a_draft_a_packet_is_pinned_to<R: WorkflowRegistry, J: JobsRepository>(
    w: &World<R, J>,
    adapter: &str,
) {
    let kind = "suite-pinned";
    let d = draft(&w.registry, viable(kind)).await;
    let pinned = packet(kind, d.version);
    w.jobs.create_job(&pinned).await.expect("pinned packet");
    // A packet of the same kind on another version pins nothing here.
    w.jobs
        .create_job(&packet(kind, d.version + 7))
        .await
        .expect("other packet");

    match w
        .registry
        .discard_draft(kind, d.version, &author(), instant(0))
        .await
    {
        Err(WorkflowError::Conflict(m)) => {
            assert!(
                m.contains("1 packet is pinned"),
                "{adapter}: the count: {m}"
            );
            assert!(
                m.contains(&pinned.id.to_string()),
                "{adapter}: names it: {m}"
            );
        }
        other => panic!("{adapter}: a pinned draft's discard answered {other:?}"),
    }
    assert_eq!(
        w.registry
            .get_version(kind, d.version)
            .await
            .expect("a refused discard removes nothing")
            .status,
        WorkflowStatus::Draft,
        "{adapter}"
    );
    let next = draft(&w.registry, viable(kind)).await;
    assert_eq!(
        next.version,
        d.version + 1,
        "{adapter}: a refused discard spends nothing"
    );
}

/// A number a discard spent admits no packet (backlog ce8b7d66, part 4).
/// The discard counts pins and removes the draft as one act; an
/// admission that reaches the jobs store after it is refused
/// `VersionDiscarded`, naming the pair, and pins nothing — the
/// sequential shape of the race the Pg row locks close
/// (`a_packet_is_never_admitted_onto_a_discarded_version_pg.rs` plays
/// both interleavings). Only a SPENT number refuses: a live draft
/// admits, and so does a version no row and no discard names.
async fn a_packet_is_never_admitted_onto_a_discarded_version<
    R: WorkflowRegistry,
    J: JobsRepository,
>(
    w: &World<R, J>,
    adapter: &str,
) {
    let kind = "suite-admit";
    let spent = draft(&w.registry, viable(kind)).await;
    let live = draft(&w.registry, viable(kind)).await;
    w.registry
        .discard_draft(kind, spent.version, &author(), instant(0))
        .await
        .expect("discard the unpinned draft");

    match w.jobs.create_job(&packet(kind, spent.version)).await {
        Err(JobsError::VersionDiscarded {
            kind: refused,
            version,
        }) => assert_eq!(
            (refused.as_str(), version),
            (kind, spent.version),
            "{adapter}: the refusal names the pair"
        ),
        other => panic!("{adapter}: admission onto a discarded version answered {other:?}"),
    }
    assert_eq!(
        w.jobs
            .jobs_pinned_to_workflow(kind, spent.version)
            .await
            .expect("pin count")
            .count,
        0,
        "{adapter}: a refused admission pins nothing"
    );

    w.jobs
        .create_job(&packet(kind, live.version))
        .await
        .expect("a live draft admits");
    w.jobs
        .create_job(&packet(kind, live.version + 5))
        .await
        .expect("a version never written admits");
}

/// A number, once handed out, names one protocol forever (backlog
/// ce8b7d66): every allocating write — a draft, an authored publish,
/// the reconcile's insert — numbers above every version the kind has
/// SPENT, discarded drafts included.
async fn a_spent_version_number_is_never_handed_out_again<
    R: WorkflowRegistry,
    J: JobsRepository,
>(
    w: &World<R, J>,
    adapter: &str,
) {
    let kind = "suite-spent";
    let v1 = draft(&w.registry, viable(kind)).await;
    assert_eq!(
        v1.version, 1,
        "{adapter}: a kind with no history starts at 1"
    );
    w.registry
        .discard_draft(kind, 1, &author(), instant(0))
        .await
        .expect("discard v1");
    let v2 = draft(&w.registry, viable(kind)).await;
    assert_eq!(v2.version, 2, "{adapter}: a draft skips the discarded v1");
    w.registry
        .discard_draft(kind, 2, &author(), instant(0))
        .await
        .expect("discard v2");
    let v3 = w
        .registry
        .publish_authored(viable(kind), JobId::new(), &author(), instant(0))
        .await
        .expect("publish_authored");
    assert_eq!(v3.version, 3, "{adapter}: an authored publish skips both");
    assert_eq!(
        versions(&w.registry, kind).await,
        [(3, WorkflowStatus::Active)],
        "{adapter}"
    );

    let seeded = "suite-spent-seed";
    draft(&w.registry, viable(seeded)).await;
    w.registry
        .discard_draft(seeded, 1, &author(), instant(0))
        .await
        .expect("discard");
    w.registry
        .bootstrap_reconcile(&[viable(seeded)], &author(), instant(0))
        .await
        .expect("reconcile");
    assert_eq!(
        versions(&w.registry, seeded).await,
        [(2, WorkflowStatus::Active)],
        "{adapter}: the reconcile's insert skips the discarded v1"
    );
}

/// The single-shot path: numbered like a draft, active at once, the
/// previous active retired, the authoring packet named on the row.
async fn publish_authored_retires_the_active_and_names_its_job<
    R: WorkflowRegistry,
    J: JobsRepository,
>(
    w: &World<R, J>,
    adapter: &str,
) {
    let kind = "suite-authored";
    draft(&w.registry, viable(kind)).await;
    publish(&w.registry, kind).await;
    let author_job = JobId::new();
    let published = w
        .registry
        .publish_authored(viable(kind), author_job, &author(), instant(7))
        .await
        .expect("publish_authored");
    let named = Some(*author_job.inner().as_uuid());
    assert_eq!(published.authoring_job_id, named, "{adapter}");
    assert_eq!(published.created_at, instant(7), "{adapter}");
    let active = w.registry.get_active(kind).await.expect("active");
    assert_eq!(active, published, "{adapter}: the row read back");
    assert_eq!(
        versions(&w.registry, kind).await,
        [(1, WorkflowStatus::Retired), (2, WorkflowStatus::Active)],
        "{adapter}"
    );

    match w
        .registry
        .publish_authored(unviable(kind), JobId::new(), &author(), instant(8))
        .await
    {
        Err(WorkflowError::Unviable(_)) => {}
        other => panic!("{adapter}: an unviable authored spec answered {other:?}"),
    }
    assert_eq!(
        versions(&w.registry, kind).await,
        [(1, WorkflowStatus::Retired), (2, WorkflowStatus::Active)],
        "{adapter}: a refused authored publish writes and spends nothing"
    );
}

/// `list_active` answers ACTIVE rows only, one per kind, ordered by
/// kind in BYTE order — `suite-a-z` before `suite-ab`, because `-`
/// sorts below `b` — and a category narrows it.
async fn list_active_narrows_by_category_in_byte_order<R: WorkflowRegistry, J: JobsRepository>(
    w: &World<R, J>,
    adapter: &str,
) {
    for kind in ["suite-b", "suite-ab", "suite-a-z"] {
        draft(&w.registry, viable(kind)).await;
        publish(&w.registry, kind).await;
    }
    draft(&w.registry, spec("suite-other", "Other", "elsewhere")).await;
    publish(&w.registry, "suite-other").await;
    // A draft-only kind and a retired kind in the same category are
    // not active, so neither is listed.
    draft(&w.registry, viable("suite-draft-only")).await;
    draft(&w.registry, viable("suite-retired")).await;
    publish(&w.registry, "suite-retired").await;
    w.registry
        .retire("suite-retired", &author(), instant(0))
        .await
        .expect("retire");
    // A kind with an active row AND a newer draft is listed once, at
    // its active version.
    draft(&w.registry, viable("suite-b")).await;

    let listed = |specs: Vec<WorkflowSpec>| -> Vec<(String, i32)> {
        specs
            .into_iter()
            .filter(|s| s.kind.starts_with("suite-"))
            .map(|s| (s.kind, s.version))
            .collect()
    };
    let all = listed(w.registry.list_active(None).await.expect("list_active"));
    let one = |k: &str| (k.to_string(), 1);
    assert_eq!(
        all,
        [
            one("suite-a-z"),
            one("suite-ab"),
            one("suite-b"),
            one("suite-other")
        ],
        "{adapter}: every active kind, in byte order"
    );
    let suite = listed(
        w.registry
            .list_active(Some("suite"))
            .await
            .expect("list_active"),
    );
    assert!(
        suite.len() < all.len(),
        "case error: a category's answer must be a strict subset of {all:?}"
    );
    assert_eq!(
        suite,
        [one("suite-a-z"), one("suite-ab"), one("suite-b")],
        "{adapter}: the category narrows"
    );
}

/// The boot-time reconcile, every branch: a missing kind is inserted
/// bootstrap-owned; an unchanged one is left alone; a drifted
/// bootstrap-owned one is republished as a NEW version (the old one
/// retired, never rewritten in place); an operator-published one is
/// preserved; an unviable default is refused and never written. Each
/// row it writes is stamped with the `now` it was handed.
async fn the_reconcile_inserts_republishes_preserves_and_refuses<
    R: WorkflowRegistry,
    J: JobsRepository,
>(
    w: &World<R, J>,
    adapter: &str,
) {
    let seed_a = spec("suite-seed-a", "A", "suite");
    let seed_b = spec("suite-seed-b", "B", "suite");
    let broken = unviable("suite-seed-broken");

    let first = w
        .registry
        .bootstrap_reconcile(
            &[seed_a.clone(), seed_b.clone(), broken.clone()],
            &author(),
            instant(10),
        )
        .await
        .expect("reconcile");
    assert_eq!(
        first,
        KindReconcileStats {
            inserted: 2,
            rejected: 1,
            ..Default::default()
        },
        "{adapter}: the first boot"
    );
    let a = w.registry.get_active("suite-seed-a").await.expect("a");
    assert_eq!(
        (a.version, a.created_at),
        (1, instant(10)),
        "{adapter}: inserted at v1, stamped with the reconcile's now"
    );
    assert!(
        w.registry
            .list_versions("suite-seed-broken")
            .await
            .expect("list")
            .is_empty(),
        "{adapter}: a refused default is never written"
    );

    let second = w
        .registry
        .bootstrap_reconcile(
            &[seed_a.clone(), seed_b.clone(), broken.clone()],
            &author(),
            instant(20),
        )
        .await
        .expect("reconcile");
    assert_eq!(
        second,
        KindReconcileStats {
            unchanged: 2,
            rejected: 1,
            ..Default::default()
        },
        "{adapter}: a boot with nothing drifted"
    );

    // An operator publishes B; the platform's A drifts.
    w.registry
        .publish_authored(
            spec("suite-seed-b", "B, edited", "suite"),
            JobId::new(),
            &author(),
            instant(25),
        )
        .await
        .expect("operator publish");
    let drifted_a = spec("suite-seed-a", "A, drifted", "suite");
    let third = w
        .registry
        .bootstrap_reconcile(&[drifted_a, seed_b], &author(), instant(30))
        .await
        .expect("reconcile");
    assert_eq!(
        third,
        KindReconcileStats {
            republished: 1,
            preserved: 1,
            ..Default::default()
        },
        "{adapter}: a drifted default and an operator's edit"
    );

    let a = w.registry.get_active("suite-seed-a").await.expect("a");
    assert_eq!(
        (
            a.version,
            a.label.as_str(),
            a.created_at,
            a.authoring_job_id
        ),
        (2, "A, drifted", instant(30), None),
        "{adapter}: republished as a new version, stamped with the reconcile's now"
    );
    assert_eq!(
        w.registry
            .get_version("suite-seed-a", 1)
            .await
            .expect("v1")
            .label,
        "A",
        "{adapter}: v1 keeps the body it had"
    );
    assert_eq!(
        versions(&w.registry, "suite-seed-a").await,
        [(1, WorkflowStatus::Retired), (2, WorkflowStatus::Active)],
        "{adapter}"
    );
    let b = w.registry.get_active("suite-seed-b").await.expect("b");
    assert_eq!(
        (b.version, b.label.as_str()),
        (2, "B, edited"),
        "{adapter}: the operator's edit is preserved"
    );
}
