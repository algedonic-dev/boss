//! `JobsRepository::kind_ledger` against Postgres is the pure fold of
//! what `list_jobs` hands the same scope.
//!
//! WHY IT EXISTS (backlogs 112c0535, 5eacf6db, 2026-09-27). The ledger
//! behind `GET /api/jobs/kinds` — packets ever, open by version, the
//! newest terminal per kind — is DEFINED as `kind_ledger::from_jobs`
//! over the packets in scope. The Postgres adapter answers it from two
//! grouped statements instead of fetching every row, and spells the
//! scope as its own SQL, so it is a second statement of both rules and
//! is pinned here against the first: for every scope shape, the
//! adapter's ledger equals the fold over the list's rows.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::kind_ledger;
use boss_jobs::port::{JobFilter, JobScope, JobsRepository};
use boss_testing::TestDb;
use chrono::NaiveDate;
use serde_json::json;
use uuid::Uuid;

/// `(kind, version, owner, subject_kind, subject_id, status, closed day)`.
type Row = (
    &'static str,
    i32,
    &'static str,
    &'static str,
    &'static str,
    JobStatus,
    u32,
);

const PACKETS: &[Row] = &[
    ("brew-day", 1, "emp-a", "asset", "FV-1", JobStatus::Open, 0),
    ("brew-day", 2, "emp-a", "asset", "FV-1", JobStatus::Open, 0),
    ("brew-day", 2, "emp-b", "asset", "FV-2", JobStatus::Open, 0),
    (
        "brew-day",
        1,
        "emp-a",
        "asset",
        "FV-1",
        JobStatus::Closed,
        20,
    ),
    (
        "brew-day",
        2,
        "emp-b",
        "asset",
        "FV-2",
        JobStatus::Closed,
        23,
    ),
    (
        "brew-day",
        2,
        "emp-b",
        "asset",
        "FV-2",
        JobStatus::Cancelled,
        0,
    ),
    ("sale", 3, "emp-a", "account", "acct-1", JobStatus::Open, 0),
    (
        "sale",
        3,
        "emp-c",
        "account",
        "acct-1",
        JobStatus::Closed,
        22,
    ),
    (
        "cellar-check",
        1,
        "emp-c",
        "employee",
        "emp-a",
        JobStatus::Closed,
        21,
    ),
];

fn job(n: usize, (kind, version, owner, sk, sid, status, closed): Row) -> Job {
    let id = Uuid::parse_str(&format!("112c0535-0000-0000-0001-{n:012}")).expect("uuid");
    Job {
        id: JobId::from_uuid(id),
        workflow_version: version,
        status,
        closed_on: (status == JobStatus::Closed)
            .then(|| NaiveDate::from_ymd_opt(2026, 9, closed).expect("day")),
        metadata: if status == JobStatus::Closed {
            json!({ "outcome": format!("outcome-{n}") })
        } else {
            serde_json::Value::Null
        },
        ..Job::new(
            kind.to_string(),
            Subject::new(sk, sid),
            format!("packet {n}"),
            owner,
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 1).expect("day"),
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ledger_is_the_fold_of_the_lists_rows_under_every_scope_in_postgres() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    for (n, p) in PACKETS.iter().enumerate() {
        repo.create_job(&job(n, *p)).await.expect("create");
    }

    let scopes = [
        JobScope::All,
        JobScope::None,
        JobScope::OwnerIs("emp-a".into()),
        JobScope::OwnerIn(vec!["emp-b".into(), "emp-c".into()]),
        JobScope::AccountIn(vec!["acct-1".into(), "emp-a".into()]),
    ];
    for scope in scopes {
        let got = repo.kind_ledger(&scope).await.expect("ledger");
        let filter = JobFilter {
            scope: scope.clone(),
            ..Default::default()
        };
        let (rows, _) = repo.list_jobs(&filter, i64::MAX, 0).await.expect("list");
        assert_eq!(got, kind_ledger::from_jobs(&rows), "{scope:?}");
    }

    // The fixture's own answer, so equality with the fold cannot pass
    // by both being empty.
    let all = repo.kind_ledger(&JobScope::All).await.expect("ledger");
    let kinds: Vec<&str> = all.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, ["brew-day", "cellar-check", "sale"]);
    let brew = &all[0];
    assert_eq!(brew.packets, 6, "cancelled counts toward packets ever");
    assert_eq!(brew.open, 3);
    assert_eq!(
        brew.open_by_version,
        std::collections::BTreeMap::from([(1, 1), (2, 2)])
    );
    let t = brew.newest_terminal.as_ref().expect("a terminal");
    assert_eq!(t.id, "112c0535-0000-0000-0001-000000000004");
    assert_eq!(t.outcome.as_deref(), Some("outcome-4"));

    let own = repo
        .kind_ledger(&JobScope::OwnerIs("emp-a".into()))
        .await
        .expect("ledger");
    let brew = own.iter().find(|r| r.kind == "brew-day").expect("brew-day");
    assert_eq!(brew.packets, 3);
    assert_eq!(
        brew.newest_terminal.as_ref().map(|t| t.id.as_str()),
        Some("112c0535-0000-0000-0001-000000000003"),
        "the newest terminal the caller may read, not the store's"
    );
    assert!(
        repo.kind_ledger(&JobScope::None)
            .await
            .expect("ledger")
            .is_empty()
    );
}
