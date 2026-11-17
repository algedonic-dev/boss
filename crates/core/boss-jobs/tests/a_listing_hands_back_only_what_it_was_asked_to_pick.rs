//! `GET /api/jobs?pick=…&step_pick=…` answers a measurement in one read:
//! the keys named, and none of the bodies around them.
//!
//! WHY THIS FILE EXISTS (backlog 1b5a14fe, found by the factory-speed
//! research session of 2026-10-15). Measuring a month of the factory
//! cost thousands of detail reads or hundreds of MB. A gate-run's
//! per-check timings live inside its verdict step's `receipt` — a JSON
//! document stored as a STRING, copied verbatim from the runner (69 KB
//! on the live gate read 2026-10-18, 7.6 KB of it the `checks`) — and a
//! listed step is slim, so a month of timings was one `GET
//! /api/jobs/{id}` per gate (~3,500). An agent-run carries its `brief`,
//! `report` and `claim_receipt` in its own metadata (61 KB on the run
//! read that day, 52 KB of it the brief), so 1,000 runs were 96 MB.
//!
//! `pick=<paths>` replaces a row's `metadata` with `picked`: only the
//! named paths, keyed by the path as asked. `step_pick=<paths>` puts the
//! same on every listed step, slim or whole. A path is dot-separated; at
//! each object the LONGEST key that matches is taken first, so a
//! literal dotted key (`launch.model` is one, on agent-run steps) is
//! reachable, and a segment that meets a string holding a JSON object
//! descends into it — which is how `receipt.checks` reaches the timings
//! without the receipt. A path that resolves to nothing is ABSENT from
//! `picked`, never `null`: null is a value a packet can record. The
//! row's `metadata` key is removed rather than narrowed, because a
//! narrowed `metadata` reads as a packet that recorded nothing else.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

const GATE: &str = "00000000-0000-0000-0000-0000000067a7";
const BRIEF: &str = "THE WHOLE BRIEF, which a throughput read must not carry";
const EVIDENCE: &str = "RUNTIME EVIDENCE, the bulk of the receipt";

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 18).expect("valid date")
}

fn ceo() -> User {
    User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

/// The receipt as the runner writes it: a JSON document in a string.
fn receipt() -> String {
    serde_json::json!({
        "verdict": "green",
        "checks": [
            {"name": "fixture", "result": "pass", "seconds": 33},
            {"name": "clippy", "result": "pass", "seconds": 87},
        ],
        "runtime_evidence": {"log": EVIDENCE},
    })
    .to_string()
}

async fn seed() -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let clock: Arc<dyn ClockClient> = Arc::new(FixedClockClient::new(ClockNow {
        now: day().and_hms_opt(12, 0, 0).expect("noon").and_utc(),
        simulated: true,
        epoch_start: None,
        epoch_end: None,
        paused: false,
        restart_in_progress: false,
        warp_factor: None,
    }));
    let id = JobId::from_uuid(Uuid::parse_str(GATE).expect("uuid"));
    jobs.create_job(&Job {
        id,
        status: JobStatus::Closed,
        metadata: serde_json::json!({
            "outcome": "green",
            "closed_at": "2026-10-18T12:00:00Z",
            "recorded": null,
            "brief": BRIEF,
        }),
        ..Job::new(
            "gate-run",
            Subject::new("asset", "BOSSNET"),
            "gate feat/x",
            "emp-david",
            Priority::Standard,
            day(),
        )
    })
    .await
    .expect("job");
    let verdict = Step {
        id: StepId::new(),
        spec_slug: Some("verdict".into()),
        status: StepStatus::Completed,
        metadata: serde_json::json!({
            "verdict": "green",
            "receipt": receipt(),
            "launch.model": "opus",
        }),
        ..Step::new(id, "gate-verdict", "Record the receipt", 1)
    };
    jobs.add_step(&verdict).await.expect("step");
    let outcome = Step {
        id: StepId::new(),
        spec_slug: Some("green".into()),
        status: StepStatus::Completed,
        metadata: serde_json::json!({ "outcome_kind": "success" }),
        ..Step::new(id, "outcome", "Gate green", 2)
    };
    jobs.add_step(&outcome).await.expect("step");
    let state = JobsApiState::minimal(jobs, bus, publisher, policy, clock);
    router(state)
}

async fn get(app: &Router, query: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/jobs?{query}"))
                .header("x-boss-user", serde_json::to_string(&ceo()).expect("user"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn list(app: &Router, query: &str) -> (serde_json::Value, String) {
    let (status, body) = get(app, query).await;
    assert_eq!(status, StatusCode::OK, "GET /api/jobs?{query}: {body}");
    (serde_json::from_str(&body).expect("json"), body)
}

fn step<'a>(row: &'a serde_json::Value, slug: &str) -> &'a serde_json::Value {
    row["steps"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["spec_slug"] == slug))
        .unwrap_or_else(|| panic!("listed row carries its {slug} step: {row}"))
}

/// The factory's throughput read: verdicts and per-check seconds, with
/// neither the brief nor the receipt's bulk on the wire.
#[tokio::test]
async fn a_month_of_gate_timings_is_one_read_without_the_bodies() {
    let app = seed().await;
    let (body, raw) = list(
        &app,
        "kind=gate-run&pick=outcome,closed_at&step_pick=verdict,receipt.checks",
    )
    .await;
    let row = &body["data"][0];
    assert!(
        row.get("metadata").is_none(),
        "a picked row carries no narrowed metadata: {row}"
    );
    assert_eq!(
        row["picked"],
        serde_json::json!({"outcome": "green", "closed_at": "2026-10-18T12:00:00Z"}),
        "{row}"
    );
    let verdict = step(row, "verdict");
    assert_eq!(verdict["picked"]["verdict"], "green", "{verdict}");
    assert_eq!(
        verdict["picked"]["receipt.checks"][1],
        serde_json::json!({"name": "clippy", "result": "pass", "seconds": 87}),
        "the receipt's checks, reached through the string it is stored in: {verdict}"
    );
    assert!(verdict.get("metadata").is_none(), "still slim: {verdict}");
    for body_text in [BRIEF, EVIDENCE] {
        assert!(
            !raw.contains(body_text),
            "the picked read carries no `{body_text}`: {raw}"
        );
    }
    assert_eq!(
        body["pick"],
        serde_json::json!(["outcome", "closed_at"]),
        "the envelope names what it picked: {body}"
    );
    assert_eq!(
        body["step_pick"],
        serde_json::json!(["verdict", "receipt.checks"]),
        "{body}"
    );
}

/// A step that holds none of the paths answers an empty `picked`; a path
/// nothing resolves is absent rather than null; a recorded null is kept.
#[tokio::test]
async fn an_absent_path_is_absent_and_a_recorded_null_is_kept() {
    let app = seed().await;
    let (body, _) = list(
        &app,
        "kind=gate-run&pick=recorded,nowhere&step_pick=verdict",
    )
    .await;
    let row = &body["data"][0];
    assert_eq!(
        row["picked"],
        serde_json::json!({"recorded": null}),
        "{row}"
    );
    assert_eq!(step(row, "green")["picked"], serde_json::json!({}), "{row}");
}

/// A literal dotted key wins over a walk, so `launch.model` reads the
/// key that is spelled that way.
#[tokio::test]
async fn a_literal_dotted_key_is_reachable() {
    let app = seed().await;
    let (body, _) = list(&app, "kind=gate-run&step_pick=launch.model").await;
    let verdict = step(&body["data"][0], "verdict");
    assert_eq!(verdict["picked"]["launch.model"], "opus", "{verdict}");
}

/// No pick, no change: the row keeps its metadata and nothing grows a
/// `picked` key or an envelope echo it did not ask for.
#[tokio::test]
async fn an_unpicked_read_is_unchanged() {
    let app = seed().await;
    let (body, _) = list(&app, "kind=gate-run").await;
    let row = &body["data"][0];
    assert_eq!(row["metadata"]["brief"], BRIEF, "{row}");
    assert!(row.get("picked").is_none(), "{row}");
    assert!(step(row, "verdict").get("picked").is_none(), "{row}");
    assert!(body.get("pick").is_none(), "{body}");
    assert!(body.get("step_pick").is_none(), "{body}");
}

/// A pick the server cannot read is a 400 naming the parameter — never
/// an answer that silently picked nothing.
#[tokio::test]
async fn an_unreadable_pick_is_refused_by_name() {
    let app = seed().await;
    let many = (0..33)
        .map(|i| format!("k{i}"))
        .collect::<Vec<_>>()
        .join(",");
    // One path of nine segments, and one of 8 KB: reviews 6fd5ad34 and
    // 92043bd0 held a worker for hours with a long step_pick path.
    let deep = ["a"; 9].join(".");
    let long = vec!["a"; 4_000].join(".");
    for (param, value) in [
        ("pick", ""),
        ("pick", "outcome,,closed_at"),
        ("step_pick", "receipt..checks"),
        ("step_pick", ".verdict"),
        ("pick", many.as_str()),
        ("step_pick", deep.as_str()),
        ("step_pick", long.as_str()),
        ("pick", long.as_str()),
    ] {
        let (status, body) = get(&app, &format!("kind=gate-run&{param}={value}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{param}={value}: {body}");
        assert!(body.starts_with(param), "the 400 names `{param}`: {body}");
    }
}
