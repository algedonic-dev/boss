//! Every `JobFilter` field narrows a listing, and its `total`, the same
//! way on both `JobsRepository` adapters — reliability mechanism C of
//! design 3036296f, "adapters agree" (backlog 67c15125).
//!
//! WHY IT EXISTS. The Postgres adapter ignored `JobFilter::priority`
//! while the in-memory adapter honoured it (backlog 74569e94), so a
//! listing asked for `urgent` answered every packet. Every earlier test
//! of the filter ran against the in-memory adapter, which was right, so
//! nothing noticed: a filter the store ignores answers instead of
//! erroring. That one field was fixed and pinned alone
//! (`a_priority_filter_narrows_the_list_in_postgres_pg.rs`); eleven
//! more fields were pinned by nothing that held the two adapters to one
//! answer. This file does, for every field, through one body of cases
//! run against both (`boss_testing::adapters_agree!`).
//!
//! THE SHAPE, so a case cannot pass by accident:
//! - Each case states its answer as titles, and each adapter is held to
//!   that stated answer — not merely to the other adapter, which would
//!   let two equally-wrong adapters agree.
//! - Each answer must be a STRICT subset of the seed. A filter the
//!   store ignores answers the whole seed, so an answer that is the
//!   whole seed could not tell a bound filter from an ignored one.
//! - The rows come from the list query and `total` from Postgres's
//!   separate count query, so a field bound in one and not the other
//!   (the count half of 74569e94's fix was a second edit) is caught by
//!   the same assertion.
//! - `every_field_is_a_case` destructures `JobFilter` field by field: a
//!   field added to the port does not compile here until it is named,
//!   and naming it is the moment to give it a case.
//!
//! The first run found a second disagreement: Postgres read
//! `kind_prefix` as a LIKE pattern, so `_` and `%` in it were wildcards
//! and `?kind_prefix=%` answered every packet, while the in-memory
//! adapter (and the port's own doc, "prefix match on kind") treats it
//! as literal text. Case `kind_prefix_is_literal_text`.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::partition::Partition;
use boss_jobs::InMemoryJobs;
use boss_jobs::port::{DepartmentFilter, JobFilter, JobScope, JobsRepository};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use uuid::Uuid;

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryJobs::new(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (boss_jobs::PgJobs::new(db.pool.clone()), db)
        },
    }
    cases {
        kind,
        kind_prefix,
        kind_prefix_is_literal_text,
        kinds,
        department,
        status,
        closed_since,
        priority,
        owner_id,
        subject_id,
        waiting_on,
        metadata_contains,
        metadata_has,
        scope,
        partition,
        a_page_is_a_window_and_the_total_is_the_world,
    }
}

/// A field added to `JobFilter` is a compile error here until it is
/// named — and each name points at the case that covers it.
const _: fn(JobFilter) = |filter| {
    let JobFilter {
        kind: _,              // case `kind`
        kind_prefix: _,       // cases `kind_prefix`, `kind_prefix_is_literal_text`
        kinds: _,             // case `kinds`
        department: _,        // case `department`
        status: _,            // case `status`
        closed_since: _,      // case `closed_since`
        priority: _,          // case `priority`
        owner_id: _,          // case `owner_id`
        subject_id: _,        // case `subject_id`
        waiting_on: _,        // case `waiting_on`
        metadata_contains: _, // case `metadata_contains`
        metadata_has: _,      // case `metadata_has`
        scope: _,             // case `scope`
        partition: _,         // case `partition`
    } = filter;
};

/// The packet a `waiting_on` filter names — a full id, which a waiter
/// may have written whole or as a >= 8-character prefix.
const BLOCKER: &str = "b10cce5e-0000-4000-8000-000000000001";

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, d).expect("a September day")
}

fn instant(secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 20, 12, 0, 0)
        .single()
        .expect("an instant")
        + Duration::seconds(secs)
}

/// A plain open packet; each seed row changes only what its title says.
fn packet(title: &str) -> Job {
    Job {
        id: JobId::new(),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "s-0"),
        title: title.into(),
        owner_id: "emp-a".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: day(20),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: Partition::Real,
    }
}

fn with(title: &str, change: impl FnOnce(&mut Job)) -> Job {
    let mut job = packet(title);
    change(&mut job);
    job
}

/// The world every filter case narrows. Each row exists to sit on one
/// side of some filter's line; the comment says which.
fn world() -> Vec<Job> {
    let md = |v: serde_json::Value| move |j: &mut Job| j.metadata = v;
    vec![
        // kind / kind_prefix / owner / subject / account scope
        with("field-a", |j| {
            j.kind = "field-service".into();
            j.owner_id = "emp-b".into();
            j.subject = Subject::new("account", "acct-1");
        }),
        // a closed packet inside the retention window; an employee
        // subject counts under an account scope
        with("field-b", |j| {
            j.kind = "field-service".into();
            j.status = JobStatus::Closed;
            j.closed_on = Some(day(15));
            j.subject = Subject::new("employee", "acct-1");
        }),
        // a terminal packet outside the window
        with("field-trip", |j| {
            j.kind = "field-trip".into();
            j.status = JobStatus::Cancelled;
            j.closed_on = Some(day(1));
        }),
        // closed with no close date: no window keeps it
        with("closed-undated", |j| j.status = JobStatus::Closed),
        // a declaring kind whose packet names ANOTHER department
        with("train", |j| {
            j.kind = "pr-train".into();
            j.priority = Priority::Urgent;
            j.metadata = serde_json::json!({"department": "ops"});
        }),
        // a declaring kind, naming no department: in by its kind
        with("train-plain", |j| j.kind = "pr-train".into()),
        // an empty department word is no word: in by its kind
        with("train-blank", |j| {
            j.kind = "pr-train".into();
            j.metadata = serde_json::json!({"department": ""});
        }),
        // a non-declaring kind naming the department itself
        with("sales-own", md(serde_json::json!({"department": "sales"}))),
        with("waits-full", md(serde_json::json!({"waiting_on": BLOCKER}))),
        with(
            "waits-prefix",
            md(serde_json::json!({"waiting_on": "b10cce5e"})),
        ),
        // seven characters is too short to resolve
        with(
            "waits-short",
            md(serde_json::json!({"waiting_on": "b10cce5"})),
        ),
        with(
            "finding",
            md(serde_json::json!({"estate_finding": "disk", "lane": "fast"})),
        ),
        // the key is present; its value does not matter
        with(
            "finding-null",
            md(serde_json::json!({"estate_finding": null})),
        ),
        with("lane-slow", md(serde_json::json!({"lane": "slow"}))),
        with("sim", |j| j.partition = Partition::Simulated),
        with("shadow", |j| j.partition = Partition::Shadow),
        // the account's id on a subject that is not an account
        with("asset-acct", |j| {
            j.subject = Subject::new("asset", "acct-1")
        }),
        with("emergency", |j| {
            j.priority = Priority::Emergency;
            j.owner_id = "emp-c".into();
        }),
        with("draft", |j| j.status = JobStatus::Draft),
    ]
}

async fn seed<R: JobsRepository>(repo: &R, jobs: &[Job], at: impl Fn(usize) -> DateTime<Utc>) {
    for (i, job) in jobs.iter().enumerate() {
        repo.create_job_at(job, at(i), &[])
            .await
            .unwrap_or_else(|e| panic!("seed {}: {e}", job.title));
    }
}

/// Every filter case starts here, on a store of its own.
async fn seed_world<R: JobsRepository>(repo: &R) {
    seed(repo, &world(), |i| instant(i as i64)).await;
}

/// List the seeded world through `filter` and hold the answer to
/// `expect` — rows AND total.
async fn narrows<R: JobsRepository>(repo: &R, adapter: &str, filter: JobFilter, expect: &[&str]) {
    let world = world();
    let titles: Vec<&str> = world.iter().map(|j| j.title.as_str()).collect();
    // The case's own honesty: an answer that is the whole world cannot
    // tell a bound filter from an ignored one.
    assert!(
        expect.len() < titles.len() && expect.iter().all(|t| titles.contains(t)),
        "case error: {expect:?} must be a strict subset of the seed {titles:?}"
    );

    let (rows, total) = repo.list_jobs(&filter, 100, 0).await.expect("list");
    let mut got: Vec<&str> = rows.iter().map(|j| j.title.as_str()).collect();
    got.sort_unstable();
    let mut want = expect.to_vec();
    want.sort_unstable();
    assert_eq!(got, want, "{adapter}: rows for {filter:?}");
    assert_eq!(
        total,
        expect.len() as i64,
        "{adapter}: total for {filter:?} counts the rows' set"
    );
}

async fn kind<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    let filter = JobFilter {
        kind: Some("field-service".into()),
        ..Default::default()
    };
    narrows(repo, adapter, filter, &["field-a", "field-b"]).await;
}

async fn kind_prefix<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    let filter = JobFilter {
        kind_prefix: Some("field".into()),
        ..Default::default()
    };
    narrows(repo, adapter, filter, &["field-a", "field-b", "field-trip"]).await;
}

/// A prefix is text, not a pattern: `_` and `%` match only themselves.
async fn kind_prefix_is_literal_text<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    for prefix in ["fi_ld", "%", "field%", "\\"] {
        let filter = JobFilter {
            kind_prefix: Some(prefix.into()),
            ..Default::default()
        };
        narrows(repo, adapter, filter, &[]).await;
    }
}

async fn kinds<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    let filter = JobFilter {
        kinds: Some(vec!["pr-train".into(), "field-trip".into()]),
        ..Default::default()
    };
    narrows(
        repo,
        adapter,
        filter,
        &["train", "train-plain", "train-blank", "field-trip"],
    )
    .await;
    // An empty set keeps nothing — never "no filter" (cc76f755).
    let filter = JobFilter {
        kinds: Some(vec![]),
        ..Default::default()
    };
    narrows(repo, adapter, filter, &[]).await;
}

async fn department<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    let filter = JobFilter {
        department: Some(DepartmentFilter {
            code: "sales".into(),
            declaring_kinds: vec!["pr-train".into()],
        }),
        ..Default::default()
    };
    narrows(
        repo,
        adapter,
        filter,
        &["train-plain", "train-blank", "sales-own"],
    )
    .await;
    // No kind declares it: only the packets naming it.
    let filter = JobFilter {
        department: Some(DepartmentFilter {
            code: "sales".into(),
            declaring_kinds: vec![],
        }),
        ..Default::default()
    };
    narrows(repo, adapter, filter, &["sales-own"]).await;
}

async fn status<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    for (status, expect) in [
        (JobStatus::Closed, &["field-b", "closed-undated"][..]),
        (JobStatus::Cancelled, &["field-trip"][..]),
        (JobStatus::Draft, &["draft"][..]),
    ] {
        let filter = JobFilter {
            status: Some(status),
            ..Default::default()
        };
        narrows(repo, adapter, filter, expect).await;
    }
}

/// Live OR closed on/after the date — and it replaces the status
/// equality rather than ANDing with it.
async fn closed_since<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    let all: Vec<String> = world().into_iter().map(|j| j.title).collect();
    let kept: Vec<&str> = all
        .iter()
        .map(String::as_str)
        .filter(|t| !matches!(*t, "field-trip" | "closed-undated"))
        .collect();
    for status in [None, Some(JobStatus::Open), Some(JobStatus::Closed)] {
        let filter = JobFilter {
            status,
            closed_since: Some(day(10)),
            ..Default::default()
        };
        narrows(repo, adapter, filter, &kept).await;
    }
}

async fn priority<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    for (priority, expect) in [
        (Priority::Urgent, &["train"][..]),
        (Priority::Emergency, &["emergency"][..]),
        (Priority::Scheduled, &[][..]),
    ] {
        let filter = JobFilter {
            priority: Some(priority),
            ..Default::default()
        };
        narrows(repo, adapter, filter, expect).await;
    }
}

async fn owner_id<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    let filter = JobFilter {
        owner_id: Some("emp-b".into()),
        ..Default::default()
    };
    narrows(repo, adapter, filter, &["field-a"]).await;
}

/// The subject's id, whatever its kind.
async fn subject_id<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    let filter = JobFilter {
        subject_id: Some("acct-1".into()),
        ..Default::default()
    };
    narrows(repo, adapter, filter, &["field-a", "field-b", "asset-acct"]).await;
}

async fn waiting_on<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    let filter = JobFilter {
        waiting_on: Some(BLOCKER.into()),
        ..Default::default()
    };
    narrows(repo, adapter, filter, &["waits-full", "waits-prefix"]).await;
}

async fn metadata_contains<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    let filter = JobFilter {
        metadata_contains: Some(serde_json::json!({"lane": "fast"})),
        ..Default::default()
    };
    narrows(repo, adapter, filter, &["finding"]).await;
}

async fn metadata_has<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    let filter = JobFilter {
        metadata_has: Some("estate_finding".into()),
        ..Default::default()
    };
    narrows(repo, adapter, filter, &["finding", "finding-null"]).await;
}

async fn scope<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    for (scope, expect) in [
        (JobScope::OwnerIs("emp-b".into()), &["field-a"][..]),
        (
            JobScope::OwnerIn(vec!["emp-b".into(), "emp-c".into()]),
            &["field-a", "emergency"][..],
        ),
        // Account and employee subjects only: the asset carrying the
        // same id is outside the territory.
        (
            JobScope::AccountIn(vec!["acct-1".into()]),
            &["field-a", "field-b"][..],
        ),
        (JobScope::None, &[][..]),
    ] {
        let filter = JobFilter {
            scope,
            ..Default::default()
        };
        narrows(repo, adapter, filter, expect).await;
    }
}

async fn partition<R: JobsRepository>(repo: &R, adapter: &str) {
    seed_world(repo).await;
    for (partition, expect) in [
        (Partition::Simulated, &["sim"][..]),
        (Partition::Shadow, &["shadow"][..]),
    ] {
        let filter = JobFilter {
            partition: Some(partition),
            ..Default::default()
        };
        narrows(repo, adapter, filter, expect).await;
    }
}

/// `limit`/`offset` choose a window; `total` is always the size of the
/// filtered world, never of the window — the number a reader compares
/// its rows to before trusting a count (a limit is not a filter). And
/// the window is the same on both adapters, because the order is:
/// `opened_on` desc, then the admission instant desc, then id asc.
async fn a_page_is_a_window_and_the_total_is_the_world<R: JobsRepository>(repo: &R, adapter: &str) {
    let fixed = |n: u128| JobId::from_uuid(Uuid::from_u128(n));
    let rows = [
        // (title, opened day, admitted at, id, kind)
        ("oldest-day", 20, 40, fixed(6), "pr-train"),
        ("early", 21, 10, fixed(5), "backlog-item"),
        ("tie-high-id", 21, 20, fixed(2), "pr-train"),
        ("tie-low-id", 21, 20, fixed(1), "backlog-item"),
        ("late", 21, 30, fixed(4), "pr-train"),
        ("newest-day", 22, 0, fixed(3), "backlog-item"),
    ];
    for (title, opened, at, id, kind) in rows {
        let job = with(title, |j| {
            j.id = id;
            j.opened_on = day(opened);
            j.kind = kind.into();
        });
        repo.create_job_at(&job, instant(at), &[])
            .await
            .expect("seed");
    }
    let order = [
        "newest-day",
        "late",
        "tie-low-id",
        "tie-high-id",
        "early",
        "oldest-day",
    ];
    let titles = |jobs: Vec<Job>| jobs.into_iter().map(|j| j.title).collect::<Vec<_>>();
    let everything = JobFilter::default();

    let (all, total) = repo.list_jobs(&everything, 100, 0).await.expect("list");
    assert_eq!(titles(all), order, "{adapter}: the order");
    assert_eq!(total, 6, "{adapter}");

    let mut walked = Vec::new();
    for offset in [0, 2, 4] {
        let (page, total) = repo.list_jobs(&everything, 2, offset).await.expect("list");
        assert_eq!(page.len(), 2, "{adapter}: page at {offset}");
        assert_eq!(total, 6, "{adapter}: a page's total is the world's");
        walked.extend(titles(page));
    }
    assert_eq!(walked, order, "{adapter}: the pages tile the list");

    for offset in [6, 100] {
        let (page, total) = repo.list_jobs(&everything, 2, offset).await.expect("list");
        assert!(page.is_empty(), "{adapter}: past the end at {offset}");
        assert_eq!(total, 6, "{adapter}: past the end still counts the world");
    }

    // Filtered: the total is the filter's world, not the page and not
    // the store.
    let trains = JobFilter {
        kind: Some("pr-train".into()),
        ..Default::default()
    };
    let (page, total) = repo.list_jobs(&trains, 1, 0).await.expect("list");
    assert_eq!(titles(page), ["late"], "{adapter}");
    assert_eq!(total, 3, "{adapter}: the filtered total");
    let (page, _) = repo.list_jobs(&trains, 1, 2).await.expect("list");
    assert_eq!(titles(page), ["oldest-day"], "{adapter}");
}
