//! `GET /api/jobs/assignments?for=me` — the viewer's own queue, whoever
//! the registry says the viewer is (backlog 74569e94, design ea906603
//! Q2, decided by David 2026-09-27).
//!
//! Measured the day the item was filed: the bare read as
//! `claude@algedonic.dev` answered 0 while `assignee_id=agent-claude`
//! answered 199 — the same actor under two spellings — and the only
//! thing that knew both was `boss orient`, on the CLI. The top board's
//! NEEDS YOU row needs the same answer in a browser, so the expansion
//! moved into the jobs API. What this pins, through the real router:
//!
//! 1. `for=me` reads for the viewer AND every id the agents registry
//!    ties to it, plus the roles the viewer holds (the request's
//!    platform role and the registry row's), each row once — and says
//!    whom it read for, in `for`.
//! 2. A viewer nobody named is refused, never answered with an empty
//!    queue that reads as "nothing needs you".
//! 3. A registry that cannot be read is a 503, never the viewer read
//!    alone as if it had no aliases.
//! 4. A `for=` the endpoint does not know, or `for=me` beside an
//!    explicit selector, is a 400 — never silently the other read.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publish::PublishMode;
use boss_core::publisher::DomainPublisher;
use boss_core::publisher::EventStamp;
use boss_jobs::agent_budget::BudgetDoor;
use boss_jobs::agent_runs::InMemoryAgentRuns;
use boss_jobs::agents::{
    AgentInput, AgentRow, AgentsBatchOutcome, AgentsError, AgentsRegistry, InMemoryAgents,
};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const AGENT: &str = "agent-claude";
const ALIAS: &str = "claude@algedonic.dev";

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

fn user_header(id: &str, role: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

/// A registry that cannot answer.
struct DarkRegistry;

#[async_trait]
impl AgentsRegistry for DarkRegistry {
    async fn resolve_login(&self, _: &str) -> Result<Option<String>, AgentsError> {
        Err(AgentsError::Storage("registry dark".into()))
    }
    async fn list(&self) -> Result<Vec<AgentRow>, AgentsError> {
        Err(AgentsError::Storage("registry dark".into()))
    }
    async fn publish(
        &self,
        _: &[AgentInput],
        _: PublishMode,
        _: &EventStamp,
    ) -> Result<AgentsBatchOutcome, AgentsError> {
        Err(AgentsError::Storage("registry dark".into()))
    }
    async fn list_automations(
        &self,
    ) -> Result<Vec<boss_jobs::agents::AutomationActor>, AgentsError> {
        Err(AgentsError::Storage("registry dark".into()))
    }
    async fn declare_automations(
        &self,
        _: &[boss_jobs::agents::AutomationActor],
        _: &EventStamp,
    ) -> Result<boss_jobs::agents::AutomationsSeedOutcome, AgentsError> {
        Err(AgentsError::Storage("registry dark".into()))
    }
}

fn app(agents: Arc<dyn AgentsRegistry>) -> (axum::Router, Arc<InMemoryJobs>) {
    app_with_reporter(agents, None)
}

fn app_with_reporter(
    agents: Arc<dyn AgentsRegistry>,
    reporter: Option<Arc<boss_policy_client::role_guard::RoleGuardReporter>>,
) -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow("guest", Action::Read, Resource::job(), Scope::All)
            // A NARROWED scope — its own packets only (backlog e2519eca
            // item 2): it cannot read the runs whole.
            .allow("sales", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        role_guards: reporter,
        agent_budget: Some(Arc::new(BudgetDoor {
            agents,
            runs: Arc::new(InMemoryAgentRuns::new(Vec::new())),
        })),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

#[tokio::test]
async fn for_me_reports_role_selection_without_dropping_alias_work() {
    use boss_policy_client::role_guard::RoleGuardReporter;
    use boss_policy_client::role_reader::{RegistryRoles, RoleSnapshotClock, SnapshotRoleReader};
    use boss_policy_client::role_reporting::{ReportMode, ReportTally};
    use std::time::{Duration, Instant};
    struct Clock;
    impl RoleSnapshotClock for Clock {
        fn now(&self) -> Instant {
            Instant::now()
        }
    }
    let roles = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(Clock),
    ));
    let ticket = roles.begin_refresh();
    assert!(roles.finish_refresh(ticket, Ok(RegistryRoles::from_sources(
        json!({"data":[{"id":AGENT,"aliases":[ALIAS],"role":"engineering-agent"}], "total":1}),
        json!({"data":[], "total":0}), json!([]),
    ).unwrap())));
    let tally = Arc::new(ReportTally::new(20));
    let (app, jobs) = app_with_reporter(
        registry(),
        Some(Arc::new(RoleGuardReporter::new(
            roles,
            tally.clone(),
            Arc::new(ReportMode::Report),
        ))),
    );
    seed(&jobs).await;
    let (status, body) = get(
        &app,
        "/api/jobs/assignments?for=me",
        Some(user_header(ALIAS, "platform-admin")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["claimable", "on the alias", "on the id"]);
    assert_eq!(body["for"]["ids"], json!([ALIAS, AGENT]));
    let reports = tally.snapshot();
    let observations: Vec<_> = reports
        .rows
        .iter()
        .filter(|row| row.observation.resource == "assignments-for-me-roles")
        .collect();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].observation.would_change_scope, Some(true));
    assert_eq!(observations[0].observation.would_deny, None);
}

fn registry() -> Arc<dyn AgentsRegistry> {
    Arc::new(InMemoryAgents::new().with_agent(AGENT, [ALIAS]))
}

/// One open packet with a step for each case: on the alias, on the
/// agent's id, claimable by the viewer's role, someone else's, and
/// claimable by a role the viewer does not hold.
async fn seed(jobs: &InMemoryJobs) {
    let job = Job {
        id: JobId::new(),
        status: JobStatus::Open,
        metadata: json!({}),
        ..Job::new(
            "backlog-item",
            Subject::new("custom", "s"),
            "an item",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 27).unwrap(),
        )
    };
    jobs.create_job_at(&job, t("2026-09-27T00:00:00Z"), &[])
        .await
        .unwrap();
    let step = |title: &str, order: i32, assignee: Option<&str>, status: StepStatus, md: Value| {
        let mut s = Step::new(job.id, "task", title, order);
        s.id = StepId::new();
        s.spec_slug = Some(title.replace(' ', "-"));
        s.assignee_id = assignee.map(str::to_string);
        s.status = status;
        s.metadata = md;
        s
    };
    for s in [
        step("on the alias", 0, Some(ALIAS), StepStatus::Ready, json!({})),
        step("on the id", 1, Some(AGENT), StepStatus::Active, json!({})),
        step(
            "claimable",
            2,
            None,
            StepStatus::Ready,
            json!({ "authority_role": "platform-admin" }),
        ),
        step("theirs", 3, Some("emp-david"), StepStatus::Ready, json!({})),
        step(
            "not my role",
            4,
            None,
            StepStatus::Ready,
            json!({ "authority_role": "brewer" }),
        ),
    ] {
        jobs.add_step_at(&s, t("2026-09-27T00:00:00Z"), &[])
            .await
            .unwrap();
    }
}

async fn get(app: &axum::Router, uri: &str, header: Option<String>) -> (StatusCode, Value) {
    let mut req = Request::get(uri);
    if let Some(h) = header {
        req = req.header("x-boss-user", h);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()));
    (status, v)
}

fn titles(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = v["data"]
        .as_array()
        .unwrap_or_else(|| panic!("no data in {v}"))
        .iter()
        .filter_map(|r| r["step"]["title"].as_str().map(str::to_string))
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn for_me_reads_the_viewer_its_aliases_and_the_roles_it_holds() {
    let (app, jobs) = app(registry());
    seed(&jobs).await;
    let (status, v) = get(
        &app,
        "/api/jobs/assignments?for=me",
        Some(user_header(AGENT, "platform-admin")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        titles(&v),
        ["claimable", "on the alias", "on the id"],
        "{v}"
    );
    assert_eq!(v["total"], 3, "{v}");
    assert_eq!(
        v["for"],
        json!({ "ids": [AGENT, ALIAS], "roles": ["platform-admin"] }),
        "the read says whom it read for"
    );

    // Control on the same connection: the bare selector for the id
    // alone misses the alias's row — the gap `for=me` closes.
    let (_, bare) = get(
        &app,
        &format!("/api/jobs/assignments?assignee_id={AGENT}"),
        Some(user_header(AGENT, "platform-admin")),
    )
    .await;
    assert_eq!(titles(&bare), ["on the id"], "{bare}");
}

#[tokio::test]
async fn a_viewer_nobody_named_is_refused_not_answered_empty() {
    let (app, jobs) = app(registry());
    seed(&jobs).await;
    let (status, v) = get(&app, "/api/jobs/assignments?for=me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
}

#[tokio::test]
async fn a_dark_registry_is_a_503_not_the_viewer_read_alone() {
    let (app, jobs) = app(Arc::new(DarkRegistry));
    seed(&jobs).await;
    let (status, v) = get(
        &app,
        "/api/jobs/assignments?for=me",
        Some(user_header(AGENT, "platform-admin")),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert!(v.to_string().contains("agents registry"), "{v}");
}

/// A CLAIM IN MY QUEUE SAYS WHETHER ANYONE IS ON IT (backlog e2519eca
/// item 2). The top board's OUTRANKS row names an active claim's run or
/// says it is STALLED (8e9d3723), but NEEDS YOU and MY WORK are this
/// read, which carried no run at all — so the same claim read as in
/// hand one section down. Each ACTIVE row an agent holds now carries
/// the reading the board computes, from the one predicate
/// (`boss_jobs::outranks::claim_run`): the run building it, or
/// `no-run`. A ready row carries none (nothing has claimed it), and a
/// caller who cannot read every packet is told `unread`, never `no-run`.
#[tokio::test]
async fn an_active_claim_in_my_queue_names_its_run_or_says_it_has_none() {
    let (app, jobs) = app(registry());
    let item = |title: &str| Job {
        id: JobId::new(),
        status: JobStatus::Open,
        metadata: json!({}),
        ..Job::new(
            "backlog-item",
            Subject::new("custom", "s"),
            title,
            "emp-david",
            Priority::Urgent,
            NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
        )
    };
    let worked = item("worked");
    let stalled = item("stalled");
    let waiting = item("waiting");
    for (job, status) in [
        (&worked, StepStatus::Active),
        (&stalled, StepStatus::Active),
        (&waiting, StepStatus::Ready),
    ] {
        jobs.create_job_at(job, t("2026-10-09T00:00:00Z"), &[])
            .await
            .unwrap();
        let mut build = Step::new(job.id, "task", "Build", 0);
        build.id = StepId::new();
        build.spec_slug = Some("build".into());
        build.assignee_id = Some(AGENT.into());
        build.status = status;
        jobs.add_step_at(&build, t("2026-10-09T00:00:00Z"), &[])
            .await
            .unwrap();
    }
    let run = Job {
        id: JobId::new(),
        status: JobStatus::Open,
        metadata: json!({ "packet": worked.id.to_string(), "step": "build" }),
        ..Job::new(
            "agent-run",
            Subject::new("custom", "r"),
            "builder run: worked",
            AGENT,
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
        )
    };
    jobs.create_job_at(&run, t("2026-10-09T00:00:00Z"), &[])
        .await
        .unwrap();
    let mut building = Step::new(run.id, "task", "Building", 0);
    building.id = StepId::new();
    building.spec_slug = Some("building".into());
    building.status = StepStatus::Active;
    jobs.add_step_at(&building, t("2026-10-09T00:00:00Z"), &[])
        .await
        .unwrap();

    let run_of = |v: &Value, title: &str| {
        v["data"]
            .as_array()
            .unwrap_or_else(|| panic!("no data in {v}"))
            .iter()
            .find(|r| r["job_title"] == title)
            .unwrap_or_else(|| panic!("{title} in the queue: {v}"))
            .get("run")
            .cloned()
    };
    let (status, v) = get(
        &app,
        "/api/jobs/assignments?for=me",
        Some(user_header(AGENT, "platform-admin")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        run_of(&v, "worked"),
        Some(json!({ "state": "building", "run": run.id.to_string() })),
        "{v}"
    );
    assert_eq!(
        run_of(&v, "stalled"),
        Some(json!({ "state": "no-run" })),
        "{v}"
    );
    assert_eq!(run_of(&v, "waiting"), None, "a ready row has no claim: {v}");

    // A caller who reads only its own packets would not see the run
    // another actor owns, so judging it would be a confident wrong
    // "no-run". It is told the runs were not read.
    let (status, v) = get(
        &app,
        "/api/jobs/assignments?for=me",
        Some(user_header(AGENT, "sales")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        run_of(&v, "stalled"),
        Some(json!({ "state": "unread" })),
        "{v}"
    );
}

/// One open urgent item whose `build` step `who` holds ACTIVE.
async fn claimed_item(jobs: &InMemoryJobs, title: &str, who: &str) -> Job {
    let item = Job {
        id: JobId::new(),
        status: JobStatus::Open,
        metadata: json!({}),
        ..Job::new(
            "backlog-item",
            Subject::new("custom", "s"),
            title,
            who,
            Priority::Urgent,
            NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
        )
    };
    jobs.create_job_at(&item, t("2026-10-09T00:00:00Z"), &[])
        .await
        .unwrap();
    let mut build = Step::new(item.id, "task", "Build", 0);
    build.id = StepId::new();
    build.spec_slug = Some("build".into());
    build.assignee_id = Some(who.into());
    build.status = StepStatus::Active;
    jobs.add_step_at(&build, t("2026-10-09T00:00:00Z"), &[])
        .await
        .unwrap();
    item
}

/// The `run` a titled row of a queue read carries, if any.
fn run_on(v: &Value, title: &str) -> Option<Value> {
    v["data"]
        .as_array()
        .unwrap_or_else(|| panic!("no data in {v}"))
        .iter()
        .find(|r| r["job_title"] == title)
        .unwrap_or_else(|| panic!("{title} in the queue: {v}"))
        .get("run")
        .cloned()
}

/// A PERSON'S OWN CLAIM CARRIES NO RUN, EVEN WHEN THE RUNS WERE NOT READ
/// (backlog 1927aca5, landed-review da21596d of car 181a77e3). A
/// `self`-scoped caller cannot read the runs whole, but the agents
/// registry IS read, so it is known that a person holds the step — and
/// a person's claim never has a run. It read `unread`, which `boss
/// orient` prints in NEEDS YOU and MY WORK as a run nobody could read.
#[tokio::test]
async fn a_persons_own_claim_carries_no_run_under_a_narrowed_scope() {
    let (app, jobs) = app(registry());
    claimed_item(&jobs, "mine", "emp-sam").await;
    let (status, v) = get(
        &app,
        "/api/jobs/assignments?for=me",
        Some(user_header("emp-sam", "sales")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(run_on(&v, "mine"), None, "a person's claim has no run: {v}");
}

/// A CUT PAGE OF RUNS IS DISCARDED BEFORE ITS STEPS ARE READ (backlog
/// 1927aca5, landed-review da21596d finding 2). The queue judges a
/// claim only on a WHOLE read of the open runs, and the read fetched
/// every run's steps — up to a page of them, one sequential read each —
/// and only then dropped the page as cut, so past a page of open runs
/// every `for=me` with an active row paid a thousand reads to answer
/// `unread`. Measured here on the answer AND on the store: the claim
/// still reads `unread`, and no run's steps were read for it. The
/// in-memory store answers the assignments query itself by scanning
/// every open packet's steps (the port's default; Postgres is one
/// JOIN), so the measure is against a CONTROL — an open packet that is
/// not a run, which only that scan reads: each run's steps are read no
/// more often than the control's.
#[tokio::test]
async fn a_cut_page_of_runs_reads_unread_without_reading_any_runs_steps() {
    let (app, jobs) = app(registry());
    let item = claimed_item(&jobs, "claimed", AGENT).await;
    let control = Job {
        id: JobId::new(),
        status: JobStatus::Open,
        metadata: json!({}),
        ..Job::new(
            "backlog-item",
            Subject::new("custom", "c"),
            "control",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
        )
    };
    jobs.create_job_at(&control, t("2026-10-09T00:00:00Z"), &[])
        .await
        .unwrap();
    let mut runs = Vec::new();
    // One more than the page the read takes (`MAX_LIMIT`, 1000).
    for n in 0..1001 {
        let run = Job {
            id: JobId::new(),
            status: JobStatus::Open,
            metadata: json!({ "packet": item.id.to_string(), "step": "build" }),
            ..Job::new(
                "agent-run",
                Subject::new("custom", "r"),
                format!("builder run {n}"),
                AGENT,
                Priority::Standard,
                NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
            )
        };
        jobs.create_job_at(&run, t("2026-10-09T00:00:00Z"), &[])
            .await
            .unwrap();
        runs.push(run.id);
    }
    // Only the request's reads count, not admission's.
    let before: Vec<usize> = runs.iter().map(|id| jobs.steps_reads(id)).collect();
    let control_before = jobs.steps_reads(&control.id);
    let (status, v) = get(
        &app,
        "/api/jobs/assignments?for=me",
        Some(user_header(AGENT, "platform-admin")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(run_on(&v, "claimed"), Some(json!({ "state": "unread" })));
    let scanned = jobs.steps_reads(&control.id) - control_before;
    let over: usize = runs
        .iter()
        .zip(before)
        .filter(|(id, was)| jobs.steps_reads(id) - was > scanned)
        .count();
    assert_eq!(
        over, 0,
        "runs whose steps were read for a page that was then discarded"
    );
}

#[tokio::test]
async fn an_unknown_for_or_one_beside_a_selector_is_refused() {
    let (app, _) = app(registry());
    let who = || Some(user_header(AGENT, "platform-admin"));
    let (status, v) = get(&app, "/api/jobs/assignments?for=everyone", who()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, v) = get(
        &app,
        "/api/jobs/assignments?for=me&assignee_id=emp-david",
        who(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
}

/// Five packets, one ready step each: three nominated to the agent's id
/// opened first, then two nominated to its alias opened LAST — the
/// newest nominations, which sort past any cut (backlog 09074ada).
async fn seed_past_the_cut(jobs: &InMemoryJobs) {
    let nominations = [
        (AGENT, "2026-10-01"),
        (AGENT, "2026-10-02"),
        (AGENT, "2026-10-03"),
        (ALIAS, "2026-10-04"),
        (ALIAS, "2026-10-05"),
    ];
    for (n, (who, day)) in nominations.into_iter().enumerate() {
        let opened = NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap();
        let job = Job {
            id: JobId::new(),
            status: JobStatus::Open,
            metadata: json!({}),
            ..Job::new(
                "backlog-item",
                Subject::new("custom", "s"),
                format!("nomination {n}"),
                "emp-david",
                Priority::Standard,
                opened,
            )
        };
        let at = t(&format!("{day}T00:00:00Z"));
        jobs.create_job_at(&job, at, &[]).await.unwrap();
        let mut s = Step::new(job.id, "task", format!("nomination {n}"), 0);
        s.id = StepId::new();
        s.spec_slug = Some("build".into());
        s.assignee_id = Some(who.to_string());
        s.status = StepStatus::Ready;
        jobs.add_step_at(&s, at, &[]).await.unwrap();
    }
}

/// A for=me answer cut at its limit SAYS it was cut (backlog 09074ada).
/// Until then `total` was counted after the cut, so three rows of five
/// answered `total: 3` — indistinguishable from a whole queue — and the
/// alias's two newest nominations simply were not there. `boss dispatch
/// --next` read that as the caller's whole inbox and reported "none
/// nominated" past 1000 rows. The total is now counted before the cut,
/// and `truncated` names a cut answer; a whole answer says false.
#[tokio::test]
async fn a_for_me_answer_cut_at_its_limit_says_it_was_cut() {
    let (app, jobs) = app(registry());
    seed_past_the_cut(&jobs).await;
    let who = || Some(user_header(AGENT, "platform-admin"));

    let (status, cut) = get(&app, "/api/jobs/assignments?for=me&limit=3", who()).await;
    assert_eq!(status, StatusCode::OK, "{cut}");
    assert_eq!(cut["data"].as_array().map(Vec::len), Some(3), "{cut}");
    assert_eq!(
        titles(&cut),
        ["nomination 0", "nomination 1", "nomination 2"],
        "the oldest three survive the cut: {cut}"
    );
    assert_eq!(cut["total"], 5, "total counted before the cut: {cut}");
    assert_eq!(cut["truncated"], true, "a cut answer says so: {cut}");

    // A cut on ONE spelling's own read is a cut too: the agent's id
    // holds three rows, so at limit=2 that read alone was full.
    let (_, one) = get(&app, "/api/jobs/assignments?for=me&limit=2", who()).await;
    assert_eq!(one["truncated"], true, "{one}");
    assert!(
        one["total"].as_u64().is_some_and(|t| t > 2),
        "a cut total is never the rows handed back: {one}"
    );

    // Controls: exactly at the limit and above it, the answer is whole.
    for limit in [5, 10] {
        let (_, whole) = get(
            &app,
            &format!("/api/jobs/assignments?for=me&limit={limit}"),
            who(),
        )
        .await;
        assert_eq!(whole["data"].as_array().map(Vec::len), Some(5), "{whole}");
        assert_eq!(whole["total"], 5, "{whole}");
        assert_eq!(whole["truncated"], false, "limit={limit}: {whole}");
    }
}
