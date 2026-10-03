//! `JobsRepository::newest_closed_job` answers the same packet on both
//! adapters — with and without a department — mechanism C of design
//! 3036296f, "adapters agree".
//!
//! WHY THE DEPARTMENT IS AN ARGUMENT (backlog a22311a1). A department's
//! readiness read reports, per protocol, the newest packet that closed.
//! It asked by kind alone while the department jobs view keeps a packet
//! by the packet-department rule (`DepartmentFilter::keeps`): a packet
//! naming another department is that department's, whatever its kind.
//! Once 61 platform kinds declared `it`, IT's readiness would have
//! named, as its newest terminal, a backlog-item finance's page audit
//! filed and finance's view counts. The newest terminal now passes the
//! same filter the view does, on both adapters.
//!
//! Each case states its answer as a title, and each adapter is held to
//! that stated answer — not merely to the other adapter.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::InMemoryJobs;
use boss_jobs::port::{DepartmentFilter, JobsRepository};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryJobs::new(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (boss_jobs::PgJobs::new(db.pool.clone()), db)
        },
    }
    cases {
        newest_by_close_date_without_a_department,
        a_packet_naming_another_department_is_not_this_ones,
        a_kind_no_one_declares_answers_only_packets_naming_the_department,
    }
}

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, d).expect("a September day")
}

fn instant(secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 20, 12, 0, 0)
        .single()
        .expect("an instant")
        + Duration::seconds(secs)
}

fn closed(title: &str, closed_on: u32, metadata: serde_json::Value) -> Job {
    Job {
        id: JobId::new(),
        status: JobStatus::Closed,
        closed_on: Some(day(closed_on)),
        metadata,
        ..Job::new(
            "backlog-item",
            Subject::new("custom", "s-0"),
            title,
            "emp-a",
            Priority::Standard,
            day(1),
        )
    }
}

/// Three closed backlog-items: the newest names finance, the middle
/// names nothing, the oldest names IT itself — plus a cancelled one,
/// newest of all, which is terminal but not closed.
async fn seed<R: JobsRepository>(repo: &R) {
    let mut cancelled = closed("cancelled-newest", 20, serde_json::json!({}));
    cancelled.status = JobStatus::Cancelled;
    let rows = [
        closed(
            "finance-named",
            15,
            serde_json::json!({"department": "finance"}),
        ),
        closed("unnamed", 12, serde_json::json!({})),
        closed("it-named", 9, serde_json::json!({"department": "it"})),
        cancelled,
    ];
    for (i, job) in rows.iter().enumerate() {
        repo.create_job_at(job, instant(i as i64), &[])
            .await
            .unwrap_or_else(|e| panic!("seed {}: {e}", job.title));
    }
}

async fn newest<R: JobsRepository>(
    repo: &R,
    department: Option<&DepartmentFilter>,
) -> Option<String> {
    repo.newest_closed_job("backlog-item", department)
        .await
        .expect("newest closed")
        .map(|j| j.title)
}

async fn newest_by_close_date_without_a_department<R: JobsRepository>(repo: &R, adapter: &str) {
    seed(repo).await;
    assert_eq!(
        newest(repo, None).await.as_deref(),
        Some("finance-named"),
        "{adapter}: no department is every closed packet of the kind"
    );
}

async fn a_packet_naming_another_department_is_not_this_ones<R: JobsRepository>(
    repo: &R,
    adapter: &str,
) {
    seed(repo).await;
    let it = DepartmentFilter {
        code: "it".into(),
        declaring_kinds: vec!["backlog-item".into()],
    };
    assert_eq!(
        newest(repo, Some(&it)).await.as_deref(),
        Some("unnamed"),
        "{adapter}: finance's packet is finance's, though IT declares its kind"
    );
    let finance = DepartmentFilter {
        code: "finance".into(),
        declaring_kinds: vec![],
    };
    assert_eq!(
        newest(repo, Some(&finance)).await.as_deref(),
        Some("finance-named"),
        "{adapter}: the packet's own word puts it in finance"
    );
}

async fn a_kind_no_one_declares_answers_only_packets_naming_the_department<R: JobsRepository>(
    repo: &R,
    adapter: &str,
) {
    seed(repo).await;
    let it_undeclared = DepartmentFilter {
        code: "it".into(),
        declaring_kinds: vec![],
    };
    assert_eq!(
        newest(repo, Some(&it_undeclared)).await.as_deref(),
        Some("it-named"),
        "{adapter}: only the packet naming it"
    );
    let nobody = DepartmentFilter {
        code: "warehouse".into(),
        declaring_kinds: vec![],
    };
    assert_eq!(
        newest(repo, Some(&nobody)).await,
        None,
        "{adapter}: none closed in a department nobody names"
    );
}
