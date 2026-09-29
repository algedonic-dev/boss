//! The agent-run log answers the same on both `AgentRunLog` adapters —
//! reliability mechanism C of design 3036296f, "adapters agree", on the
//! second port it reaches (backlog be459ab9; the jobs store's list
//! filters were the first, `the_adapters_agree_on_every_list_filter_pg.rs`).
//!
//! WHY IT EXISTS. The port's behaviour was stated twice and held to
//! itself by nothing: `agent_runs_port.rs` asks its questions of the
//! in-memory adapter only, and `agent_runs_pg.rs` asks the SCHEMA's
//! questions of Postgres only. The first run of this suite found the
//! gap that arrangement hid. A `RunFilter` with no `limit` is "don't
//! filter" on the port and in the in-memory adapter, while Postgres
//! read it as `LIMIT 200` and clamped any limit into 1..=1000. So
//! `GET /api/agent-runs/cost`, which passes the caller's filter
//! straight through, summed the newest 200 runs and called it the
//! total — measured 2026-09-28 against the system of record: the
//! unfiltered roll-up answered `runs: 200` while `?limit=1000` listed
//! 1000 — and the budget door's hour-window read (`agent_budget`)
//! would have undercounted the same way past 200 runs in an hour.
//! Every in-memory test of both was right, which is why nothing
//! noticed. Case `no_limit_is_no_ceiling`.
//!
//! THE SHAPE, the jobs suite's, so a case cannot pass by accident:
//! - Each case states its answer as run ids and each adapter is held to
//!   that stated answer, not merely to the other adapter.
//! - A filter's answer must be a STRICT subset of the seed: an ignored
//!   filter answers the whole seed.
//! - `every_field_is_a_case` destructures `RunFilter` field by field, so
//!   a field added to the port does not compile here until it is named.
//!
//! What is NOT compared, and why: the rate card and so every price. The
//! card is registry data a migration seeds into Postgres, and the
//! in-memory adapter holds whatever card it is handed, so the two
//! answer different cards by construction; `agent_runs_pg.rs` pins the
//! seeded card to the published page. Every run here names its model
//! in the legacy colon form, so no case depends on an `agents` row
//! either.

use boss_core::actor::ActorId;
use boss_jobs::agent_runs::{
    AgentRunError, AgentRunLog, InMemoryAgentRuns, NewAgentRun, RunFilter, RunOutcome, TokenUsage,
    WorkProfile,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use uuid::Uuid;

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryAgentRuns::new(vec![]), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (boss_jobs::agent_runs::PgAgentRuns::new(db.pool.clone()), db)
        },
    }
    cases {
        job_id,
        branch,
        actor_id,
        since,
        limit,
        no_limit_is_no_ceiling,
        a_count_is_the_filters_answer_whatever_the_limit,
        newest_finish_first,
        a_retried_record_is_the_run_already_held,
        a_malformed_run_is_refused_and_writes_nothing,
        a_profile_is_replaced_and_read_in_a_half_open_window,
    }
}

/// A field added to `RunFilter` is a compile error here until it is
/// named — and each name points at the case that covers it.
const _: fn(RunFilter) = |filter| {
    let RunFilter {
        job_id: _,   // case `job_id`
        branch: _,   // case `branch`
        actor_id: _, // case `actor_id`
        since: _,    // case `since`
        limit: _,    // cases `limit`, `no_limit_is_no_ceiling`
    } = filter;
};

/// The packet two of the seeded runs worked for.
const PACKET: Uuid = Uuid::from_u128(0xbe45_9ab9_0000_4000_8000_0000_0000_0001);

/// Whole seconds: Postgres holds microseconds, so an instant finer than
/// that would read back as a different value on one adapter only.
fn instant(secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 28, 1, 0, 0)
        .single()
        .expect("an instant")
        + Duration::seconds(secs)
}

/// A finished run by `claude:opus-5`, one minute long, finishing
/// `finished` seconds after the fixture's epoch.
fn run(run_id: &str, finished: i64) -> NewAgentRun {
    NewAgentRun {
        run_id: run_id.into(),
        actor_id: ActorId::agent("claude", "opus-5"),
        model: None,
        started_at: instant(finished - 60),
        finished_at: instant(finished),
        outcome: RunOutcome::Success,
        error: None,
        tokens: TokenUsage::TotalOnly { total: 1_000 },
        tool_calls: 3,
        job_id: None,
        branch: None,
        detail: serde_json::json!({}),
    }
}

fn with(run_id: &str, finished: i64, change: impl FnOnce(&mut NewAgentRun)) -> NewAgentRun {
    let mut r = run(run_id, finished);
    change(&mut r);
    r
}

fn filer() -> ActorId {
    ActorId::agent("claude", "opus-5[1m]")
}

/// The world every filter case narrows. Each row exists to sit on one
/// side of some filter's line; the comment says which.
fn world() -> Vec<NewAgentRun> {
    vec![
        // job_id: two runs for the packet, one for another
        with("for-packet-a", 10, |r| r.job_id = Some(PACKET)),
        with("for-packet-b", 20, |r| r.job_id = Some(PACKET)),
        with("for-other", 30, |r| r.job_id = Some(Uuid::from_u128(7))),
        // branch: equality, not a prefix
        with("on-branch", 40, |r| r.branch = Some("fix/a-thing".into())),
        with("on-longer-branch", 50, |r| {
            r.branch = Some("fix/a-thing-2".into())
        }),
        // actor_id: another model is another actor
        with("by-haiku", 60, |r| {
            r.actor_id = ActorId::agent("claude", "haiku-4-5")
        }),
        // since: finishing exactly AT the cutoff is inside it
        run("at-cutoff", 100),
        run("after-cutoff", 110),
    ]
}

async fn seed<L: AgentRunLog>(log: &L, runs: &[NewAgentRun]) {
    for r in runs {
        log.record_run(r, &filer())
            .await
            .unwrap_or_else(|e| panic!("seed {}: {e}", r.run_id));
    }
}

fn ids(runs: &[boss_jobs::agent_runs::AgentRun]) -> Vec<&str> {
    runs.iter().map(|r| r.run.run_id.as_str()).collect()
}

/// List the seeded world through `filter` and hold the answer to
/// `expect`, in the port's order (newest finish first).
async fn narrows<L: AgentRunLog>(log: &L, adapter: &str, filter: RunFilter, expect: &[&str]) {
    let world = world();
    let all: Vec<&str> = world.iter().map(|r| r.run_id.as_str()).collect();
    // The case's own honesty: an answer that is the whole world cannot
    // tell a bound filter from an ignored one.
    assert!(
        expect.len() < all.len() && expect.iter().all(|id| all.contains(id)),
        "case error: {expect:?} must be a strict subset of the seed {all:?}"
    );
    let got = log.list_runs(&filter).await.expect("list");
    assert_eq!(ids(&got), expect, "{adapter}: rows for {filter:?}");
}

async fn job_id<L: AgentRunLog>(log: &L, adapter: &str) {
    seed(log, &world()).await;
    let filter = RunFilter {
        job_id: Some(PACKET),
        ..Default::default()
    };
    narrows(log, adapter, filter, &["for-packet-b", "for-packet-a"]).await;
}

async fn branch<L: AgentRunLog>(log: &L, adapter: &str) {
    seed(log, &world()).await;
    let filter = RunFilter {
        branch: Some("fix/a-thing".into()),
        ..Default::default()
    };
    narrows(log, adapter, filter, &["on-branch"]).await;
}

async fn actor_id<L: AgentRunLog>(log: &L, adapter: &str) {
    seed(log, &world()).await;
    let filter = RunFilter {
        actor_id: Some("claude:haiku-4-5".into()),
        ..Default::default()
    };
    narrows(log, adapter, filter, &["by-haiku"]).await;
}

async fn since<L: AgentRunLog>(log: &L, adapter: &str) {
    seed(log, &world()).await;
    let filter = RunFilter {
        since: Some(instant(100)),
        ..Default::default()
    };
    narrows(log, adapter, filter, &["after-cutoff", "at-cutoff"]).await;
}

/// A limit keeps the NEWEST n of the filter's answer, and a limit of
/// zero or below keeps nothing — never "one", and never "no limit".
async fn limit<L: AgentRunLog>(log: &L, adapter: &str) {
    seed(log, &world()).await;
    let filter = RunFilter {
        limit: Some(2),
        ..Default::default()
    };
    narrows(log, adapter, filter, &["after-cutoff", "at-cutoff"]).await;
    let filter = RunFilter {
        job_id: Some(PACKET),
        limit: Some(1),
        ..Default::default()
    };
    narrows(log, adapter, filter, &["for-packet-b"]).await;
    for n in [0, -1] {
        let filter = RunFilter {
            limit: Some(n),
            ..Default::default()
        };
        narrows(log, adapter, filter, &[]).await;
    }
}

/// `None` is "don't filter" — the whole answer, however long. The cost
/// roll-up and the budget door both read through a filter with no
/// limit, and each is a SUM: a ceiling the caller never asked for is a
/// wrong total that looks like a right one.
async fn no_limit_is_no_ceiling<L: AgentRunLog>(log: &L, adapter: &str) {
    // One past the ceiling Postgres used to apply unasked (200).
    const RUNS: i64 = 201;
    let runs: Vec<NewAgentRun> = (0..RUNS)
        .map(|i| run(&format!("run-{i:03}"), i * 10))
        .collect();
    seed(log, &runs).await;

    let all = log.list_runs(&RunFilter::default()).await.expect("list");
    assert_eq!(
        all.len() as i64,
        RUNS,
        "{adapter}: an unfiltered read is every run"
    );

    // The budget door's own shape: one actor, a window, no limit.
    let window = RunFilter {
        actor_id: Some("claude:opus-5".into()),
        since: Some(instant(0)),
        ..Default::default()
    };
    let held = log.list_runs(&window).await.expect("list");
    assert_eq!(
        held.len() as i64,
        RUNS,
        "{adapter}: a window read is every run in the window"
    );
}

/// The listing's `total` (backlog 11a0998a): every run the filter
/// matches, the limit aside, so a reader can tell a page from the whole
/// answer. Stated as numbers off `world()`, one per filter field.
async fn a_count_is_the_filters_answer_whatever_the_limit<L: AgentRunLog>(log: &L, adapter: &str) {
    seed(log, &world()).await;
    let whole = world().len() as u64;
    let cases: [(RunFilter, u64); 6] = [
        (RunFilter::default(), whole),
        (
            RunFilter {
                limit: Some(1),
                ..Default::default()
            },
            whole,
        ),
        (
            RunFilter {
                limit: Some(0),
                ..Default::default()
            },
            whole,
        ),
        (
            RunFilter {
                job_id: Some(PACKET),
                limit: Some(1),
                ..Default::default()
            },
            2,
        ),
        (
            RunFilter {
                branch: Some("fix/a-thing".into()),
                actor_id: Some("claude:opus-5".into()),
                ..Default::default()
            },
            1,
        ),
        (
            RunFilter {
                since: Some(instant(100)),
                ..Default::default()
            },
            2,
        ),
    ];
    for (filter, want) in cases {
        let got = log.count_runs(&filter).await.expect("count");
        assert_eq!(got, want, "{adapter}: count for {filter:?}");
    }
}

/// Newest finish first, `run_id` ascending on a tie — a total order, so
/// a limit cuts a busy window at the same point on either store.
async fn newest_finish_first<L: AgentRunLog>(log: &L, adapter: &str) {
    seed(
        log,
        &[
            run("oldest", 10),
            run("tie-b", 20),
            run("tie-a", 20),
            run("newest", 30),
        ],
    )
    .await;
    let got = log.list_runs(&RunFilter::default()).await.expect("list");
    assert_eq!(
        ids(&got),
        ["newest", "tie-a", "tie-b", "oldest"],
        "{adapter}: the order"
    );
}

/// `run_id` is the idempotency key: a second report of a held run
/// records nothing and answers the run already held, not the retry's
/// body.
async fn a_retried_record_is_the_run_already_held<L: AgentRunLog>(log: &L, adapter: &str) {
    let first = log
        .record_run(&run("once", 10), &filer())
        .await
        .expect("records");
    assert!(first.recorded, "{adapter}: the first report is recorded");

    let retry = with("once", 10, |r| r.tool_calls = 99);
    let again = log.record_run(&retry, &filer()).await.expect("answers");
    assert!(!again.recorded, "{adapter}: a retry records nothing");
    assert_eq!(
        again.run, first.run,
        "{adapter}: the held run, not the retry"
    );

    let held = log.list_runs(&RunFilter::default()).await.expect("list");
    assert_eq!(ids(&held), ["once"], "{adapter}: one run, not two");
    assert_eq!(held[0].run.tool_calls, 3, "{adapter}");
}

/// A run the port refuses is refused by both, as a bad request, and
/// leaves no row.
async fn a_malformed_run_is_refused_and_writes_nothing<L: AgentRunLog>(log: &L, adapter: &str) {
    let refused = [
        with("", 10, |_| {}),
        with("ends-before-it-starts", 10, |r| r.started_at = instant(20)),
        with("not-an-agent", 10, |r| {
            r.actor_id = ActorId::Automation("platform".into())
        }),
        with("blank-model", 10, |r| r.model = Some(" ".into())),
    ];
    for r in &refused {
        match log.record_run(r, &filer()).await {
            Err(AgentRunError::BadRequest(_)) => {}
            other => panic!("{adapter}: {:?} answered {other:?}", r.run_id),
        }
    }
    let held = log.list_runs(&RunFilter::default()).await.expect("list");
    assert!(held.is_empty(), "{adapter}: {:?}", ids(&held));
}

/// A re-report replaces a run's profile; a window is `[since, until)`,
/// read oldest first; and a window that holds nothing by construction
/// is a bad request, not a quiet week.
async fn a_profile_is_replaced_and_read_in_a_half_open_window<L: AgentRunLog>(
    log: &L,
    adapter: &str,
) {
    let profile = |tool_calls: u64| WorkProfile {
        tool_calls,
        ..WorkProfile::default()
    };
    for (run_id, at, calls) in [
        ("before", 0, 1),
        ("at-since", 10, 2),
        ("replaced", 15, 3),
        ("at-until", 30, 4),
    ] {
        log.record_profile(run_id, &profile(calls), instant(at))
            .await
            .expect("records");
    }
    // The re-report: a longer transcript, read later.
    log.record_profile("replaced", &profile(30), instant(20))
        .await
        .expect("replaces");

    let got = log
        .list_profiles(instant(10), instant(30))
        .await
        .expect("lists");
    let read: Vec<(&str, i64, u64)> = got
        .iter()
        .map(|p| {
            (
                p.run_id.as_str(),
                (p.recorded_at - instant(0)).num_seconds(),
                p.profile.tool_calls,
            )
        })
        .collect();
    assert_eq!(
        read,
        [("at-since", 10, 2), ("replaced", 20, 30)],
        "{adapter}: the window's profiles"
    );

    for (since, until) in [(instant(30), instant(30)), (instant(31), instant(30))] {
        match log.list_profiles(since, until).await {
            Err(AgentRunError::BadRequest(_)) => {}
            other => panic!("{adapter}: [{since}, {until}) answered {other:?}"),
        }
    }
    match log.record_profile(" ", &profile(1), instant(0)).await {
        Err(AgentRunError::BadRequest(_)) => {}
        other => panic!("{adapter}: a blank run id answered {other:?}"),
    }
}
