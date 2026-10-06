//! `GET /api/jobs?…` returns each row WITH its steps embedded.
//!
//! WHY THIS FILE EXISTS (backlog d0ee20f8). This is a wire contract
//! live consumers depend on and nothing held. `infra/ops/ops-runner.sh`
//! lists its open `ops-request` packets once and reads the `execute`
//! step straight off each listed row —
//!
//!     ((.steps // []) | map(select(.spec_slug == "execute")) | .[0])
//!     // ((.steps // []) | map(select(.title == "execute")) | .[0])
//!
//! — and skips any packet where that selection is empty. Drop the
//! enrichment loop in `boss_jobs::http::jobs`' list handler and the
//! runner does not error: it logs "has no execute step — skipping" for
//! every request and the ops queue silently stops moving. A list read
//! that quietly stopped carrying steps is what this file refuses.
//!
//! IT IS ALSO THE REFUTATION OF A RECURRING SHORTCUT. The reflex behind
//! the duplicate exit that backlog 50fede8b collapsed was "write the
//! fact at the request level so a reader need not fetch steps". A
//! request-level reader never had to fetch them: the list has carried
//! them all along. That argument is only usable by the next author if
//! it is written where they look and held by something that fails —
//! which is this test, named from `boss-dispatcher-handlers`' crate doc
//! and from the enrichment site itself.
//!
//! A STEP'S METADATA IS ASKED FOR BY NAME (backlog 9b473d4a, design
//! 3036296f mechanism D, 2026-09-27). Measured that day, 200 open
//! backlog-item rows weighed 3.6 MB, and 2.2 MB of it was the steps'
//! `metadata` and `fields` — a procedure's prose copied onto every step
//! — which truncated a reader twice. `full=false` serves a row's steps
//! SLIM: every step, with its identity, slug, status, holder and
//! stamps, and without `metadata` or `fields`. `full=true` serves them
//! whole, and every reader that reads a listed step's metadata asks for
//! it — the ops-runner first among them, whose query below is the one
//! it sends. Since backlog ea80b5fd an unflagged read is SLIM. A slim
//! step has no metadata key at all, never an empty one that reads as
//! "the step recorded nothing"; it carries `slim: true`, which a typed
//! `Step` parse refuses; and the envelope says which shape it served
//! (`full`).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::job::{
    Job, JobId, JobStatus, Priority, SLIM_STEP_MARKER, Step, StepId, StepStatus, Subject,
};
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

const FIRST: &str = "00000000-0000-0000-0000-00000000f001";
const SECOND: &str = "00000000-0000-0000-0000-00000000f002";
const ALERT: &str = "00000000-0000-0000-0000-00000000a1e7";
const FAILED_LINE: &str = "publish-drift: FAILED — the mirror refused the push";

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
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

fn request(id: &str, verb: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).expect("uuid")),
        status: JobStatus::Open,
        metadata: serde_json::json!({ "verb": verb, "host": "forge" }),
        ..Job::new(
            "ops-request",
            Subject::new("asset", "BOSSNET"),
            format!("{verb} on forge"),
            "emp-david",
            Priority::Standard,
            day(2026, 9, 20),
        )
    }
}

/// The execute step as the ops-runner meets it: addressed by
/// `spec_slug`, gated on `status`, and carrying the verb's exit under
/// `exit_code` — the one spelling, on the step (backlog 50fede8b).
fn execute_step(job: &str, exit_code: &str) -> Step {
    Step {
        id: StepId::new(),
        spec_slug: Some("execute".into()),
        status: StepStatus::Ready,
        metadata: serde_json::json!({ "exit_code": exit_code }),
        ..Step::new(
            JobId::from_uuid(Uuid::parse_str(job).expect("uuid")),
            "task",
            "execute",
            1,
        )
    }
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
        now: day(2026, 9, 20)
            .and_hms_opt(12, 0, 0)
            .expect("noon")
            .and_utc(),
        simulated: true,
        epoch_start: None,
        epoch_end: None,
        paused: false,
        restart_in_progress: false,
        warp_factor: None,
    }));
    for (id, verb, exit) in [(FIRST, "converge", "0"), (SECOND, "publish-drift", "75")] {
        jobs.create_job(&request(id, verb)).await.expect("job");
        let mut step = execute_step(id, exit);
        if id == SECOND {
            // The note jobs.complete_linked_step writes on a step whose
            // verb FAILED, beside keys that are not part of it.
            step.metadata = serde_json::json!({
                "exit_code": exit,
                "output": "a long verb output the list should not carry",
                "failed": FAILED_LINE,
                "failed_exit": "75",
                "failed_source": FIRST,
                "alert": ALERT,
            });
        }
        jobs.add_step(&step).await.expect("step");
    }
    let state = JobsApiState::minimal(jobs, bus, publisher, policy, clock);
    router(state)
}

async fn list(app: &Router, query: &str) -> serde_json::Value {
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
    let body = String::from_utf8_lossy(&body).into_owned();
    assert_eq!(status, StatusCode::OK, "GET /api/jobs?{query}: {body}");
    serde_json::from_str(&body).expect("json")
}

/// The ops-runner's own selection, in Rust: slug first, title as
/// fallback, off the LISTED row — no second fetch anywhere.
fn execute_of(row: &serde_json::Value) -> &serde_json::Value {
    let steps = row["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("listed row carries a steps array: {row}"));
    steps
        .iter()
        .find(|s| s["spec_slug"] == "execute")
        .or_else(|| steps.iter().find(|s| s["title"] == "execute"))
        .unwrap_or_else(|| panic!("listed row carries its execute step: {row}"))
}

#[tokio::test]
async fn a_listed_packet_carries_its_steps() {
    let app = seed().await;
    // The ops-runner's own read asks for the whole step (9b473d4a).
    let body = list(&app, "kind=ops-request&full=true").await;
    assert_eq!(body["full"], true, "the envelope names the shape: {body}");
    let rows = body["data"].as_array().expect("data array");
    assert_eq!(rows.len(), 2, "both requests listed: {body}");

    for row in rows {
        let step = execute_of(row);
        // The fields the runner gates on before it acts.
        assert_eq!(step["status"], "ready", "step status rides the list: {row}");
        assert_eq!(step["job_id"], row["id"], "each row carries ITS OWN steps");
        assert!(
            step["metadata"]["exit_code"].is_string(),
            "step metadata rides the list: {row}"
        );
    }

    // Per-row, not one row's steps stamped on every row: the two
    // requests must answer with their own exits.
    let exits: Vec<&str> = rows
        .iter()
        .map(|r| {
            execute_of(r)["metadata"]["exit_code"]
                .as_str()
                .unwrap_or("")
        })
        .collect();
    assert!(
        exits.contains(&"0") && exits.contains(&"75"),
        "each listed row carries its own step metadata, got {exits:?}"
    );
}

/// The slim shape (9b473d4a): every step still rides, addressable and
/// gateable exactly as the runner selects it, and neither of the two
/// heavy keys does. The job's own metadata is untouched.
#[tokio::test]
async fn a_listed_step_carries_no_metadata_when_asked_slim() {
    let app = seed().await;
    let query = "kind=ops-request&full=false";
    let body = list(&app, query).await;
    assert_eq!(body["full"], false, "the envelope names the shape: {body}");
    let rows = body["data"].as_array().expect("data array");
    assert_eq!(rows.len(), 2, "both requests listed: {body}");
    for row in rows {
        let step = execute_of(row);
        assert_eq!(step["status"], "ready", "{row}");
        assert_eq!(step["job_id"], row["id"], "{row}");
        assert_eq!(step["title"], "execute", "{row}");
        assert!(step["id"].is_string(), "{row}");
        for heavy in ["metadata", "fields"] {
            assert!(
                step.get(heavy).is_none(),
                "a slim step carries no `{heavy}` key at all: {row}"
            );
        }
        assert!(
            row["metadata"]["verb"].is_string(),
            "the JOB's metadata still rides: {row}"
        );
        // The shape is marked on the step itself, so a TYPED reader
        // that forgot `full=true` fails instead of reading a step that
        // recorded nothing (backlog ea80b5fd).
        assert_eq!(step[SLIM_STEP_MARKER], true, "{row}");
        let refused = serde_json::from_value::<Step>(step.clone());
        assert!(
            refused.is_err(),
            "a slim listed step must not parse as a Step: {row}"
        );
    }
}

/// The contract, flipped (backlog ea80b5fd, 2026-09-27): an unflagged
/// read serves SLIM steps and its envelope says so. Every reader of a
/// listed step's metadata asks `full=true` by name — the sweep that
/// moved them landed as train #740 and every deployed copy converged
/// past it before this flipped — so a reader that does not ask reads
/// only what a board selects and gates on.
#[tokio::test]
async fn an_unflagged_list_serves_the_shape_its_envelope_names() {
    let app = seed().await;
    let body = list(&app, "kind=ops-request").await;
    let full = body["full"]
        .as_bool()
        .unwrap_or_else(|| panic!("the envelope names its shape: {body}"));
    assert!(!full, "an unflagged read is slim: {body}");
    for row in body["data"].as_array().expect("data array") {
        let step = execute_of(row);
        assert!(step.get("metadata").is_none(), "{row}");
        assert_eq!(step[SLIM_STEP_MARKER], true, "{row}");
    }
}

/// A whole step carries no slim marker, and parses as a Step.
#[tokio::test]
async fn a_whole_listed_step_carries_no_slim_marker() {
    let app = seed().await;
    let body = list(&app, "kind=ops-request&full=true").await;
    for row in body["data"].as_array().expect("data array") {
        let step = execute_of(row);
        assert!(step.get(SLIM_STEP_MARKER).is_none(), "{row}");
        serde_json::from_value::<Step>(step.clone())
            .unwrap_or_else(|e| panic!("a whole listed step parses: {e}: {row}"));
    }
}

/// THE FAILED-VERB NOTE IS A SERVER READING (backlog ea80b5fd). The
/// receiving yard drew a step whose verb FAILED as troubled by reading
/// the note off the listed step's metadata, so it had to ask the whole
/// list for four keys. `failed_verbs=true` puts exactly those keys on
/// each listed step that carries the note, as `failed_verb`, and on a
/// slim read — the rest of the metadata stays home.
#[tokio::test]
async fn a_slim_listed_step_carries_its_failed_verb_when_asked() {
    let app = seed().await;
    let body = list(&app, "kind=ops-request&failed_verbs=true").await;
    assert_eq!(body["full"], false, "{body}");
    let rows = body["data"].as_array().expect("data array");
    let failed = rows
        .iter()
        .find(|r| r["id"] == SECOND)
        .map(execute_of)
        .expect("the failed request is listed");
    assert_eq!(
        failed["failed_verb"],
        serde_json::json!({
            "failed": FAILED_LINE,
            "failed_exit": "75",
            "failed_source": FIRST,
            "alert": ALERT,
        }),
        "the note's four keys and nothing else: {failed}"
    );
    let clean = rows
        .iter()
        .find(|r| r["id"] == FIRST)
        .map(execute_of)
        .expect("the clean request is listed");
    assert!(clean.get("failed_verb").is_none(), "{clean}");

    // Opt-in, like `lane` and `origin`: nobody meets it unasked.
    let unasked = list(&app, "kind=ops-request").await;
    for row in unasked["data"].as_array().expect("data array") {
        assert!(execute_of(row).get("failed_verb").is_none(), "{row}");
    }
}

/// The knowledge half of backlog d0ee20f8: the fact above is only
/// reachable by the next author if the places they read NAME this file.
/// Two do — the enrichment site and the handler crate's doc — and this
/// test is what stops either reference going quietly dead.
#[test]
fn the_contract_is_named_where_an_author_would_look() {
    let root = boss_testing::repo_root();
    for path in [
        "crates/core/boss-jobs/src/http/jobs.rs",
        "crates/orchestrators/boss-dispatcher-handlers/src/lib.rs",
    ] {
        let text =
            std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"));
        assert!(
            text.contains("a_listed_packet_carries_its_steps"),
            "{path} must name the test that holds the list-carries-steps contract"
        );
    }
}
